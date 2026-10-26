//! LiNC panel service: driver contract, physical CNC panel protocol and the
//! simulation driver.
//!
//! The service owns the panel link and the `cnc_panel.*` HAL surface. The egui
//! HMI never opens the serial port and never writes these pins.

pub mod hal;
pub mod serial;
pub mod sim;

use linc_core::model::{
    now_ms, JogState, PanelCapabilities, PanelLinkStats, PanelMode, PanelModeValidity,
    PanelSnapshot, PendantState,
};

pub use hal::CncPanelHal;
pub use serial::CncPanelDriver;
pub use sim::{PanelFace, SimPanelDriver, SimPanelState};

#[derive(Debug, thiserror::Error)]
pub enum PanelError {
    #[error("panel transport unavailable: {0}")]
    Transport(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("panel link timed out")]
    Timeout,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Bit masks for the 58 CNC control panel buttons. Values follow the existing
/// panel protocol and are active-low on the wire. The first 46 are the mill
/// panel's; the rest are the lathe-only keys of the lathe face.
pub mod buttons {
    pub const PROGRAM_PROTECT: u64 = 1 << 0;
    pub const ESTOP: u64 = 1 << 7;
    pub const CYCLE_START: u64 = 1 << 14;
    pub const CYCLE_STOP: u64 = 1 << 15;
    pub const RAPID: u64 = 1 << 63;
    pub const MLK: u64 = 1 << 55;
    pub const FLUSH: u64 = 1 << 47;
    pub const SPD_SP: u64 = 1 << 39;
    pub const K2: u64 = 1 << 34;
    pub const JOG_XN: u64 = 1 << 62;
    pub const BDT: u64 = 1 << 54;
    pub const SPD_CTS: u64 = 1 << 46;
    pub const SPD_CW: u64 = 1 << 38;
    pub const ARM_MAN: u64 = 1 << 30;
    pub const JOG_YP: u64 = 1 << 61;
    pub const SBK: u64 = 1 << 53;
    pub const COOLANT: u64 = 1 << 45;
    pub const MAG_CCW: u64 = 1 << 37;
    pub const POT_DN: u64 = 1 << 29;
    pub const JOG_ZP: u64 = 1 << 60;
    pub const MPG_SIM: u64 = 1 << 52;
    pub const PT_LA: u64 = 1 << 44;
    pub const MAG_CW: u64 = 1 << 36;
    pub const CHUCK: u64 = 1 << 28;
    pub const JOG_AN: u64 = 1 << 59;
    pub const JOG_AP: u64 = 1 << 51;
    pub const AIR_BLOW: u64 = 1 << 43;
    pub const SPD_ORI: u64 = 1 << 35;
    pub const CHIP_CCW: u64 = 1 << 27;
    pub const JOG_BP: u64 = 1 << 58;
    pub const JOG_ZN: u64 = 1 << 50;
    pub const WORKLAMP: u64 = 1 << 42;
    /// Hardware-verified on the machine: the panel reports K1 and K2 on the
    /// same bit, so pressing either raises both pins. Bit 32 belongs to LUB.
    /// This must stay in step with `linc-linuxcnc/src/panel.rs`.
    pub const K1: u64 = 1 << 34;
    pub const CHIP_SP: u64 = 1 << 26;
    pub const SAFE_DOOR: u64 = 1 << 57;
    pub const JOG_YN: u64 = 1 << 49;
    pub const DRN: u64 = 1 << 41;
    pub const CORRIDOR: u64 = 1 << 33;
    pub const CHIP_CW: u64 = 1 << 25;
    pub const K4: u64 = 1 << 17;
    pub const JOG_BN: u64 = 1 << 56;
    pub const JOG_XP: u64 = 1 << 48;
    pub const OPT: u64 = 1 << 40;
    pub const LUB: u64 = 1 << 32;
    pub const SPD_CCW: u64 = 1 << 24;
    pub const K3: u64 = 1 << 16;
    // Lathe-only keys. The lathe panel carries a second spindle (SPD.2.*), a
    // spindle jog and a feed brake, a tailstock, one chip conveyor, one
    // tool-change key and MST, none of which the mill panel has. The C-axis jog
    // pair and C/S come from the same panel's jog group; the mill table has no
    // C axis at all, so they need masks of their own.
    pub const JOG_CN: u64 = 1 << 11;
    pub const JOG_CP: u64 = 1 << 12;
    pub const C_S: u64 = 1 << 18;
    pub const SPD2_CW: u64 = 1 << 1;
    pub const SPD2_SP: u64 = 1 << 2;
    pub const SPD2_CCW: u64 = 1 << 3;
    pub const SPD_JOG: u64 = 1 << 4;
    pub const FEED_BRAKE: u64 = 1 << 5;
    pub const TAIL: u64 = 1 << 6;
    pub const CHIP: u64 = 1 << 8;
    pub const TOOL_CHG: u64 = 1 << 9;
    pub const MST: u64 = 1 << 10;
}

pub const BUTTON_PINS: [(&str, u64); 58] = [
    ("program_protect", buttons::PROGRAM_PROTECT),
    ("estop", buttons::ESTOP),
    ("cycle_start", buttons::CYCLE_START),
    ("cycle_stop", buttons::CYCLE_STOP),
    ("rapid", buttons::RAPID),
    ("mlk", buttons::MLK),
    ("flush", buttons::FLUSH),
    ("spd_sp", buttons::SPD_SP),
    ("k2", buttons::K2),
    ("jog_xn", buttons::JOG_XN),
    ("bdt", buttons::BDT),
    ("spd_cts", buttons::SPD_CTS),
    ("spd_cw", buttons::SPD_CW),
    ("arm_man", buttons::ARM_MAN),
    ("jog_yp", buttons::JOG_YP),
    ("sbk", buttons::SBK),
    ("coolant", buttons::COOLANT),
    ("mag_ccw", buttons::MAG_CCW),
    ("pot_dn", buttons::POT_DN),
    ("jog_zp", buttons::JOG_ZP),
    ("mpg_sim", buttons::MPG_SIM),
    ("pt_la", buttons::PT_LA),
    ("mag_cw", buttons::MAG_CW),
    ("chuck", buttons::CHUCK),
    ("jog_an", buttons::JOG_AN),
    ("jog_ap", buttons::JOG_AP),
    ("air_blow", buttons::AIR_BLOW),
    ("spd_ori", buttons::SPD_ORI),
    ("chip_ccw", buttons::CHIP_CCW),
    ("jog_bp", buttons::JOG_BP),
    ("jog_zn", buttons::JOG_ZN),
    ("worklamp", buttons::WORKLAMP),
    ("k1", buttons::K1),
    ("chip_sp", buttons::CHIP_SP),
    ("safe_door", buttons::SAFE_DOOR),
    ("jog_yn", buttons::JOG_YN),
    ("drn", buttons::DRN),
    ("corridor", buttons::CORRIDOR),
    ("chip_cw", buttons::CHIP_CW),
    ("k4", buttons::K4),
    ("jog_bn", buttons::JOG_BN),
    ("jog_xp", buttons::JOG_XP),
    ("opt", buttons::OPT),
    ("lube", buttons::LUB),
    ("spd_ccw", buttons::SPD_CCW),
    ("k3", buttons::K3),
    // Lathe-only keys, appended so the mill pin order stays stable.
    ("spd2_cw", buttons::SPD2_CW),
    ("spd2_sp", buttons::SPD2_SP),
    ("spd2_ccw", buttons::SPD2_CCW),
    ("spd_jog", buttons::SPD_JOG),
    ("feed_brake", buttons::FEED_BRAKE),
    ("tail", buttons::TAIL),
    ("chip", buttons::CHIP),
    ("tool_chg", buttons::TOOL_CHG),
    ("mst", buttons::MST),
    ("jog_cn", buttons::JOG_CN),
    ("jog_cp", buttons::JOG_CP),
    ("c_s", buttons::C_S),
];

pub mod leds {
    pub const RAPID: u64 = 1 << 0;
    pub const MLK: u64 = 1 << 1;
    pub const FLUSH: u64 = 1 << 2;
    pub const SPD_SP: u64 = 1 << 3;
    pub const K2: u64 = 1 << 4;
    pub const C: u64 = 1 << 5;
    pub const CYCLE_OFF: u64 = 1 << 6;
    pub const JOG_XN: u64 = 1 << 8;
    pub const BDT: u64 = 1 << 9;
    pub const SPD_CTS: u64 = 1 << 10;
    pub const SPD_CW: u64 = 1 << 11;
    pub const ARM_MAN: u64 = 1 << 12;
    pub const B: u64 = 1 << 13;
    pub const CYCLE_ON: u64 = 1 << 14;
    pub const JOG_YP: u64 = 1 << 16;
    pub const SBK: u64 = 1 << 17;
    pub const COOLANT: u64 = 1 << 18;
    pub const MAG_CCW: u64 = 1 << 19;
    pub const POT_DN: u64 = 1 << 20;
    pub const A: u64 = 1 << 21;
    pub const READY: u64 = 1 << 22;
    pub const JOG_ZP: u64 = 1 << 24;
    pub const MPG_SIM: u64 = 1 << 25;
    pub const PT_LA: u64 = 1 << 26;
    pub const MAG_CW: u64 = 1 << 27;
    pub const CHUCK: u64 = 1 << 28;
    pub const Z: u64 = 1 << 29;
    pub const RUN: u64 = 1 << 30;
    pub const JOG_AN: u64 = 1 << 32;
    pub const JOG_AP: u64 = 1 << 33;
    pub const AIR_BLOW: u64 = 1 << 34;
    pub const SPD_ORI: u64 = 1 << 35;
    pub const CHIP_CCW: u64 = 1 << 36;
    pub const Y: u64 = 1 << 37;
    pub const AIRBLOW_ALARM: u64 = 1 << 38;
    pub const JOG_BP: u64 = 1 << 40;
    pub const JOG_ZN: u64 = 1 << 41;
    pub const WORKLAMP: u64 = 1 << 42;
    pub const K1: u64 = 1 << 43;
    pub const CHIP_SP: u64 = 1 << 44;
    pub const X: u64 = 1 << 45;
    pub const OIL_ALARM: u64 = 1 << 46;
    pub const SAFE_DOOR: u64 = 1 << 48;
    pub const JOG_YN: u64 = 1 << 49;
    pub const DRN: u64 = 1 << 50;
    pub const CORRIDOR: u64 = 1 << 51;
    pub const CHIP_CW: u64 = 1 << 52;
    pub const K4: u64 = 1 << 53;
    pub const GEAR_ALARM: u64 = 1 << 54;
    pub const JOG_BN: u64 = 1 << 56;
    pub const JOG_XP: u64 = 1 << 57;
    pub const OPT: u64 = 1 << 58;
    pub const LUB: u64 = 1 << 59;
    pub const SPD_CCW: u64 = 1 << 60;
    pub const K3: u64 = 1 << 61;
    pub const MC_ALARM: u64 = 1 << 62;
    // Lathe alarm lamps: NC / PLC / MMC / EMG replace the mill's MC / OIL /
    // GEAR / AIR BLOW column. Free bits, so the mill lamps keep their masks.
    pub const NC: u64 = 1 << 7;
    pub const PLC: u64 = 1 << 15;
    pub const MMC: u64 = 1 << 23;
    pub const EMG: u64 = 1 << 31;
}

pub const LED_PINS: [(&str, u64); 60] = [
    ("led_rapid", leds::RAPID),
    ("led_mlk", leds::MLK),
    ("led_flush", leds::FLUSH),
    ("led_spd_sp", leds::SPD_SP),
    ("led_k2", leds::K2),
    ("led_c", leds::C),
    ("led_cycle_off", leds::CYCLE_OFF),
    ("led_jog_xn", leds::JOG_XN),
    ("led_bdt", leds::BDT),
    ("led_spd_cts", leds::SPD_CTS),
    ("led_spd_cw", leds::SPD_CW),
    ("led_arm_man", leds::ARM_MAN),
    ("led_b", leds::B),
    ("led_cycle_on", leds::CYCLE_ON),
    ("led_jog_yp", leds::JOG_YP),
    ("led_sbk", leds::SBK),
    ("led_coolant", leds::COOLANT),
    ("led_mag_ccw", leds::MAG_CCW),
    ("led_pot_dn", leds::POT_DN),
    ("led_a", leds::A),
    ("led_ready", leds::READY),
    ("led_jog_zp", leds::JOG_ZP),
    ("led_mpg_sim", leds::MPG_SIM),
    ("led_pt_la", leds::PT_LA),
    ("led_mag_cw", leds::MAG_CW),
    ("led_chuck", leds::CHUCK),
    ("led_z", leds::Z),
    ("led_run", leds::RUN),
    ("led_jog_an", leds::JOG_AN),
    ("led_jog_ap", leds::JOG_AP),
    ("led_air_blow", leds::AIR_BLOW),
    ("led_spd_ori", leds::SPD_ORI),
    ("led_chip_ccw", leds::CHIP_CCW),
    ("led_y", leds::Y),
    ("led_airblow_alarm", leds::AIRBLOW_ALARM),
    ("led_jog_bp", leds::JOG_BP),
    ("led_jog_zn", leds::JOG_ZN),
    ("led_worklamp", leds::WORKLAMP),
    ("led_k1", leds::K1),
    ("led_chip_sp", leds::CHIP_SP),
    ("led_x", leds::X),
    ("led_oil_alarm", leds::OIL_ALARM),
    ("led_safe_door", leds::SAFE_DOOR),
    ("led_jog_yn", leds::JOG_YN),
    ("led_drn", leds::DRN),
    ("led_corridor", leds::CORRIDOR),
    ("led_chip_cw", leds::CHIP_CW),
    ("led_k4", leds::K4),
    ("led_gear_alarm", leds::GEAR_ALARM),
    ("led_jog_bn", leds::JOG_BN),
    ("led_jog_xp", leds::JOG_XP),
    ("led_opt", leds::OPT),
    ("led_lub", leds::LUB),
    ("led_spd_ccw", leds::SPD_CCW),
    ("led_k3", leds::K3),
    ("led_mc_alarm", leds::MC_ALARM),
    // Lathe lamps, appended so the mill pin order stays stable.
    ("led_nc", leds::NC),
    ("led_plc", leds::PLC),
    ("led_mmc", leds::MMC),
    ("led_emg", leds::EMG),
];

pub const MODE_PINS: [(&str, PanelMode); 8] = [
    ("mode_edit", PanelMode::Edit),
    ("mode_auto", PanelMode::Auto),
    ("mode_mdi", PanelMode::Mdi),
    ("mode_jog", PanelMode::Jog),
    ("mode_mpg", PanelMode::Mpg),
    ("mode_inc", PanelMode::Inc),
    ("mode_home", PanelMode::Home),
    ("mode_proghome", PanelMode::ProgHome),
];

pub const HOME_PINS: [&str; 4] = ["home-all", "home-x", "home-y", "home-z"];
pub const PROGHOME_PINS: [&str; 4] = ["proghome-all", "proghome-x", "proghome-y", "proghome-z"];

pub use linc_core::model::PanelDisplay;

#[derive(Clone, Debug, Default)]
pub struct PanelOutputs {
    pub buttons: u64,
    pub mode: PanelModeValidity,
    pub feed_override: f64,
    pub rapid_override: f64,
    pub spindle_override: f64,
    pub program_protect: bool,
    pub estop: bool,
    /// Simulation-only probe contact, wired to `rio.sim-probe`.
    pub probe: bool,
    pub home: [bool; 4],
    pub proghome: [bool; 4],
    pub step: bool,
    /// Simulation-only machine power state; real panels leave this None.
    pub power_on: Option<bool>,
    /// Momentary POWER ON/OFF key edges for simulation hardware.
    pub power_on_pulse: bool,
    pub power_off_pulse: bool,
    /// One-shot E-stop release: asks the controller to leave E-stop without
    /// turning the machine on.
    pub estop_reset_pulse: bool,
    pub display: PanelDisplay,
}

impl PanelOutputs {
    pub fn button(&self, mask: u64) -> bool {
        self.buttons & mask != 0
    }
}

pub trait PanelDriver {
    fn poll(&mut self) -> Result<PanelSnapshot, PanelError>;
    fn write(&mut self, outputs: &PanelOutputs) -> Result<(), PanelError>;
    fn capabilities(&self) -> PanelCapabilities;
    /// Observe HAL-written display inputs. Simulation drivers use this to
    /// reflect machine power/E-stop lamps in their local panel state.
    fn observe_display(&mut self, _display: &PanelDisplay) {}
}

/// Decode a steady 3-bit mode field into the one-hot contract. Invalid values
/// fail closed.
pub fn decode_mode_field(value: u8) -> PanelModeValidity {
    let mode = match value {
        0 => Some(PanelMode::Edit),
        1 => Some(PanelMode::Auto),
        2 => Some(PanelMode::Mdi),
        3 => Some(PanelMode::Jog),
        4 => Some(PanelMode::Mpg),
        5 => Some(PanelMode::Inc),
        6 => Some(PanelMode::Home),
        7 => Some(PanelMode::ProgHome),
        _ => None,
    };
    match mode {
        Some(mode) => PanelModeValidity::Valid(mode),
        None => PanelModeValidity::Invalid(Vec::new()),
    }
}

/// Decode a one-hot mode bitmap (used by the simulator and future panels).
pub fn decode_one_hot(mut bits: u8) -> PanelModeValidity {
    let mut modes = Vec::new();
    for (index, mode) in PanelMode::ALL.iter().enumerate() {
        if bits & (1 << index) != 0 {
            modes.push(*mode);
            bits &= !(1 << index);
        }
    }
    match modes.len() {
        1 => PanelModeValidity::Valid(modes[0]),
        0 => PanelModeValidity::Invalid(Vec::new()),
        _ => PanelModeValidity::Invalid(modes),
    }
}

pub fn one_hot_bits(mode: PanelModeValidity) -> u8 {
    match mode {
        PanelModeValidity::Valid(mode) => {
            let index = PanelMode::ALL
                .iter()
                .position(|item| *item == mode)
                .unwrap_or(0);
            1 << index
        }
        _ => 0,
    }
}

/// Override switch decoding shared by the physical protocol and simulator.
pub fn decode_feed_override(value: u8) -> f64 {
    value as f64 / 10.0
}

pub fn decode_rapid_override(value: u8) -> f64 {
    match value {
        0 => 0.0,
        1 => 0.25,
        2 => 0.5,
        3 => 1.0,
        _ => 1.0,
    }
}

pub fn decode_spindle_override(value: u8) -> f64 {
    match value {
        0 => 0.5,
        1 => 0.6,
        2 => 0.7,
        3 => 0.8,
        4 => 0.9,
        5 => 1.0,
        6 => 1.1,
        7 => 1.2,
        _ => 1.0,
    }
}

/// Convert decimal 0..99 to the panel's BCD-like byte.
pub fn dec_to_bcd_hex(value: i32) -> u8 {
    let value = value.clamp(0, 99);
    (((value / 10) as u8) << 4) | ((value % 10) as u8)
}

/// Encode the LED bitmap plus two tool displays into the 10-byte panel frame.
pub fn led_state_to_bytes(leds: u64) -> [u8; 10] {
    let mut led_state: u128 = u64::MAX as u128;
    led_state &= !(leds as u128);
    led_state <<= 16;
    let mut output = [0u8; 10];
    for i in 0..10 {
        let shift = (9 - i) * 8;
        output[i] = ((led_state >> shift) & 0xFF) as u8;
    }
    output
}

pub fn extract_button_state(response: &[u8]) -> u64 {
    if response.len() < 10 {
        return 0;
    }
    let mut button_state = 0u64;
    for i in 2..10 {
        button_state |= (response[i] as u64) << ((9 - i) * 8);
    }
    button_state
}

pub fn modbus_crc16(data: &[u8]) -> [u8; 2] {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            if crc & 0x0001 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    [(crc & 0xFF) as u8, ((crc >> 8) & 0xFF) as u8]
}

pub fn base_capabilities(transport: &str) -> PanelCapabilities {
    PanelCapabilities {
        buttons: BUTTON_PINS.len(),
        leds: LED_PINS.len(),
        displays: 3,
        has_status_display: true,
        mode_encoding: "one_hot_steady".into(),
        override_encoding: "analog_steady".into(),
        pendant_supported: true,
        transport: transport.into(),
    }
}

pub fn empty_snapshot() -> PanelSnapshot {
    PanelSnapshot {
        mode: PanelModeValidity::Unknown,
        link: PanelLinkStats::default(),
        capabilities: base_capabilities("none"),
        pendant: PendantState::default(),
        jog: JogState::default(),
        ..Default::default()
    }
}

pub fn make_snapshot(
    connected: bool,
    error: bool,
    outputs: &PanelOutputs,
    caps: PanelCapabilities,
    link: PanelLinkStats,
) -> PanelSnapshot {
    PanelSnapshot {
        connected,
        communication_error: error,
        mode: if connected {
            outputs.mode.clone()
        } else {
            PanelModeValidity::Unknown
        },
        feed_override: outputs.feed_override,
        rapid_override: outputs.rapid_override,
        spindle_override: outputs.spindle_override,
        program_protect: outputs.program_protect,
        program_protect_known: connected,
        estop: outputs.estop,
        probe: outputs.probe,
        current_tool: outputs.display.sseg_tool.max(0) as u16,
        next_tool: outputs.display.sseg_next_tool.max(0) as u16,
        buttons: outputs.buttons,
        leds: outputs.display.leds,
        sseg_tool: outputs.display.sseg_tool,
        sseg_next_tool: outputs.display.sseg_next_tool,
        sseg_status: outputs.display.sseg_status,
        home: outputs.home,
        proghome: outputs.proghome,
        step: outputs.step,
        rapid: outputs.button(buttons::RAPID),
        jog: JogState {
            mode: outputs.mode.label().into(),
            active_axis: None,
            speed: 0.0,
            rapid: outputs.button(buttons::RAPID),
            increment: None,
            remaining_step: None,
            handwheel_scale: None,
        },
        link,
        capabilities: caps,
        pendant: PendantState::default(),
        snapshot_age_ms: now_ms(),
        power_on: outputs.power_on,
        power_on_pulse: outputs.power_on_pulse,
        estop_reset_pulse: outputs.estop_reset_pulse,
        power_off_pulse: outputs.power_off_pulse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel bit tables are verified against the machine. This serial
    /// driver and the NML reader in `linc-linuxcnc/src/panel.rs` must agree: the
    /// driver decodes the panel's bitfield into pins, and the reader encodes
    /// those pins back into a bitfield for the UI.
    ///
    /// Two names share a bit in the panel's own table: `POT_DN`/`CHUCK` on bit 29
    /// and `K2`/`K1` on bit 34. `CHUCK` moves to the free bit 28, which hardware
    /// verification confirmed. `K1`/`K2` stay on bit 34 because the panel really
    /// does report both keys there; the pins alias instead of one of them being
    /// invented.
    #[test]
    fn button_table_matches_the_verified_panel() {
        // Hardware-verified on the SZGH-1080 panel.
        assert_eq!(buttons::LUB, 1 << 32);
        assert_eq!(buttons::K1, 1 << 34);
        assert_eq!(buttons::K2, 1 << 34);
        assert_eq!(buttons::CHUCK, 1 << 28);
        assert_eq!(buttons::POT_DN, 1 << 29);

        // Every other button keeps its own bit; only K1/K2 may alias.
        let mut seen: std::collections::HashMap<u32, &str> = std::collections::HashMap::new();
        for (name, mask) in BUTTON_PINS {
            let bit = mask.trailing_zeros();
            if let Some(previous) = seen.insert(bit, name) {
                assert!(
                    bit == 34 && (previous == "k1" || previous == "k2"),
                    "button {name} reuses bit {bit} of {previous}; only K1/K2 alias on this panel"
                );
            }
        }
    }

    #[test]
    fn one_hot_contract_fails_closed() {
        assert_eq!(
            decode_mode_field(1),
            PanelModeValidity::Valid(PanelMode::Auto)
        );
        assert!(matches!(
            decode_mode_field(9),
            PanelModeValidity::Invalid(_)
        ));
        let bits = one_hot_bits(PanelModeValidity::Valid(PanelMode::Mdi));
        assert_eq!(
            decode_one_hot(bits),
            PanelModeValidity::Valid(PanelMode::Mdi)
        );
        assert!(matches!(decode_one_hot(0), PanelModeValidity::Invalid(_)));
        assert!(matches!(
            decode_one_hot(0b11),
            PanelModeValidity::Invalid(_)
        ));
    }

    #[test]
    fn panel_pin_manifest_matches_hal() {
        assert_eq!(BUTTON_PINS.len(), 58);
        assert_eq!(LED_PINS.len(), 60);
        assert_eq!(MODE_PINS.len(), 8);
        assert_eq!(HOME_PINS.len(), 4);
        assert_eq!(PROGHOME_PINS.len(), 4);
        let mut seen = std::collections::HashSet::new();
        for (name, _) in BUTTON_PINS {
            assert!(seen.insert(name));
        }
        // Names alone are not enough: two names sharing one bit make the pins
        // indistinguishable on the wire. K1 and K2 do share bit 34 on this panel
        // (hardware-verified), so that one alias is expected; any other
        // collision is a bug.
        let mut bits: std::collections::HashMap<u64, &str> = std::collections::HashMap::new();
        for (name, mask) in BUTTON_PINS {
            if let Some(previous) = bits.insert(mask, name) {
                assert!(
                    mask.trailing_zeros() == 34 && (previous == "k1" || previous == "k2"),
                    "button {name} reuses a bit mask already assigned to {previous}"
                );
            }
        }
    }

    #[test]
    fn bcd_and_led_frame_roundtrip() {
        assert_eq!(dec_to_bcd_hex(54), 0x54);
        assert_eq!(dec_to_bcd_hex(99), 0x99);
        assert_eq!(dec_to_bcd_hex(-1), 0x00);
        let mut expected = [0xFF; 10];
        expected[8] = 0;
        expected[9] = 0;
        assert_eq!(led_state_to_bytes(0), expected);
        assert_eq!(led_state_to_bytes(u64::MAX)[..8], [0u8; 8]);
    }

    #[test]
    fn crc_matches_known_modbus_value() {
        // CRC-16/MODBUS of 01 03 00 00 00 01 = 0x0A84 (wire low first).
        let crc = modbus_crc16(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(crc, [0x84, 0x0A]);
    }

    #[test]
    fn override_decoding_matches_panel_detents() {
        assert_eq!(decode_feed_override(10), 1.0);
        assert_eq!(decode_rapid_override(3), 1.0);
        assert_eq!(decode_spindle_override(5), 1.0);
        assert_eq!(decode_spindle_override(7), 1.2);
    }
}
