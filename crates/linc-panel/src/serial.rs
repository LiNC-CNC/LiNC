//! Physical CNC control panel driver (RS-485, 115200 8N1).
//!
//! The driver owns exactly one serial port for its process lifetime. It speaks
//! the existing MODBUS-like panel protocol, decodes steady modes and override
//! levels, and never opens a second link.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use linc_core::model::{PanelCapabilities, PanelLinkStats, PanelModeValidity, PanelSnapshot};
use serialport::{DataBits, Parity, SerialPort, StopBits};

use crate::{
    base_capabilities, buttons, decode_feed_override, decode_mode_field, decode_rapid_override,
    decode_spindle_override, extract_button_state, led_state_to_bytes, make_snapshot, modbus_crc16,
    PanelDisplay, PanelDriver, PanelError, PanelOutputs,
};

/// Record one frame on the panel diagnostic ring, if one is attached.
///
/// The ring is the only place raw bytes are kept: they are never logged, because
/// a 20 Hz link would bury the log and the bytes are only useful next to the
/// decoded state.
fn record_frame(
    tap: &Option<Arc<Mutex<linc_diag::PanelBus>>>,
    direction: &str,
    bytes: &[u8],
    decoded: Option<String>,
    crc_ok: Option<bool>,
    error: Option<String>,
) {
    let Some(bus) = tap else {
        return;
    };
    if let Ok(mut bus) = bus.lock() {
        bus.push_frame(direction, bytes, decoded, crc_ok, error);
    }
}

pub struct CncPanelDriver {
    port: Option<Box<dyn SerialPort>>,
    path: String,
    baud: u32,
    cycle_time: Duration,
    caps: PanelCapabilities,
    last_display: PanelDisplay,
    last_poll: Instant,
    last_rx: Option<Instant>,
    last_reconnect: Instant,
    stats: PanelLinkStats,
    outputs: PanelOutputs,
    connected: bool,
    error: bool,
    status_transport_warned: bool,
    frames: Option<Arc<Mutex<linc_diag::PanelBus>>>,
}

impl CncPanelDriver {
    pub fn new(path: impl Into<String>, baud: u32, cycle_time: Duration) -> Self {
        let path = path.into();
        let caps = base_capabilities("rs485");
        let stats = PanelLinkStats {
            port: path.clone(),
            baud,
            cycle_time_ms: cycle_time.as_secs_f64() * 1000.0,
            available: true,
            ..Default::default()
        };
        Self {
            port: None,
            path,
            baud,
            cycle_time: cycle_time.max(Duration::from_millis(10)),
            caps,
            last_display: PanelDisplay {
                leds: 0,
                sseg_tool: 0,
                sseg_next_tool: 0,
                sseg_status: -1,
                vel: [0.0; 3],
                homing: false,
            },
            last_poll: Instant::now(),
            last_rx: None,
            last_reconnect: Instant::now() - Duration::from_secs(10),
            stats,
            outputs: PanelOutputs::default(),
            connected: false,
            error: false,
            status_transport_warned: false,
            frames: None,
        }
    }

    /// Tap every request, reply and write onto the panel diagnostic ring.
    pub fn set_frame_tap(&mut self, bus: Arc<Mutex<linc_diag::PanelBus>>) {
        self.frames = Some(bus);
    }

    fn open(&mut self) -> Result<(), PanelError> {
        if self.last_reconnect.elapsed() < Duration::from_secs(1) {
            return Err(PanelError::Transport("reconnect throttled".into()));
        }
        self.last_reconnect = Instant::now();
        let port = serialport::new(&self.path, self.baud)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .timeout(Duration::from_millis(250))
            .open()
            .map_err(|error| PanelError::Transport(error.to_string()))?;
        self.port = Some(port);
        self.stats.reconnects += 1;
        log::info!("panel link open on {} at {}", self.path, self.baud);
        Ok(())
    }

    fn communicate(&mut self) -> Result<[u8; 12], PanelError> {
        let tap = self.frames.clone();
        let Some(port) = self.port.as_mut() else {
            return Err(PanelError::Transport("not open".into()));
        };
        let started = Instant::now();
        let request = [0xEB, 0x01, 0x80, 0x8E];
        record_frame(&tap, "tx", &request, Some("poll".into()), None, None);
        port.write_all(&request)?;
        port.flush()?;
        std::thread::sleep(Duration::from_micros(4000));

        let mut response = [0u8; 12];
        if let Err(error) = port.read_exact(&mut response) {
            // No bytes were returned, so the record carries the error and an
            // empty payload rather than an invented reply.
            record_frame(&tap, "rx", &[], None, None, Some(error.to_string()));
            return Err(error.into());
        }
        self.stats.rx_bytes += response.len() as u64;
        self.stats.last_rx_age_ms = Some(0);
        self.last_rx = Some(Instant::now());
        let calculated = modbus_crc16(&response[..10]);
        let crc_ok =
            response[10..12] == calculated || response[10..12] == [calculated[1], calculated[0]];
        record_frame(
            &tap,
            "rx",
            &response,
            None,
            Some(crc_ok),
            if crc_ok {
                None
            } else {
                Some("CRC mismatch".into())
            },
        );
        if !crc_ok {
            self.stats.crc_errors += 1;
            return Err(PanelError::Protocol("CRC mismatch".into()));
        }

        // Prepare the LED/7-segment frame from the latest HAL inputs.
        let mut led_bytes = led_state_to_bytes(self.last_display.leds);
        led_bytes[8] = crate::dec_to_bcd_hex(self.last_display.sseg_tool);
        led_bytes[9] = crate::dec_to_bcd_hex(self.last_display.sseg_next_tool);
        let mut frame = Vec::with_capacity(14);
        frame.extend_from_slice(&[0xEB, 0x02]);
        frame.extend_from_slice(&led_bytes);
        let crc = modbus_crc16(&frame);
        frame.push(crc[1]);
        frame.push(crc[0]);
        std::thread::sleep(Duration::from_micros(5000));
        record_frame(&tap, "tx", &frame, Some("led frame".into()), Some(true), None);
        port.write_all(&frame)?;
        self.stats.tx_bytes += frame.len() as u64;
        std::thread::sleep(Duration::from_micros(5000));
        let commit = [0xEB, 0x02, 0x81, 0xCE];
        record_frame(&tap, "tx", &commit, Some("led commit".into()), None, None);
        port.write_all(&commit)?;
        self.stats.tx_bytes += 4;
        port.flush()?;

        if self.last_display.sseg_status >= 0 && !self.status_transport_warned {
            // The current protocol carries two tool displays only. The panel
            // service still exposes cnc_panel.sseg_status for the simulator and
            // future panel revisions.
            log::warn!(
                "status display {} is available on the HAL pin but the legacy \
                 panel protocol has no third display frame; transport extension pending",
                self.last_display.sseg_status
            );
            self.status_transport_warned = true;
        }
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        self.stats.cycle_time_ms = elapsed;
        Ok(response)
    }

    fn build_outputs(&mut self, raw: u64) -> PanelOutputs {
        let mode_field = ((raw >> 18) & 0b111) as u8;
        let mode = decode_mode_field(mode_field);
        let pressed = !raw;
        let home_active = matches!(
            mode,
            PanelModeValidity::Valid(linc_core::model::PanelMode::Home)
        );
        let proghome_active = matches!(
            mode,
            PanelModeValidity::Valid(linc_core::model::PanelMode::ProgHome)
        );
        let rapid = pressed & buttons::RAPID != 0;
        let x_direction = pressed & (buttons::JOG_XP | buttons::JOG_XN) != 0;
        let y_direction = pressed & (buttons::JOG_YP | buttons::JOG_YN) != 0;
        let z_direction = pressed & (buttons::JOG_ZP | buttons::JOG_ZN) != 0;

        let feed = decode_feed_override(((raw >> 8) & 0xF) as u8);
        let rapid_override = decode_rapid_override(((raw >> 12) & 0b11) as u8);
        let spindle = decode_spindle_override(((raw >> 21) & 0b111) as u8);

        let mut outputs = self.outputs.clone();
        let mut visible_buttons = pressed;
        // HOME/PROG.HOME keys drive the home/proghome pins, not jog.
        if home_active || proghome_active {
            visible_buttons &= !(buttons::RAPID
                | buttons::JOG_XP
                | buttons::JOG_XN
                | buttons::JOG_YP
                | buttons::JOG_YN
                | buttons::JOG_ZP
                | buttons::JOG_ZN
                | buttons::JOG_AP
                | buttons::JOG_AN
                | buttons::JOG_BP
                | buttons::JOG_BN);
        }
        let auto_active = matches!(
            mode,
            PanelModeValidity::Valid(linc_core::model::PanelMode::Auto)
        );
        let single_block = self.last_display.leds & crate::leds::SBK != 0;
        let step_active = auto_active && single_block && pressed & buttons::CYCLE_START != 0;
        if step_active {
            visible_buttons &= !buttons::CYCLE_START;
        }
        outputs.buttons = visible_buttons;
        outputs.mode = mode;
        outputs.feed_override = feed;
        outputs.rapid_override = rapid_override;
        outputs.spindle_override = spindle;
        outputs.program_protect = pressed & buttons::PROGRAM_PROTECT != 0;
        outputs.estop = pressed & buttons::ESTOP != 0;
        let individual_home_inhibited = self.last_display.homing;
        outputs.home = [
            home_active && rapid,
            home_active && x_direction && !individual_home_inhibited,
            home_active && y_direction && !individual_home_inhibited,
            home_active && z_direction && !individual_home_inhibited,
        ];
        outputs.proghome = [
            proghome_active && rapid,
            proghome_active && x_direction && !individual_home_inhibited,
            proghome_active && y_direction && !individual_home_inhibited,
            proghome_active && z_direction && !individual_home_inhibited,
        ];
        outputs.step = step_active;
        outputs.display = self.last_display.clone();
        outputs
    }

    pub fn set_display(&mut self, display: PanelDisplay) {
        self.last_display = display;
    }
}

impl PanelDriver for CncPanelDriver {
    fn poll(&mut self) -> Result<PanelSnapshot, PanelError> {
        // Poll cadence is owned by the caller; keep the port open.
        let since_poll = self.last_poll.elapsed();
        if since_poll < self.cycle_time {
            return Ok(make_snapshot(
                self.connected,
                self.error,
                &self.outputs,
                self.caps.clone(),
                self.stats.clone(),
            ));
        }
        self.last_poll = Instant::now();

        if self.port.is_none() {
            if let Err(error) = self.open() {
                self.connected = false;
                self.error = true;
                self.stats.last_rx_age_ms =
                    self.last_rx.map(|last| last.elapsed().as_millis() as u64);
                log::warn!("panel link unavailable: {error}");
                return Ok(make_snapshot(
                    false,
                    true,
                    &self.outputs,
                    self.caps.clone(),
                    self.stats.clone(),
                ));
            }
        }

        match self.communicate() {
            Ok(response) => {
                let raw = extract_button_state(&response);
                self.outputs = self.build_outputs(raw);
                self.connected = true;
                self.error = false;
                self.stats.last_rx_age_ms = Some(0);
                Ok(make_snapshot(
                    true,
                    false,
                    &self.outputs,
                    self.caps.clone(),
                    self.stats.clone(),
                ))
            }
            Err(error) => {
                self.stats.timeouts += 1;
                if !matches!(error, PanelError::Protocol(_)) {
                    log::warn!("panel communication failed: {error}");
                }
                self.connected = false;
                self.error = true;
                self.port = None;
                self.stats.last_rx_age_ms =
                    self.last_rx.map(|last| last.elapsed().as_millis() as u64);
                Ok(make_snapshot(
                    false,
                    true,
                    &self.outputs,
                    self.caps.clone(),
                    self.stats.clone(),
                ))
            }
        }
    }

    fn write(&mut self, outputs: &PanelOutputs) -> Result<(), PanelError> {
        // Button/home/override values are sampled from the hardware; only the
        // display side is driven by HAL.
        self.last_display = outputs.display.clone();
        self.outputs.display = outputs.display.clone();
        Ok(())
    }

    fn capabilities(&self) -> PanelCapabilities {
        self.caps.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_starts_disconnected_without_a_port() {
        let mut driver = CncPanelDriver::new(
            "/dev/definitely-not-a-panel",
            115200,
            Duration::from_millis(20),
        );
        let snapshot = driver.poll().unwrap();
        assert!(!snapshot.connected);
        assert_eq!(snapshot.mode, PanelModeValidity::Unknown);
        assert_eq!(driver.capabilities().buttons, 46);
    }

    #[test]
    fn active_low_buttons_become_pressed_masks() {
        let mut driver = CncPanelDriver::new("/dev/null", 115200, Duration::from_millis(20));
        let mut raw = !(buttons::CYCLE_START | buttons::JOG_XP | buttons::ESTOP);
        raw &= !(0b111 << 18);
        raw |= 1 << 18; // AUTO
        let outputs = driver.build_outputs(raw);
        assert!(outputs.button(buttons::CYCLE_START));
        assert!(outputs.button(buttons::JOG_XP));
        assert!(outputs.estop);
        assert!(!outputs.button(buttons::CYCLE_STOP));
    }

    #[test]
    fn individual_home_outputs_are_inhibited_while_homing() {
        let mut driver = CncPanelDriver::new("/dev/null", 115200, Duration::from_millis(20));
        driver.set_display(PanelDisplay {
            homing: true,
            ..PanelDisplay::default()
        });
        let mut raw = !(buttons::RAPID | buttons::JOG_XP);
        raw &= !(0b111 << 18);
        raw |= 6 << 18; // HOME
        let outputs = driver.build_outputs(raw);
        assert_eq!(outputs.home, [true, false, false, false]);
    }

    #[test]
    fn home_mapping_is_gated_by_home_mode() {
        let mut driver = CncPanelDriver::new("/dev/null", 115200, Duration::from_millis(20));
        // mode field 6 = HOME, RAPID + JOG_XP pressed.
        let mut raw = !(buttons::RAPID | buttons::JOG_XP);
        raw &= !(0b111 << 18);
        raw |= 6 << 18;
        let outputs = driver.build_outputs(raw);
        assert_eq!(
            outputs.mode,
            PanelModeValidity::Valid(linc_core::model::PanelMode::Home)
        );
        assert_eq!(outputs.home, [true, true, false, false]);
        assert!(outputs.proghome.iter().all(|value| !value));
    }

    #[test]
    fn the_frame_tap_keeps_the_bytes_and_their_crc_verdict() {
        let bus = Arc::new(Mutex::new(linc_diag::PanelBus::new()));
        let tap = Some(bus.clone());
        record_frame(&tap, "tx", &[0xeb, 0x01, 0x80, 0x8e], Some("poll".into()), None, None);
        record_frame(&tap, "rx", &[0xeb, 0x01, 0xaa], None, Some(false), Some("CRC mismatch".into()));

        let frames = bus.lock().unwrap().frames(10);        let frames = frames["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1]["direction"], "tx", "newest first");
        assert_eq!(frames[1]["hex"], "eb01808e");
        assert_eq!(frames[1]["decoded"], "poll");
        assert_eq!(frames[0]["crc_ok"], false);
        assert_eq!(frames[0]["error"], "CRC mismatch");
    }
}
