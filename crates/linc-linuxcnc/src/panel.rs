//! Panel state read from the `cnc_panel.*` HAL pins.
//!
//! The panel service remains the owner of the physical link. This module only
//! reads the pins it publishes; it never opens the serial port and has no write
//! path.

use std::time::{Duration, Instant};

use linc_core::config::Profile;
use linc_core::model::{
    JogState, PanelCapabilities, PanelLinkStats, PanelMode, PanelModeValidity, PanelSnapshot,
    PendantState,
};

use linc_hal::HalReader;

const MODE_PINS: [(&str, PanelMode); 8] = [
    ("mode_edit", PanelMode::Edit),
    ("mode_auto", PanelMode::Auto),
    ("mode_mdi", PanelMode::Mdi),
    ("mode_jog", PanelMode::Jog),
    ("mode_mpg", PanelMode::Mpg),
    ("mode_inc", PanelMode::Inc),
    ("mode_home", PanelMode::Home),
    ("mode_proghome", PanelMode::ProgHome),
];

const BUTTON_PINS: [(&str, u32); 46] = [
    ("program_protect", 0),
    ("estop", 7),
    ("cycle_start", 14),
    ("cycle_stop", 15),
    ("rapid", 63),
    ("mlk", 55),
    ("flush", 47),
    ("spd_sp", 39),
    ("k2", 34),
    ("jog_xn", 62),
    ("bdt", 54),
    ("spd_cts", 46),
    ("spd_cw", 38),
    ("arm_man", 30),
    ("jog_yp", 61),
    ("sbk", 53),
    ("coolant", 45),
    ("mag_ccw", 37),
    ("pot_dn", 29),
    ("jog_zp", 60),
    ("mpg_sim", 52),
    ("pt_la", 44),
    ("mag_cw", 36),
    ("chuck", 28),
    ("jog_an", 59),
    ("jog_ap", 51),
    ("air_blow", 43),
    ("spd_ori", 35),
    ("chip_ccw", 27),
    ("jog_bp", 58),
    ("jog_zn", 50),
    ("worklamp", 42),
    ("k1", 34),
    ("chip_sp", 26),
    ("safe_door", 57),
    ("jog_yn", 49),
    ("drn", 41),
    ("corridor", 33),
    ("chip_cw", 25),
    ("k4", 17),
    ("jog_bn", 56),
    ("jog_xp", 48),
    ("opt", 40),
    ("lub", 32),
    ("spd_ccw", 24),
    ("k3", 16),
];

const LED_PINS: [(&str, u32); 56] = [
    ("led_rapid", 0),
    ("led_mlk", 1),
    ("led_flush", 2),
    ("led_spd_sp", 3),
    ("led_k2", 4),
    ("led_c", 5),
    ("led_cycle_off", 6),
    ("led_jog_xn", 8),
    ("led_bdt", 9),
    ("led_spd_cts", 10),
    ("led_spd_cw", 11),
    ("led_arm_man", 12),
    ("led_b", 13),
    ("led_cycle_on", 14),
    ("led_jog_yp", 16),
    ("led_sbk", 17),
    ("led_coolant", 18),
    ("led_mag_ccw", 19),
    ("led_pot_dn", 20),
    ("led_a", 21),
    ("led_ready", 22),
    ("led_jog_zp", 24),
    ("led_mpg_sim", 25),
    ("led_pt_la", 26),
    ("led_mag_cw", 27),
    ("led_chuck", 28),
    ("led_z", 29),
    ("led_run", 30),
    ("led_jog_an", 32),
    ("led_jog_ap", 33),
    ("led_air_blow", 34),
    ("led_spd_ori", 35),
    ("led_chip_ccw", 36),
    ("led_y", 37),
    ("led_airblow_alarm", 38),
    ("led_jog_bp", 40),
    ("led_jog_zn", 41),
    ("led_worklamp", 42),
    ("led_k1", 43),
    ("led_chip_sp", 44),
    ("led_x", 45),
    ("led_oil_alarm", 46),
    ("led_safe_door", 48),
    ("led_jog_yn", 49),
    ("led_drn", 50),
    ("led_corridor", 51),
    ("led_chip_cw", 52),
    ("led_k4", 53),
    ("led_gear_alarm", 54),
    ("led_jog_bn", 56),
    ("led_jog_xp", 57),
    ("led_opt", 58),
    ("led_lub", 59),
    ("led_spd_ccw", 60),
    ("led_k3", 61),
    ("led_mc_alarm", 62),
];

#[derive(Clone, Debug, Default)]
struct Bitmap {
    buttons: u64,
    leds: u64,
    sseg_tool: i32,
    sseg_next: i32,
}

/// The panel-derived fields a snapshot needs. `current_tool` comes from NML,
/// not from the panel, and is filled by the snapshot builder.
#[derive(Clone, Debug)]
pub struct PanelState {
    pub connected: bool,
    pub communication_error: bool,
    pub mode: PanelModeValidity,
    pub feed_override: f64,
    pub rapid_override: f64,
    pub spindle_override: f64,
    pub program_protect: bool,
    pub program_protect_known: bool,
    pub estop: bool,
    pub rapid: bool,
    pub override_limits: bool,
    pub next_tool: u16,
    pub buttons: u64,
    pub leds: u64,
    pub sseg_tool: i32,
    pub sseg_next: i32,
    pub sseg_status: i32,
    pub tool_change_active: bool,
    pub door_closed: bool,
    pub spindle_override_enabled: bool,
    pub pendant_connected: bool,
    pub pendant_jog_enable: bool,
    pub pendant_axis: Option<String>,
    pub pendant_scale: Option<f64>,
    pub pendant_step: Option<String>,
}

impl PanelState {
    pub fn offline(_profile: &Profile) -> Self {
        Self {
            connected: false,
            communication_error: true,
            mode: PanelModeValidity::Unknown,
            feed_override: 1.0,
            rapid_override: 1.0,
            spindle_override: 1.0,
            program_protect: true,
            program_protect_known: false,
            estop: false,
            rapid: false,
            override_limits: false,
            next_tool: 0,
            buttons: 0,
            leds: 0,
            sseg_tool: 0,
            sseg_next: 0,
            sseg_status: -1,
            tool_change_active: false,
            door_closed: true,
            spindle_override_enabled: true,
            pendant_connected: false,
            pendant_jog_enable: false,
            pendant_axis: None,
            pendant_scale: None,
            pendant_step: None,
        }
    }

    pub fn to_snapshot(&self, profile: &Profile, current_tool: u16) -> PanelSnapshot {
        let capabilities = PanelCapabilities {
            buttons: 46,
            leds: 56,
            displays: 3,
            has_status_display: true,
            mode_encoding: "one_hot_steady".into(),
            override_encoding: "analog_steady".into(),
            pendant_supported: profile.config.pendant.enabled,
            transport: profile
                .config
                .panel
                .link
                .kind
                .clone()
                .unwrap_or_else(|| "hal".into()),
        };
        let link = PanelLinkStats {
            port: profile
                .config
                .panel
                .link
                .port
                .clone()
                .unwrap_or_else(|| "hal".into()),
            baud: profile.config.panel.link.baud.unwrap_or(115200),
            cycle_time_ms: profile.config.panel.link.cycle_time_ms.unwrap_or(0) as f64,
            available: self.connected,
            ..Default::default()
        };
        let jog = JogState {
            mode: self.mode.label().into(),
            active_axis: self.pendant_axis.clone(),
            handwheel_scale: self.pendant_scale,
            ..Default::default()
        };
        PanelSnapshot {
            connected: self.connected,
            communication_error: self.communication_error,
            mode: self.mode.clone(),
            feed_override: self.feed_override,
            rapid_override: self.rapid_override,
            spindle_override: self.spindle_override,
            program_protect: self.program_protect,
            program_protect_known: self.program_protect_known,
            estop: self.estop,
            rapid: self.rapid,
            current_tool,
            next_tool: self.next_tool,
            buttons: self.buttons,
            leds: self.leds,
            sseg_tool: self.sseg_tool,
            sseg_next_tool: self.sseg_next,
            sseg_status: self.sseg_status,
            jog,
            link,
            capabilities,
            pendant: PendantState {
                enabled: profile.config.pendant.enabled,
                connected: self.pendant_connected,
                jog_only: true,
                jog_enable: self.pendant_jog_enable,
                active_axis: self.pendant_axis.clone(),
                handwheel_scale: self.pendant_scale,
                step_mode: self.pendant_step.clone(),
                ignored_functions: profile.config.pendant.ignored_functions.clone(),
            },
            ..Default::default()
        }
    }
}

pub struct PanelReader {
    last_heartbeat: Option<i64>,
    last_heartbeat_at: Instant,
    bitmap: Bitmap,
    bitmap_at: Instant,
}

impl PanelReader {
    pub fn new() -> Self {
        Self {
            last_heartbeat: None,
            last_heartbeat_at: Instant::now(),
            bitmap: Bitmap::default(),
            bitmap_at: Instant::now() - Duration::from_secs(2),
        }
    }

    /// Read the panel pins. `None` means the HAL library or a pin was
    /// unavailable; the caller treats that as panel offline.
    pub fn read(&mut self, hal: Option<&HalReader>, profile: &Profile) -> PanelState {
        let Some(hal) = hal else {
            return PanelState::offline(profile);
        };
        let prefix = format!("{}.", profile.config.hal.panel_component);

        let mut state = PanelState::offline(profile);
        let mut active = Vec::new();
        for (pin, mode) in MODE_PINS {
            if bit(hal, &format!("{prefix}{pin}"), false) {
                active.push(mode);
            }
        }
        state.mode = match active.as_slice() {
            [mode] => PanelModeValidity::Valid(*mode),
            [] => PanelModeValidity::Unknown,
            many => PanelModeValidity::Invalid(many.to_vec()),
        };

        let heartbeat = hal
            .read(&format!("{prefix}heartbeat"))
            .and_then(|value| value.as_i64());
        let now = Instant::now();
        let heartbeat_ok = match heartbeat {
            None => true,
            Some(value) => {
                if self.last_heartbeat != Some(value) {
                    self.last_heartbeat = Some(value);
                    self.last_heartbeat_at = now;
                }
                now.duration_since(self.last_heartbeat_at) < Duration::from_millis(1500)
            }
        };

        state.connected = bit(hal, &profile.config.hal.pins.panel_connected, false) && heartbeat_ok;
        state.communication_error =
            bit(hal, &profile.config.hal.pins.panel_error, true) || !heartbeat_ok;

        let protect = read_bit(hal, &profile.config.hal.pins.program_protect);
        state.program_protect = protect.unwrap_or(true);
        state.program_protect_known = protect.is_some() && state.connected;

        state.feed_override = first_float(
            hal,
            &[
                "halui.feed-override.value",
                &profile.config.hal.pins.feed_override,
            ],
        )
        .unwrap_or(1.0);
        state.rapid_override = first_float(
            hal,
            &[
                "halui.rapid-override.value",
                &profile.config.hal.pins.rapid_override,
            ],
        )
        .unwrap_or(1.0);
        state.spindle_override = first_float(
            hal,
            &[
                "halui.spindle.0.override.value",
                &profile.config.hal.pins.spindle_override,
            ],
        )
        .unwrap_or(1.0);

        state.sseg_status = hal.s32(&format!("{prefix}sseg_status")).unwrap_or(-1);
        state.estop = bit(hal, &format!("{prefix}estop"), false);
        state.rapid = bit(hal, &format!("{prefix}rapid"), false);
        state.override_limits = bit(hal, "halui.joint.0.override-limits", false);
        state.next_tool = hal.s32("iocontrol.0.tool-prep-number").unwrap_or(0).max(0) as u16;
        state.tool_change_active = bit(hal, "iocontrol.0.tool-change", false);
        state.door_closed = bit(hal, &format!("{prefix}led_safe_door"), true);
        state.spindle_override_enabled = bit(hal, "halui.spindle.0.override.is-on", true);

        state.pendant_connected = bit(hal, "whb.axis.x.jog-enable", false);
        state.pendant_jog_enable = state.pendant_connected;
        state.pendant_scale = hal.float("whb.axis.x.jog-scale");

        if self.bitmap_at.elapsed() >= Duration::from_secs(1) {
            self.bitmap = read_bitmap(hal, &prefix);
            self.bitmap_at = now;
        }
        state.buttons = self.bitmap.buttons;
        state.leds = self.bitmap.leds;
        state.sseg_tool = self.bitmap.sseg_tool;
        state.sseg_next = self.bitmap.sseg_next;
        state
    }
}

fn read_bitmap(hal: &HalReader, prefix: &str) -> Bitmap {
    let mut bitmap = Bitmap::default();
    for (pin, bit_index) in BUTTON_PINS {
        if bit(hal, &format!("{prefix}{pin}"), false) {
            bitmap.buttons |= 1u64 << bit_index;
        }
    }
    for (pin, bit_index) in LED_PINS {
        if bit(hal, &format!("{prefix}{pin}"), false) {
            bitmap.leds |= 1u64 << bit_index;
        }
    }
    bitmap.sseg_tool = hal.s32(&format!("{prefix}sseg_magtool")).unwrap_or(0);
    bitmap.sseg_next = hal.s32(&format!("{prefix}sseg_spdtool")).unwrap_or(0);
    bitmap
}

fn read_bit(hal: &HalReader, name: &str) -> Option<bool> {
    hal.bit(name)
}

fn bit(hal: &HalReader, name: &str, default: bool) -> bool {
    hal.bit(name).unwrap_or(default)
}

fn first_float(hal: &HalReader, names: &[&str]) -> Option<f64> {
    names.iter().find_map(|name| hal.float(name))
}
