//! Simulation CNC control panel. This is a separate process/window from the
//! HMI and creates the same `cnc_panel` HAL surface as the physical driver.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use linc_core::diag::{DiagError, PanelFault, SimPanelPatch};
use linc_core::model::{
    now_ms, PanelCapabilities, PanelLinkStats, PanelMode, PanelModeValidity, PanelSnapshot,
};
use serde_json::{json, Value};

use crate::{
    base_capabilities, buttons, make_snapshot, PanelDisplay, PanelDriver, PanelError, PanelOutputs,
};

/// Link and input faults a simulator can produce on request.
///
/// `stale` is not here: it stops the diagnostic bus from advancing, which is the
/// panel service's publication, not the simulator's state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimFaults {
    pub disconnect: bool,
    pub timeout: bool,
    pub crc_error: bool,
    pub invalid_mode: bool,
    pub protect_unknown: bool,
}

impl SimFaults {
    pub fn set(&mut self, fault: PanelFault) {
        match fault {
            PanelFault::Disconnect => self.disconnect = true,
            PanelFault::Timeout => self.timeout = true,
            PanelFault::CrcError => self.crc_error = true,
            PanelFault::InvalidMode => self.invalid_mode = true,
            PanelFault::ProtectUnknown => self.protect_unknown = true,
            // The bus owns staleness; see `PanelBus::set_stale`.
            PanelFault::Stale => {}
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn labels(self) -> Vec<&'static str> {
        let mut labels = Vec::new();
        if self.disconnect {
            labels.push("disconnect");
        }
        if self.timeout {
            labels.push("timeout");
        }
        if self.crc_error {
            labels.push("crc_error");
        }
        if self.invalid_mode {
            labels.push("invalid_mode");
        }
        if self.protect_unknown {
            labels.push("protect_unknown");
        }
        labels
    }

    pub fn any(self) -> bool {
        !self.labels().is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct SimPanelState {
    pub connected: bool,
    pub error: bool,
    pub power: bool,
    pub power_on_pulse: bool,
    pub power_off_pulse: bool,
    /// One-shot: the E-stop was just released.
    pub estop_reset_pulse: bool,
    pub estop: bool,
    pub mode: PanelMode,
    pub feed_override: f64,
    pub rapid_override: f64,
    pub spindle_override: f64,
    pub program_protect: bool,
    /// Simulated probe contact. Asserted only while the panel PROBE key is held.
    pub probe: bool,
    pub buttons: u64,
    pub leds: u64,
    pub sseg_tool: i32,
    pub sseg_next_tool: i32,
    pub sseg_status: i32,
    pub velocity: [f64; 3],
    pub last_cycle_ms: f64,
    pub faults: SimFaults,
    /// Buttons a remote caller pressed with a deadline, so a held key is
    /// released even if the caller never does.
    pub holds: Vec<(u64, Instant)>,
}

/// How far through its power-up sequence a simulated cabinet starts.
///
/// The default is the state a real cabinet is found in before the operator
/// touches it: E-stop latched, power off. A launcher that wants the machine
/// ready to use can ask for a later point with `LINC_SIM_READY`; the panel then
/// raises the same one-shots the window's buttons raise, so the HAL sees an
/// operator's press rather than a special case.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SimStart {
    /// E-stop latched, machine off.
    #[default]
    Cold,
    /// The mushroom has been pulled out; the machine is still off.
    EstopReleased,
    /// Mushroom released and POWER ON pressed.
    Powered,
}

impl SimStart {
    /// `LINC_SIM_READY=estop` or `=power`; anything else starts cold.
    pub fn from_env() -> Self {
        Self::from_value(&std::env::var("LINC_SIM_READY").unwrap_or_default())
    }

    pub fn from_value(value: &str) -> Self {
        match value.trim() {
            "estop" => Self::EstopReleased,
            "power" => Self::Powered,
            _ => Self::Cold,
        }
    }
}

impl SimPanelState {
    /// Move the cabinet to `start`, as if the operator had already pulled the
    /// mushroom out (`estop`) or also pressed POWER ON (`power`).
    pub fn apply_start(&mut self, start: SimStart) {
        match start {
            SimStart::Cold => {}
            SimStart::EstopReleased => {
                self.estop = false;
                self.estop_reset_pulse = true;
            }
            SimStart::Powered => {
                self.estop = false;
                self.power = true;
                self.power_on_pulse = true;
                self.power_off_pulse = false;
            }
        }
    }
}

impl Default for SimPanelState {
    fn default() -> Self {
        Self {
            connected: true,
            error: false,
            // The simulator boots in the state a real cabinet is found in
            // before the operator powers it up: E-stop latched, power off.
            // Starting powered-on and un-latched hid the whole power-up
            // sequence and let the machine look ready when it was not.
            power: false,
            power_on_pulse: false,
            power_off_pulse: false,
            estop_reset_pulse: false,
            estop: true,
            mode: PanelMode::Auto,
            feed_override: 1.0,
            rapid_override: 1.0,
            spindle_override: 1.0,
            program_protect: false,
            probe: false,
            buttons: 0,
            leds: 0,
            sseg_tool: 12,
            sseg_next_tool: 0,
            sseg_status: 54,
            velocity: [0.0; 3],
            last_cycle_ms: 0.0,
            faults: SimFaults::default(),
            holds: Vec::new(),
        }
    }
}

impl SimPanelState {
    pub fn set_button(&mut self, mask: u64, pressed: bool) {
        if pressed {
            self.buttons |= mask;
        } else {
            self.buttons &= !mask;
        }
    }

    pub fn pulse_button(&mut self, mask: u64, duration: Duration) {
        // The UI uses this for momentary keys. The panel process keeps the
        // pulse for one cycle so HAL sees an edge.
        self.buttons |= mask;
        let _ = duration;
    }

    /// Press `mask` and release it on its own after `duration`, so a remote
    /// caller that disconnects cannot leave a key held forever.
    pub fn press_for(&mut self, mask: u64, duration: Duration) {
        self.buttons |= mask;
        if duration.is_zero() {
            let _ = self.release_expired_at(Instant::now());
            return;
        }
        let until = Instant::now() + duration;
        match self.holds.iter_mut().find(|(held, _)| *held == mask) {
            Some((_, at)) => *at = until,
            None => self.holds.push((mask, until)),
        }
    }

    /// Release every held button whose deadline passed. Returns true when the
    /// button mask changed.
    pub fn release_expired(&mut self) -> bool {
        self.release_expired_at(Instant::now())
    }

    fn release_expired_at(&mut self, now: Instant) -> bool {
        let mut released = 0u64;
        self.holds.retain(|(mask, at)| {
            if *at <= now {
                released |= *mask;
                false
            } else {
                true
            }
        });
        if released == 0 {
            return false;
        }
        self.buttons &= !released;
        true
    }

    pub fn led(&self, mask: u64) -> bool {
        self.leds & mask != 0
    }

    /// Apply the simulator part of a remote patch. `Stale` is the bus's, and
    /// button names are resolved by the caller.
    pub fn apply_patch(
        &mut self,
        patch: &SimPanelPatch,
        press: u64,
        release: u64,
    ) -> Result<(), DiagError> {
        if let Some(connected) = patch.connected {
            self.connected = connected;
        }
        if let Some(mode) = patch.mode.as_deref() {
            self.mode = parse_mode(mode)?;
        }
        if let Some(value) = patch.feed_override {
            check_override("feed_override", value)?;
            self.feed_override = value;
        }
        if let Some(value) = patch.rapid_override {
            check_override("rapid_override", value)?;
            self.rapid_override = value;
        }
        if let Some(value) = patch.spindle_override {
            check_override("spindle_override", value)?;
            self.spindle_override = value;
        }
        if let Some(protect) = patch.program_protect {
            self.program_protect = protect;
        }
        if let Some(estop) = patch.estop {
            // Releasing the mushroom raises the same one-shot the window's key
            // raises, so the controller leaves E-stop without being powered on.
            if self.estop && !estop {
                self.estop_reset_pulse = true;
            }
            self.estop = estop;
        }
        if let Some(power) = patch.power {
            if self.power != power {
                if power {
                    self.power_on_pulse = true;
                } else {
                    self.power_off_pulse = true;
                }
            }
            self.power = power;
        }
        self.buttons |= press;
        self.buttons &= !release;
        Ok(())
    }
}

/// Map a mode label (`AUTO`, `PROG.HOME`, ...) to the panel mode.
pub fn parse_mode(value: &str) -> Result<PanelMode, DiagError> {
    let value = value.trim();
    PanelMode::ALL
        .into_iter()
        .find(|mode| mode.label().eq_ignore_ascii_case(value))
        .ok_or_else(|| {
            DiagError::invalid_argument(format!(
                "unknown panel mode {value:?}; use a mode label, or diag.sim_fault invalid_mode"
            ))
        })
}

fn check_override(name: &str, value: f64) -> Result<(), DiagError> {
    if !value.is_finite() || !(0.0..=2.0).contains(&value) {
        return Err(DiagError::invalid_argument(format!(
            "{name} must be between 0 and 2"
        )));
    }
    Ok(())
}

/// Resolve button names against the panel table, so a tool cannot invent a pin.
pub fn button_mask(name: &str) -> Option<u64> {
    crate::BUTTON_PINS
        .iter()
        .find(|(button, _)| *button == name.trim())
        .map(|(_, mask)| *mask)
}

/// The names of the buttons currently pressed, for the diagnostic socket.
pub fn pressed_button_names(mask: u64) -> Vec<&'static str> {
    crate::BUTTON_PINS
        .iter()
        .filter(|(_, bit)| mask & *bit != 0)
        .map(|(name, _)| *name)
        .collect()
}

#[derive(Clone)]
pub struct SimPanelHandle {
    pub state: Arc<Mutex<SimPanelState>>,
}

impl SimPanelHandle {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SimPanelState::default())),
        }
    }

    pub fn set(&self, update: impl FnOnce(&mut SimPanelState)) {
        if let Ok(mut state) = self.state.lock() {
            update(&mut state);
        }
    }

    pub fn snapshot(&self) -> SimPanelState {
        self.state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default()
    }
}

impl Default for SimPanelHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// The simulator's control surface for the panel diagnostic socket.
///
/// It mutates the same handle the window and the driver use, so every change
/// goes through `cnc_panel.*` on the next cycle exactly as an operator's key
/// would. It exists only when the panel service was started with
/// `--allow-sim-control`; a real panel has no instance of it.
pub struct SimControlState {
    handle: SimPanelHandle,
}

impl SimControlState {
    pub fn new(handle: SimPanelHandle) -> Self {
        Self { handle }
    }

    fn state_value(&self) -> Value {
        let state = self.handle.snapshot();
        json!({
            "connected": state.connected,
            "error": state.error,
            "estop": state.estop,
            "power": state.power,
            "mode": state.mode.label(),
            "feed_override": state.feed_override,
            "rapid_override": state.rapid_override,
            "spindle_override": state.spindle_override,
            "program_protect": state.program_protect,
            "buttons": state.buttons,
            "buttons_hex": format!("0x{:016x}", state.buttons),
            "pressed": pressed_button_names(state.buttons),
            "leds": state.leds,
            "sseg_tool": state.sseg_tool,
            "sseg_next_tool": state.sseg_next_tool,
            "sseg_status": state.sseg_status,
            "faults": state.faults.labels(),
            "held_buttons": state.holds.len(),
        })
    }
}

fn resolve_buttons(names: &[String]) -> Result<u64, DiagError> {
    names.iter().try_fold(0u64, |mask, name| {
        button_mask(name).map(|bit| mask | bit).ok_or_else(|| {
            DiagError::invalid_argument(format!(
                "unknown panel button {name:?}; buttons are the names in linc_get_panel_state"
            ))
        })
    })
}

impl linc_diag::SimControl for SimControlState {
    fn state(&self) -> Value {
        self.state_value()
    }

    fn apply(&self, patch: &SimPanelPatch) -> Result<Value, DiagError> {
        let press = resolve_buttons(patch.press.as_deref().unwrap_or_default())?;
        let release = resolve_buttons(patch.release.as_deref().unwrap_or_default())?;
        if press & release != 0 {
            return Err(DiagError::invalid_argument(
                "a button cannot be pressed and released in the same patch",
            ));
        }
        let mut error = None;
        self.handle.set(|state| {
            if let Err(failure) = state.apply_patch(patch, press, release) {
                error = Some(failure);
            }
        });
        match error {
            Some(error) => Err(error),
            None => Ok(self.state_value()),
        }
    }

    fn buttons(
        &self,
        press: &[String],
        release: &[String],
        duration_ms: Option<u64>,
    ) -> Result<Value, DiagError> {
        let press_mask = resolve_buttons(press)?;
        let release_mask = resolve_buttons(release)?;
        if press_mask & release_mask != 0 {
            return Err(DiagError::invalid_argument(
                "a button cannot be pressed and released at once",
            ));
        }
        self.handle.set(|state| {
            if release_mask != 0 {
                state.buttons &= !release_mask;
                state.holds.retain(|(held, _)| held & release_mask == 0);
            }
            if press_mask != 0 {
                match duration_ms {
                    Some(duration) => {
                        state.press_for(press_mask, Duration::from_millis(duration.max(1)))
                    }
                    None => state.buttons |= press_mask,
                }
            }
        });
        Ok(self.state_value())
    }

    fn inject_fault(&self, fault: PanelFault) -> Result<Value, DiagError> {
        self.handle.set(|state| state.faults.set(fault));
        Ok(self.state_value())
    }

    fn clear_faults(&self) -> Result<Value, DiagError> {
        self.handle.set(|state| state.faults.clear());
        Ok(self.state_value())
    }

    fn reset(&self) -> Result<Value, DiagError> {
        self.handle.set(|state| {
            *state = SimPanelState::default();
        });
        Ok(self.state_value())
    }
}

pub struct SimPanelDriver {
    handle: SimPanelHandle,
    caps: PanelCapabilities,
    last_poll: Instant,
    cycle_time: Duration,
    last_display: PanelDisplay,
    outputs: PanelOutputs,
    /// When the current `power_on`/`power_off` pulse was first seen asserted,
    /// so the driver can expire it on its own cycle even when the simulator
    /// window (and its timer) is not running.
    pulse_set_at: Option<Instant>,
}

impl SimPanelDriver {
    pub fn new(handle: SimPanelHandle, cycle_time: Duration) -> Self {
        Self {
            handle,
            caps: base_capabilities("virtual"),
            last_poll: Instant::now(),
            cycle_time,
            last_display: PanelDisplay {
                leds: 0,
                sseg_tool: 12,
                sseg_next_tool: 0,
                sseg_status: 54,
                vel: [0.0; 3],
                homing: false,
            },
            outputs: PanelOutputs::default(),
            pulse_set_at: None,
        }
    }

    pub fn handle(&self) -> SimPanelHandle {
        self.handle.clone()
    }

    pub fn set_mode(&mut self, mode: PanelMode) {
        self.handle.set(|state| state.mode = mode);
    }

    pub fn press(&mut self, mask: u64, pressed: bool) {
        self.handle.set(|state| state.set_button(mask, pressed));
    }

    fn outputs_from_state(&self, state: &SimPanelState, connected: bool) -> PanelOutputs {
        let mode = if connected {
            PanelModeValidity::Valid(state.mode)
        } else {
            PanelModeValidity::Unknown
        };
        let active = |mask: u64| state.buttons & mask != 0;
        let home_active = state.mode == PanelMode::Home;
        let proghome_active = state.mode == PanelMode::ProgHome;
        let rapid = active(buttons::RAPID);
        let x = active(buttons::JOG_XP) || active(buttons::JOG_XN);
        let y = active(buttons::JOG_YP) || active(buttons::JOG_YN);
        let z = active(buttons::JOG_ZP) || active(buttons::JOG_ZN);
        let mut visible_buttons = state.buttons;
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
        let auto_active = state.mode == PanelMode::Auto;
        let step_active =
            auto_active && state.leds & crate::leds::SBK != 0 && active(buttons::CYCLE_START);
        if step_active {
            visible_buttons &= !buttons::CYCLE_START;
        }
        PanelOutputs {
            buttons: visible_buttons,
            mode,
            feed_override: state.feed_override,
            rapid_override: state.rapid_override,
            spindle_override: state.spindle_override,
            program_protect: state.program_protect,
            estop: state.estop,
            probe: state.probe,
            home: [
                home_active && rapid,
                home_active && x && !self.last_display.homing,
                home_active && y && !self.last_display.homing,
                home_active && z && !self.last_display.homing,
            ],
            proghome: [
                proghome_active && rapid,
                proghome_active && x && !self.last_display.homing,
                proghome_active && y && !self.last_display.homing,
                proghome_active && z && !self.last_display.homing,
            ],
            step: step_active,
            power_on: Some(state.power && !state.estop),
            power_on_pulse: state.power_on_pulse,
            power_off_pulse: state.power_off_pulse,
            estop_reset_pulse: state.estop_reset_pulse,
            display: self.last_display.clone(),
        }
    }
}

impl PanelDriver for SimPanelDriver {
    fn observe_display(&mut self, display: &PanelDisplay) {
        // The panel's POWER and E-STOP are operator-owned inputs; the READY and
        // MC_ALARM lamps are outputs. Deriving either input from a lamp created
        // two problems:
        //   * E-STOP followed MC_ALARM, so the controller could clear an E-stop
        //     the operator had pressed;
        //   * POWER followed READY, which cannot be true while LinuxCNC is in
        //     E-stop — so `cnc_panel.power_on` (wired to `halui.estop.reset`)
        //     could never re-assert and the controller could never leave
        //     E-stop.
        // The lamps are still consumed for the alarm/axis indicators below.
        let _ = display;
    }

    fn poll(&mut self) -> Result<PanelSnapshot, PanelError> {
        let due = self.last_poll.elapsed() >= self.cycle_time;
        if due {
            self.last_poll = Instant::now();
            // Power pulses are one-shots that drive `cnc_panel.power_on` ->
            // `halui.estop.reset`. They must expire on the driver cycle: the
            // panel service runs headless under LinuxCNC, where the simulator
            // window (and its timer) never runs, so a pulse set there would
            // stay asserted.
            let state = self.handle.snapshot();
            let asserted = state.power_on_pulse || state.power_off_pulse || state.estop_reset_pulse;
            match (asserted, self.pulse_set_at) {
                (true, None) => self.pulse_set_at = Some(Instant::now()),
                (true, Some(at)) if at.elapsed() >= POWER_PULSE => {
                    self.handle.set(|state| {
                        state.power_on_pulse = false;
                        state.power_off_pulse = false;
                        state.estop_reset_pulse = false;
                    });
                    self.pulse_set_at = None;
                }
                (false, _) => self.pulse_set_at = None,
                _ => {}
            }
            // A remote caller can press a key for a bounded time and go away;
            // the deadline lives in the state so the key is released anyway.
            self.handle.set(|state| {
                state.release_expired();
            });
        }

        let state = self.handle.snapshot();
        let faults = state.faults;
        let connected = state.connected && !faults.disconnect && !faults.timeout;
        let error = state.error || faults.disconnect || faults.timeout || faults.crc_error;
        let outputs = self.outputs_from_state(&state, connected);
        if due {
            self.outputs = outputs.clone();
        }
        let mut snapshot = make_snapshot(
            connected,
            error,
            &outputs,
            self.caps.clone(),
            PanelLinkStats {
                cycle_time_ms: state
                    .last_cycle_ms
                    .max(self.cycle_time.as_secs_f64() * 1000.0),
                port: "virtual".into(),
                available: false,
                last_rx_age_ms: Some(0),
                ..Default::default()
            },
        );
        // Faults that do not stop the link: the panel is connected but its
        // inputs are the ones the fault describes.
        if faults.invalid_mode && connected {
            snapshot.mode = PanelModeValidity::Invalid(Vec::new());
        }
        if faults.protect_unknown {
            snapshot.program_protect_known = false;
        }
        snapshot.snapshot_age_ms = now_ms();
        Ok(snapshot)
    }

    fn write(&mut self, outputs: &PanelOutputs) -> Result<(), PanelError> {
        self.last_display = outputs.display.clone();
        self.handle.set(|state| {
            state.leds = outputs.display.leds;
            state.sseg_tool = outputs.display.sseg_tool;
            state.sseg_next_tool = outputs.display.sseg_next_tool;
            state.sseg_status = outputs.display.sseg_status;
            state.velocity = outputs.display.vel;
            let started = Instant::now();
            state.last_cycle_ms = started.elapsed().as_secs_f64() * 1000.0;
        });
        Ok(())
    }

    fn capabilities(&self) -> PanelCapabilities {
        self.caps.clone()
    }
}

/// Render the 7-segment status display as text, `--` when unknown.
pub fn status_display_text(value: i32) -> String {
    if (54..=59).contains(&value) {
        value.to_string()
    } else {
        "--".into()
    }
}

/// Open the simulator panel window.
///
/// The window is exactly 1280x720 and may not shrink below it: that is the
/// acceptance size for the touchscreen layout, where every interactive target
/// is at least 44 px (covered by a headless geometry test).
pub fn simulator_window(
    handle: SimPanelHandle,
    title: &str,
    scale: f32,
    probe_key: bool,
    shutdown: Arc<AtomicBool>,
) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([1280.0, 720.0])
            .with_title(title),
        ..Default::default()
    };
    eframe::run_native(
        title,
        options,
        Box::new(move |cc| {
            Ok(Box::new(SimPanelApp::new(
                cc, handle, scale, probe_key, shutdown,
            )))
        }),
    )
}

struct SimPanelApp {
    handle: SimPanelHandle,
    pressed: u64,
    /// When the current momentary key was pressed, so it releases after a fixed
    /// hold instead of when a frame happens to take long enough.
    press_at: Option<Instant>,
    /// Momentary keys currently held, one entry per input source: the mouse,
    /// or one per active touch id. A hold is latched when a press lands on a
    /// key rect and cleared only when that same source releases — never by
    /// per-frame polling (is_pointer_button_down_on dropped after about a
    /// second, which stopped a continuous jog while the operator was still
    /// holding the key) and never by another finger's release (the old
    /// "any release clears mouse_held" rule killed a held jog as soon as a
    /// second touch ended).
    holds: HoldTracker,
    power_pulse: Option<(bool, Instant)>,
    /// Show the simulation-only PROBE CONTACT debug key (--probe-key).
    probe_key: bool,
    last_frame: Instant,
    shutdown: Arc<AtomicBool>,
}

impl SimPanelApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        handle: SimPanelHandle,
        scale: f32,
        probe_key: bool,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        if (scale - 1.0).abs() > f32::EPSILON {
            cc.egui_ctx.set_zoom_factor(scale);
        }
        Self {
            handle,
            pressed: 0,
            press_at: None,
            holds: HoldTracker::default(),
            power_pulse: None,
            probe_key,
            last_frame: Instant::now(),
            shutdown,
        }
    }
}

impl eframe::App for SimPanelApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.shutdown.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Momentary keys pressed from the keyboard or a round button must
        // release after a fixed hold. Tracking the press instant matters:
        // comparing the *frame interval* to the hold time never fires at
        // 60 fps, so the key latched and the machine kept jogging after the
        // operator let go. Pointer/touch-held keys are released by the
        // HoldTracker from release events, not here.
        if !self.holds.is_empty() {
            // Keep frames coming so releases are observed promptly and the
            // held keys stay asserted in the panel state.
            ctx.request_repaint();
        }
        if self.pressed != 0 {
            let started = *self.press_at.get_or_insert_with(Instant::now);
            if started.elapsed() >= KEY_HOLD {
                let pressed = self.pressed;
                self.handle.set(|state| state.set_button(pressed, false));
                self.pressed = 0;
                self.press_at = None;
            } else {
                ctx.request_repaint_after(KEY_HOLD - started.elapsed());
            }
        }
        if self.pressed != 0 && self.press_at.is_none() {
            self.press_at = Some(Instant::now());
            ctx.request_repaint_after(KEY_HOLD);
        }
        if let Some((on, at)) = self.power_pulse {
            if at.elapsed().as_millis() > 120 {
                self.handle.set(|state| {
                    if on {
                        state.power_on_pulse = false;
                    } else {
                        state.power_off_pulse = false;
                    }
                });
                self.power_pulse = None;
            }
        }
        self.last_frame = Instant::now();
        let state = self.handle.snapshot();
        let layout = panel_layout(ui);
        paint_panel(
            ui,
            &layout,
            &state,
            &mut self.handle,
            &mut self.pressed,
            &mut self.holds,
            &mut self.power_pulse,
            self.probe_key,
        );
        ctx.request_repaint_after(Duration::from_millis(60));
    }
}

/// The plate, bezel and control palette, matched to the physical panel
/// (controller-documentation/img/cover.jpg): a near-black matte charcoal
/// plate, black legend keys, ivory/black guarded POWER buttons.
const SURROUND: Color32 = Color32::from_rgb(0x0f, 0x10, 0x11);
const PLATE: Color32 = Color32::from_rgb(0x19, 0x1b, 0x1d);
const BEZEL_RIM: Color32 = Color32::from_rgb(0x0c, 0x0d, 0x0e);
const EDGE_HI: Color32 = Color32::from_rgb(0x38, 0x3c, 0x40);
const KEY_BG: Color32 = Color32::from_rgb(0x17, 0x19, 0x1c);
const KEY_BORDER: Color32 = Color32::from_rgb(0x0d, 0x0f, 0x10);
const KEY_TOP_HI: Color32 = Color32::from_rgb(0x2c, 0x30, 0x33);
const IVORY: Color32 = Color32::from_rgb(0xe8, 0xe5, 0xdc);
const TEXT: Color32 = Color32::from_rgb(0xf2, 0xf3, 0xf4);
const MUTED: Color32 = Color32::from_rgb(0x9b, 0xa1, 0xa4);
const GREEN: Color32 = Color32::from_rgb(0x41, 0xe6, 0x5f);
const RED: Color32 = Color32::from_rgb(0xff, 0x3b, 0x30);
const AMBER: Color32 = Color32::from_rgb(0xff, 0xb0, 0x20);
const YELLOW: Color32 = Color32::from_rgb(0xff, 0xd6, 0x0a);
const BLUE: Color32 = Color32::from_rgb(0x4d, 0xa3, 0xff);

/// Logical layout canvas; the panel is scaled by `k` to fit the window.
const CANVAS_W: f32 = 1680.0;
const CANVAS_H: f32 = 1000.0;

/// Legend key size. 64 logical units is 46 px at the 1280x720 acceptance
/// size, above the 44 px touch-target floor.
const KEY_W: f32 = 145.0;
const KEY_H: f32 = 64.0;
const KEY_COL_PITCH: f32 = 150.0;
const KEY_ROW_PITCH: f32 = 70.0;
const KEY_TOP: f32 = 500.0;
const MIDDLE_X: f32 = 540.0;
const RIGHT_X: f32 = 1050.0;
/// The left jog cluster uses a wider column pitch than the other grids.
const LEFT_X: [f32; 3] = [28.0, 188.0, 348.0];

/// Guarded POWER pushbuttons: square, 66 logical units per side (47.5 px at
/// 1280x720).
const POWER_KEY_SIDE: f32 = 66.0;
const POWER_ON_AT: (f32, f32) = (30.0, 122.0);
const POWER_OFF_AT: (f32, f32) = (108.0, 122.0);

/// E-stop centre. The power keys are centred on the same row (155) and the
/// OFF key (right edge 174) now leaves 22 logical units to the collar: at 5
/// the two groups read as one cluster.
const ESTOP_AT: (f32, f32) = (262.0, 155.0);
const ESTOP_HIT: f32 = 132.0;
const MODE_KNOB: (f32, f32, f32) = (510.0, 145.0, 72.0);
const FEED_KNOB: (f32, f32, f32) = (800.0, 145.0, 48.0);
const RAPID_KNOB: (f32, f32, f32) = (1030.0, 145.0, 48.0);
const SPINDLE_KNOB: (f32, f32, f32) = (1260.0, 145.0, 48.0);
/// Knob interaction padding beyond the radius (override knobs only).
const KNOB_HIT_PAD: f32 = 14.0;
const PROTECT_AT: (f32, f32) = (1530.0, 145.0);
const PROTECT_HIT: f32 = 110.0;
const CYCLE_START_AT: (f32, f32) = (123.0, 870.0);
const CYCLE_STOP_AT: (f32, f32) = (235.0, 870.0);
const CYCLE_R: f32 = 48.0;
/// Chrome mounting ring around the cycle buttons; part of the hit target.
const CYCLE_RING: f32 = 8.0;
/// Illuminated cycle-status plates, one per cycle button. They sit in the
/// button row, to the right of the buttons and centred on the same midline
/// (the two buttons stay centred under the power/E-stop cluster).
const CYCLE_PLATE_SIZE: (f32, f32) = (52.0, 30.0);
const CYCLE_START_PLATE_AT: (f32, f32) = (335.0, 870.0);
const CYCLE_STOP_PLATE_AT: (f32, f32) = (397.0, 870.0);
const PROBE_KEY_AT: (f32, f32) = (1507.0, 850.0);

/// Sentinel mask for the PROBE CONTACT key in the hold tracker. The probe is
/// not a `buttons` bit, so this all-ones value never mixes into
/// `state.buttons` (every real button mask is a single bit).
const PROBE_HOLD: u64 = u64::MAX;

// Key matrix, matching the machine panel layout.
// Same layout and arrow directions as svg/sim-panel.svg:
//   B ▲   SAFE DOOR   B ▼
//   A ▲   Z ▲   Y ▲
//   X ◀   RAPID   X ▶
//   Y ▼   Z ▼   A ▼
// `<` `>` `^` `v` are painted as triangles (see `paint_key_arrow`), so the
// arrows never depend on font glyph coverage. Rows 2 and 4 previously
// shared one JOG_YN, which left Y+ unreachable, and Z/A had their
// directions swapped.
const LEFT_KEYS: [(&str, Option<&str>, u64, u64); 12] = [
    ("B", Some("^"), buttons::JOG_BP, crate::leds::JOG_BP),
    ("SAFE DOOR", None, buttons::SAFE_DOOR, crate::leds::SAFE_DOOR),
    ("B", Some("v"), buttons::JOG_BN, crate::leds::JOG_BN),
    ("A", Some("^"), buttons::JOG_AP, crate::leds::JOG_AP),
    ("Z", Some("^"), buttons::JOG_ZP, crate::leds::JOG_ZP),
    ("Y", Some("^"), buttons::JOG_YP, crate::leds::JOG_YP),
    ("X", Some("<"), buttons::JOG_XN, crate::leds::JOG_XN),
    ("RAPID", None, buttons::RAPID, crate::leds::RAPID),
    ("X", Some(">"), buttons::JOG_XP, crate::leds::JOG_XP),
    ("Y", Some("v"), buttons::JOG_YN, crate::leds::JOG_YN),
    ("Z", Some("v"), buttons::JOG_ZN, crate::leds::JOG_ZN),
    ("A", Some("v"), buttons::JOG_AN, crate::leds::JOG_AN),
];

const MIDDLE_KEYS: [(&str, u64, Option<u64>); 15] = [
    ("MPG SIM", buttons::MPG_SIM, Some(crate::leds::MPG_SIM)),
    ("S.B.K.", buttons::SBK, Some(crate::leds::SBK)),
    ("B.D.T.", buttons::BDT, Some(crate::leds::BDT)),
    ("M.LK", buttons::MLK, Some(crate::leds::MLK)),
    ("OPT", buttons::OPT, Some(crate::leds::OPT)),
    ("D.R.N.", buttons::DRN, Some(crate::leds::DRN)),
    ("WORKLAMP", buttons::WORKLAMP, Some(crate::leds::WORKLAMP)),
    ("AIR BLOW", buttons::AIR_BLOW, Some(crate::leds::AIR_BLOW)),
    ("PT.LA", buttons::PT_LA, Some(crate::leds::PT_LA)),
    ("COOLANT", buttons::COOLANT, Some(crate::leds::COOLANT)),
    ("SPD.CTS", buttons::SPD_CTS, Some(crate::leds::SPD_CTS)),
    ("FLUSH", buttons::FLUSH, Some(crate::leds::FLUSH)),
    ("LUB", buttons::LUB, Some(crate::leds::LUB)),
    ("CORRIDOR", buttons::CORRIDOR, Some(crate::leds::CORRIDOR)),
    ("K1", buttons::K1, Some(crate::leds::K1)),
];

const RIGHT_KEYS: [(&str, u64, Option<u64>); 15] = [
    ("SPD.ORT", buttons::SPD_ORI, Some(crate::leds::SPD_ORI)),
    ("MAG CW", buttons::MAG_CW, Some(crate::leds::MAG_CW)),
    ("MAG CCW", buttons::MAG_CCW, Some(crate::leds::MAG_CCW)),
    ("SPD.CW", buttons::SPD_CW, Some(crate::leds::SPD_CW)),
    ("SPD.SP", buttons::SPD_SP, Some(crate::leds::SPD_SP)),
    ("SPD.CCW", buttons::SPD_CCW, Some(crate::leds::SPD_CCW)),
    ("CHIP CW", buttons::CHIP_CW, Some(crate::leds::CHIP_CW)),
    ("CHIP SP", buttons::CHIP_SP, Some(crate::leds::CHIP_SP)),
    ("CHIP CCW", buttons::CHIP_CCW, Some(crate::leds::CHIP_CCW)),
    ("CHUCK", buttons::CHUCK, Some(crate::leds::CHUCK)),
    ("POT DN", buttons::POT_DN, Some(crate::leds::POT_DN)),
    ("ARM MAN", buttons::ARM_MAN, Some(crate::leds::ARM_MAN)),
    ("K2", buttons::K2, Some(crate::leds::K2)),
    ("K3", buttons::K3, Some(crate::leds::K3)),
    ("K4", buttons::K4, Some(crate::leds::K4)),
];

/// What is holding a momentary key down: the mouse pointer is a single
/// source, every active touch id is its own source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldSource {
    Mouse,
    Touch(egui::TouchId),
}

/// Momentary-key hold state, matched from raw input events.
///
/// egui-winit only translates the FIRST active touch into pointer events, so
/// hold tracking cannot rely on the pointer alone: extra fingers arrive as
/// raw `Event::Touch` and would be invisible here. Matching press and release
/// events per source against the key rects means a held key survives taps
/// from other fingers, and a hold still ends exactly when its own press
/// ends. A finger that slides off its key keeps it, matching egui's pointer
/// capture for a held mouse drag.
#[derive(Default)]
struct HoldTracker {
    holds: Vec<(HoldSource, u64)>,
}

impl HoldTracker {
    fn is_empty(&self) -> bool {
        self.holds.is_empty()
    }

    fn is_held(&self, mask: u64) -> bool {
        self.holds.iter().any(|(_, held)| *held == mask)
    }

    fn press(&mut self, source: HoldSource, mask: u64) {
        if !self
            .holds
            .iter()
            .any(|(held_source, held)| *held_source == source && *held == mask)
        {
            self.holds.push((source, mask));
        }
    }

    fn release(&mut self, source: HoldSource) {
        self.holds.retain(|(held_source, _)| *held_source != source);
    }

    /// Fold this frame's input events into the hold state. `rects` are the
    /// momentary key rects in screen coordinates.
    fn process(&mut self, ui: &egui::Ui, rects: &[(u64, Rect)]) {
        let hit = |pos: Pos2| {
            rects
                .iter()
                .find(|(_, rect)| rect.contains(pos))
                .map(|(mask, _)| *mask)
        };
        ui.input(|input| {
            for event in &input.events {
                match *event {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        ..
                    } => {
                        if pressed {
                            if let Some(mask) = hit(pos) {
                                self.press(HoldSource::Mouse, mask);
                            }
                        } else {
                            self.release(HoldSource::Mouse);
                        }
                    }
                    egui::Event::PointerGone => self.release(HoldSource::Mouse),
                    egui::Event::Touch { id, phase, pos, .. } => match phase {
                        egui::TouchPhase::Start => {
                            if let Some(mask) = hit(pos) {
                                self.press(HoldSource::Touch(id), mask);
                            }
                        }
                        egui::TouchPhase::Move => {}
                        egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                            self.release(HoldSource::Touch(id));
                        }
                    },
                    _ => {}
                }
            }
        });
    }
}

/// Every interactive target on the panel, in screen coordinates. Computed in
/// one place so painting, hold tracking and the touch-target size test all
/// see the same rects.
struct Interactives {
    momentary: Vec<(u64, Rect)>,
    power_on: Rect,
    power_off: Rect,
    estop: Rect,
    protect: Rect,
    mode_knob: Rect,
    feed: Rect,
    rapid: Rect,
    spindle: Rect,
    cycle_start: Rect,
    cycle_stop: Rect,
}

fn left_key_rects(l: &PanelLayout) -> Vec<Rect> {
    (0..LEFT_KEYS.len())
        .map(|index| {
            let x = LEFT_X[index % 3];
            let y = KEY_TOP + (index / 3) as f32 * KEY_ROW_PITCH;
            l.r(x, y, KEY_W, KEY_H)
        })
        .collect()
}

fn key_grid_rects(l: &PanelLayout, x0: f32, columns: usize, count: usize) -> Vec<Rect> {
    (0..count)
        .map(|index| {
            let x = x0 + (index % columns) as f32 * KEY_COL_PITCH;
            let y = KEY_TOP + (index / columns) as f32 * KEY_ROW_PITCH;
            l.r(x, y, KEY_W, KEY_H)
        })
        .collect()
}

fn knob_hit_rect(l: &PanelLayout, knob: (f32, f32, f32)) -> Rect {
    let (cx, cy, r) = knob;
    Rect::from_center_size(
        l.p(cx, cy),
        Vec2::splat(2.0 * (r + KNOB_HIT_PAD) * l.k),
    )
}

fn interactives(l: &PanelLayout, probe_key: bool) -> Interactives {
    let mut momentary: Vec<(u64, Rect)> = Vec::new();
    for (rect, (_, _, mask, _)) in left_key_rects(l).iter().zip(LEFT_KEYS.iter()) {
        momentary.push((*mask, *rect));
    }
    for (rect, (_, mask, _)) in key_grid_rects(l, MIDDLE_X, 3, MIDDLE_KEYS.len())
        .iter()
        .zip(MIDDLE_KEYS.iter())
    {
        momentary.push((*mask, *rect));
    }
    for (rect, (_, mask, _)) in key_grid_rects(l, RIGHT_X, 3, RIGHT_KEYS.len())
        .iter()
        .zip(RIGHT_KEYS.iter())
    {
        momentary.push((*mask, *rect));
    }
    if probe_key {
        momentary.push((
            PROBE_HOLD,
            l.r(PROBE_KEY_AT.0, PROBE_KEY_AT.1, KEY_W, KEY_H),
        ));
    }
    Interactives {
        momentary,
        power_on: l.r(POWER_ON_AT.0, POWER_ON_AT.1, POWER_KEY_SIDE, POWER_KEY_SIDE),
        power_off: l.r(
            POWER_OFF_AT.0,
            POWER_OFF_AT.1,
            POWER_KEY_SIDE,
            POWER_KEY_SIDE,
        ),
        estop: Rect::from_center_size(l.p(ESTOP_AT.0, ESTOP_AT.1), Vec2::splat(ESTOP_HIT * l.k)),
        protect: Rect::from_center_size(
            l.p(PROTECT_AT.0, PROTECT_AT.1),
            Vec2::splat(PROTECT_HIT * l.k),
        ),
        mode_knob: Rect::from_center_size(
            l.p(MODE_KNOB.0, MODE_KNOB.1),
            Vec2::splat(2.0 * MODE_KNOB.2 * l.k),
        ),
        feed: knob_hit_rect(l, FEED_KNOB),
        rapid: knob_hit_rect(l, RAPID_KNOB),
        spindle: knob_hit_rect(l, SPINDLE_KNOB),
        cycle_start: Rect::from_center_size(
            l.p(CYCLE_START_AT.0, CYCLE_START_AT.1),
            Vec2::splat(2.0 * (CYCLE_R + CYCLE_RING) * l.k),
        ),
        cycle_stop: Rect::from_center_size(
            l.p(CYCLE_STOP_AT.0, CYCLE_STOP_AT.1),
            Vec2::splat(2.0 * (CYCLE_R + CYCLE_RING) * l.k),
        ),
    }
}

#[derive(Clone, Copy)]
struct PanelLayout {
    origin: Pos2,
    k: f32,
}

impl PanelLayout {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x * self.k, y * self.k)
    }
    fn r(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(self.p(x, y), Vec2::new(w * self.k, h * self.k))
    }
    fn font(&self, size: f32) -> FontId {
        FontId::proportional(size * self.k)
    }
}

fn layout_for_rect(avail: Rect) -> PanelLayout {
    let k = (avail.width() / CANVAS_W)
        .min(avail.height() / CANVAS_H)
        .max(0.2);
    let size = Vec2::new(CANVAS_W * k, CANVAS_H * k);
    PanelLayout {
        origin: avail.center() - size / 2.0,
        k,
    }
}

fn panel_layout(ui: &egui::Ui) -> PanelLayout {
    layout_for_rect(ui.available_rect_before_wrap())
}

fn label(
    painter: &egui::Painter,
    l: &PanelLayout,
    x: f32,
    y: f32,
    size: f32,
    text: &str,
    color: Color32,
    align: Align2,
) {
    painter.text(l.p(x, y), align, text, l.font(size), color);
}

/// A filled circle shaded from `inner` at the center to `outer` at the rim,
/// so pushbuttons, lamps and knob caps read as domed instead of flat.
fn paint_dome(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    inner: Color32,
    outer: Color32,
) {
    const SEGMENTS: u32 = 40;
    let mut mesh = egui::epaint::Mesh::default();
    mesh.colored_vertex(center, inner);
    for i in 0..=SEGMENTS {
        let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(center + Vec2::new(a.cos(), a.sin()) * radius, outer);
    }
    for i in 0..SEGMENTS {
        mesh.add_triangle(0, 1 + i, 2 + i);
    }
    painter.add(egui::Shape::from(mesh));
}

/// A filled rect shaded from `top` to `bottom`: a cheap bevel for the
/// square guarded pushbuttons.
fn paint_vgrad_rect(painter: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = egui::epaint::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::from(mesh));
}

/// A small specular highlight at ten o'clock, for domed parts.
fn paint_specular(painter: &egui::Painter, center: Pos2, radius: f32, alpha: u8) {
    painter.circle_filled(
        center + Vec2::new(-0.35 * radius, -0.38 * radius),
        radius * 0.22,
        Color32::from_white_alpha(alpha),
    );
}

/// A panel lamp in a dark metal bezel: raised ring, colored lens when lit.
fn lamp(
    painter: &egui::Painter,
    l: &PanelLayout,
    x: f32,
    y: f32,
    r: f32,
    on: bool,
    color: Color32,
) {
    let center = l.p(x, y);
    let radius = r * l.k;
    paint_dome(
        painter,
        center,
        radius,
        Color32::from_rgb(0x2e, 0x32, 0x35),
        Color32::from_rgb(0x0b, 0x0d, 0x0e),
    );
    painter.circle_stroke(
        center,
        radius,
        Stroke::new(1.0 * l.k, Color32::from_rgb(0x44, 0x49, 0x4d)),
    );
    let lens = radius * 0.62;
    if on {
        paint_dome(painter, center, lens, color, color.gamma_multiply(0.5));
        painter.circle_stroke(
            center,
            lens,
            Stroke::new(1.0 * l.k, color.gamma_multiply(1.4)),
        );
        paint_specular(painter, center, lens, 90);
    } else {
        paint_dome(
            painter,
            center,
            lens,
            Color32::from_rgb(0x2b, 0x2f, 0x31),
            Color32::from_rgb(0x16, 0x18, 0x1a),
        );
    }
}

/// Draw a direction arrow as a filled triangle. `"<`, `">`, `"^"` and
/// `"v"` are the four directions; anything else is drawn as small text.
fn paint_key_arrow(
    painter: &egui::Painter,
    l: &PanelLayout,
    center: Pos2,
    code: &str,
    color: Color32,
) {
    let s = 9.0 * l.k;
    let points = match code {
        "^" => vec![
            center + Vec2::new(0.0, -s),
            center + Vec2::new(-s * 0.85, s * 0.6),
            center + Vec2::new(s * 0.85, s * 0.6),
        ],
        "v" => vec![
            center + Vec2::new(0.0, s),
            center + Vec2::new(-s * 0.85, -s * 0.6),
            center + Vec2::new(s * 0.85, -s * 0.6),
        ],
        "<" => vec![
            center + Vec2::new(-s, 0.0),
            center + Vec2::new(s * 0.6, -s * 0.85),
            center + Vec2::new(s * 0.6, s * 0.85),
        ],
        ">" => vec![
            center + Vec2::new(s, 0.0),
            center + Vec2::new(-s * 0.6, -s * 0.85),
            center + Vec2::new(-s * 0.6, s * 0.85),
        ],
        other => {
            painter.text(center, Align2::CENTER_CENTER, other, l.font(9.0), color);
            return;
        }
    };
    painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
}

/// The cap of a compact black legend key: face, thin dark border, top edge
/// highlight and the green LED dot.
fn paint_key_cap(
    ui: &egui::Ui,
    l: &PanelLayout,
    rect: Rect,
    id: egui::Id,
    fill: Color32,
    led_on: bool,
    down: bool,
) {
    // Hover-only sense: the hold state comes from the HoldTracker, the key
    // cap just lights up under the mouse.
    let resp = ui.interact(rect, id, Sense::hover());
    let fill = if down {
        fill.gamma_multiply(1.45)
    } else if resp.hovered() {
        fill.gamma_multiply(1.22)
    } else {
        fill
    };
    ui.painter().rect(
        rect,
        3.0 * l.k,
        fill,
        Stroke::new(1.0 * l.k, KEY_BORDER),
        egui::StrokeKind::Inside,
    );
    // Top inner highlight: the molded edge of the key cap.
    ui.painter().line_segment(
        [
            rect.left_top() + Vec2::new(3.0 * l.k, 1.5 * l.k),
            rect.right_top() + Vec2::new(-3.0 * l.k, 1.5 * l.k),
        ],
        Stroke::new(1.0 * l.k, KEY_TOP_HI),
    );
    let led = rect.left_top() + Vec2::new(12.0 * l.k, 12.0 * l.k);
    ui.painter().circle_filled(
        led,
        3.5 * l.k,
        if led_on {
            GREEN
        } else {
            Color32::from_rgb(0x38, 0x3e, 0x41)
        },
    );
    if led_on {
        ui.painter().circle_stroke(
            led,
            5.2 * l.k,
            Stroke::new(1.0 * l.k, GREEN.gamma_multiply(0.55)),
        );
    }
}

/// One compact black legend key with a white small-caps legend. `down` comes
/// from the HoldTracker: the hold state is settled from the raw input events
/// before painting (see `paint_panel`), so the key itself only paints.
#[allow(clippy::too_many_arguments)]
fn panel_key(
    ui: &egui::Ui,
    l: &PanelLayout,
    rect: Rect,
    label_text: &str,
    sub: Option<&str>,
    fill: Color32,
    led_on: bool,
    down: bool,
    mask: u64,
) {
    paint_key_cap(
        ui,
        l,
        rect,
        ui.scope_id().with(("panel-key", mask)),
        fill,
        led_on,
        down,
    );
    let cx = rect.center().x;
    let cy = if sub.is_some() {
        rect.top() + rect.height() * 0.40
    } else {
        rect.center().y
    };
    ui.painter().text(
        Pos2::new(cx, cy),
        Align2::CENTER_CENTER,
        label_text,
        l.font(14.0),
        TEXT,
    );
    if let Some(sub) = sub {
        paint_key_arrow(
            ui.painter(),
            l,
            Pos2::new(cx, rect.top() + rect.height() * 0.72),
            sub,
            TEXT,
        );
    }
}

/// A guarded POWER pushbutton: a translucent protective frame around a square
/// illuminated button, like the clear guards on the physical panel.
#[allow(clippy::too_many_arguments)]
fn power_key(
    ui: &egui::Ui,
    l: &PanelLayout,
    rect: Rect,
    label_text: &str,
    base: Color32,
    lit: Option<Color32>,
    id: &'static str,
) -> bool {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    // The clear guard reads as a slightly lighter translucent frame.
    let guard = rect.expand(6.0 * l.k);
    ui.painter().rect(
        guard,
        4.0 * l.k,
        Color32::from_white_alpha(9),
        Stroke::new(1.5 * l.k, Color32::from_white_alpha(55)),
        egui::StrokeKind::Inside,
    );
    let fill = lit.unwrap_or(base);
    let fill = if resp.hovered() {
        fill.gamma_multiply(1.15)
    } else {
        fill
    };
    paint_vgrad_rect(
        ui.painter(),
        rect,
        fill.gamma_multiply(1.12),
        fill.gamma_multiply(0.82),
    );
    ui.painter().rect_stroke(
        rect,
        3.0 * l.k,
        Stroke::new(1.0 * l.k, KEY_BORDER),
        egui::StrokeKind::Inside,
    );
    if let Some(lit_color) = lit {
        // Illuminated buttons spill a little light onto the guard.
        ui.painter().rect_stroke(
            rect.expand(2.0 * l.k),
            3.0 * l.k,
            Stroke::new(2.0 * l.k, lit_color.gamma_multiply(0.6)),
            egui::StrokeKind::Inside,
        );
    }
    let dark_text = lit.is_some() || base == IVORY;
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label_text,
        l.font(13.0),
        if dark_text {
            Color32::from_rgb(0x16, 0x18, 0x1a)
        } else {
            TEXT
        },
    );
    resp.clicked()
}

/// A large round pushbutton with a chrome mounting ring (CYCLE START/STOP).
/// The state lamp is a separate illuminated plate drawn by
/// `draw_cycle_plate` above the button, as on the cabinet; the button
/// carries no LED.
#[allow(clippy::too_many_arguments)]
fn round_key(
    ui: &egui::Ui,
    l: &PanelLayout,
    at: (f32, f32),
    rect: Rect,
    label_text: &str,
    inner: Color32,
    outer: Color32,
    id: &'static str,
) -> bool {
    let center = l.p(at.0, at.1);
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    // Chrome ring.
    paint_dome(
        ui.painter(),
        center,
        (CYCLE_R + CYCLE_RING) * l.k,
        Color32::from_rgb(0xd4, 0xd8, 0xdc),
        Color32::from_rgb(0x62, 0x67, 0x6c),
    );
    ui.painter().circle_stroke(
        center,
        (CYCLE_R + CYCLE_RING) * l.k,
        Stroke::new(1.0 * l.k, Color32::from_rgb(0x38, 0x3c, 0x40)),
    );
    let brighten = if resp.hovered() { 1.08 } else { 1.0 };
    paint_dome(
        ui.painter(),
        center,
        CYCLE_R * l.k,
        inner.gamma_multiply(brighten),
        outer.gamma_multiply(brighten),
    );
    ui.painter().circle_stroke(
        center,
        CYCLE_R * l.k,
        Stroke::new(2.0 * l.k, Color32::from_rgb(0x0c, 0x0e, 0x0f)),
    );
    label(
        ui.painter(),
        l,
        at.0,
        at.1 + CYCLE_R + 22.0,
        13.0,
        label_text,
        TEXT,
        Align2::CENTER_CENTER,
    );
    resp.clicked()
}

/// What a cycle-status plate stands for: the start plate carries a vertical
/// pill in its loop, the stop plate a filled dot, matching the cabinet lamps.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CyclePlate {
    Start,
    Stop,
}

/// Illuminated cycle-status plate above a cycle button: a smoked lamp window
/// with a loop-and-arrows glyph. Unlit it stays dim grey; lit it glows in its
/// own colour (white for start, red for stop).
fn draw_cycle_plate(
    painter: &egui::Painter,
    l: &PanelLayout,
    cx: f32,
    cy: f32,
    kind: CyclePlate,
    lit: bool,
) {
    let (w, h) = CYCLE_PLATE_SIZE;
    let rect = l.r(cx - w * 0.5, cy - h * 0.5, w, h);
    let (lit_color, dim) = match kind {
        CyclePlate::Start => (
            Color32::from_rgb(0xf2, 0xf3, 0xf4),
            Color32::from_rgb(0x55, 0x59, 0x5c),
        ),
        CyclePlate::Stop => (RED, Color32::from_rgb(0x5a, 0x2a, 0x28)),
    };
    let ink = if lit { lit_color } else { dim };
    painter.rect(
        rect,
        3.0 * l.k,
        Color32::from_rgb(0x0b, 0x0c, 0x0d),
        Stroke::new(1.0 * l.k, Color32::from_rgb(0x24, 0x27, 0x29)),
        egui::StrokeKind::Inside,
    );
    if lit {
        painter.rect_stroke(
            rect.expand(1.0 * l.k),
            4.0 * l.k,
            Stroke::new(2.0 * l.k, lit_color.gamma_multiply(0.30)),
            egui::StrokeKind::Outside,
        );
    }
    // Loop glyph: rounded rectangle with arrowheads following it clockwise.
    let g = rect.shrink2(Vec2::new(9.0 * l.k, 6.0 * l.k));
    painter.rect_stroke(
        g,
        2.0 * l.k,
        Stroke::new(1.8 * l.k, ink),
        egui::StrokeKind::Inside,
    );
    let s = 3.0 * l.k;
    let arrow = |tip: Pos2, dir: Vec2| {
        let back = tip - dir * s;
        let perp = Vec2::new(-dir.y, dir.x) * s;
        painter.add(egui::Shape::convex_polygon(
            vec![tip, back + perp, back - perp],
            ink,
            Stroke::NONE,
        ));
    };
    arrow(g.right_top() + Vec2::new(0.0, 1.5 * l.k), Vec2::new(1.0, 0.0));
    arrow(g.left_bottom() - Vec2::new(0.0, 1.5 * l.k), Vec2::new(-1.0, 0.0));
    let c = rect.center();
    match kind {
        CyclePlate::Start => {
            let pill = Rect::from_center_size(c, Vec2::new(5.0 * l.k, 15.0 * l.k));
            painter.rect(pill, 2.5 * l.k, ink, Stroke::NONE, egui::StrokeKind::Inside);
        }
        CyclePlate::Stop => {
            painter.circle_filled(c, 8.0 * l.k, ink);
            painter.circle_filled(c, 3.0 * l.k, Color32::from_rgb(0x0b, 0x0c, 0x0d));
        }
    }
}

/// A knurled rotary knob: serrated grip ring, domed black cap, metal hub and
/// a white pointer line at `angle` (egui convention: 0 = +x, positive is
/// clockwise because y points down).
fn paint_knob(painter: &egui::Painter, l: &PanelLayout, center: Pos2, r: f32, angle: f32) {
    let radius = r * l.k;
    // Knurled grip: alternating ridges around the rim.
    const RIDGES: u32 = 48;
    for i in 0..RIDGES {
        let a = i as f32 * std::f32::consts::TAU / RIDGES as f32;
        let dir = Vec2::new(a.cos(), a.sin());
        let shade = if i % 2 == 0 {
            Color32::from_rgb(0x3a, 0x3e, 0x42)
        } else {
            Color32::from_rgb(0x0d, 0x0f, 0x10)
        };
        painter.line_segment(
            [center + dir * radius * 0.86, center + dir * radius * 0.99],
            Stroke::new(2.4 * l.k, shade),
        );
    }
    paint_dome(
        painter,
        center,
        radius * 0.88,
        Color32::from_rgb(0x26, 0x29, 0x2c),
        Color32::from_rgb(0x0f, 0x11, 0x12),
    );
    painter.circle_stroke(
        center,
        radius,
        Stroke::new(2.0 * l.k, Color32::from_rgb(0x08, 0x09, 0x0a)),
    );
    // Pointer line from the hub to the rim.
    let dir = Vec2::new(angle.cos(), angle.sin());
    painter.line_segment(
        [center + dir * radius * 0.30, center + dir * radius * 0.80],
        Stroke::new(4.0 * l.k, Color32::WHITE),
    );
    // Metal hub.
    paint_dome(
        painter,
        center,
        radius * 0.26,
        Color32::from_rgb(0xc2, 0xc7, 0xcc),
        Color32::from_rgb(0x58, 0x5d, 0x62),
    );
    painter.circle_stroke(
        center,
        radius * 0.26,
        Stroke::new(1.0 * l.k, Color32::from_rgb(0x2c, 0x30, 0x33)),
    );
}

/// Value dial sweep: the minimum sits at 7:30, the maximum at 4:30 and the
/// gap is at the bottom, matching the printed scales on the physical panel.
fn value_angle(norm: f32) -> f32 {
    (135.0 + norm.clamp(0.0, 1.0) * 270.0).to_radians()
}

/// How long a momentary panel key (keyboard shortcut or round button) stays
/// asserted. Long enough for halui to sample it, short enough to feel like a
/// press.
const KEY_HOLD: Duration = Duration::from_millis(120);

/// How long `cnc_panel.power_on` stays asserted. That pin is wired to
/// `halui.estop.reset`, which needs a pulse rather than a held level.
const POWER_PULSE: Duration = Duration::from_millis(150);

fn mode_index(mode: PanelMode) -> usize {
    match mode {
        PanelMode::Edit => 0,
        PanelMode::Auto => 1,
        PanelMode::Mdi => 2,
        PanelMode::Jog => 3,
        PanelMode::Inc => 4,
        PanelMode::Mpg => 5,
        PanelMode::Home => 6,
        PanelMode::ProgHome => 7,
    }
}

fn mode_angle(index: usize) -> f32 {
    // Angles point at the printed label for each detent.
    const ANGLES: [f32; 8] = [119.0, 143.0, 180.0, -126.0, 0.0, -54.0, 37.0, 61.0];
    ANGLES[index.min(7)].to_radians()
}

fn mode_from_angle(degrees: f32) -> PanelMode {
    let mut best = (f32::INFINITY, PanelMode::Auto);
    for mode in PanelMode::ALL {
        let label = mode_angle(mode_index(mode)).to_degrees();
        let mut delta = (degrees - label + 540.0) % 360.0 - 180.0;
        if delta < -180.0 {
            delta += 360.0;
        }
        if delta.abs() < best.0 {
            best = (delta.abs(), mode);
        }
    }
    best.1
}

fn draw_mode_knob(ui: &egui::Ui, l: &PanelLayout, state: &SimPanelState, rect: Rect) {
    let (cx, cy, r) = MODE_KNOB;
    let center = l.p(cx, cy);
    let knob = ui.interact(
        rect,
        ui.scope_id().with("mode-knob"),
        Sense::click_and_drag(),
    );
    // Printed scale: a tick ring on the plate and the eight mode names around
    // the arc, the active one lit. The names used to be separate click targets
    // (88x24 logical, 17 px tall at 1280x720 — far under the 44 px touch
    // floor); the knob itself keeps click-to-step, drag-to-angle and scroll.
    for i in 0..=16 {
        let a = (-135.0 + i as f32 * 270.0 / 16.0).to_radians();
        let dir = Vec2::new(a.cos(), a.sin());
        let p0 = center + dir * (r + 8.0) * l.k;
        let p1 = center + dir * (r + 16.0) * l.k;
        ui.painter().line_segment([p0, p1], Stroke::new(1.0 * l.k, MUTED));
    }
    for mode in PanelMode::ALL {
        let a = mode_angle(mode_index(mode));
        let pos = center + Vec2::new(a.cos(), a.sin()) * (r + 28.0) * l.k;
        ui.painter().text(
            pos,
            Align2::CENTER_CENTER,
            mode.label(),
            l.font(11.0),
            if state.mode == mode { BLUE } else { TEXT },
        );
    }
    paint_knob(ui.painter(), l, center, r, mode_angle(mode_index(state.mode)));

    if knob.dragged() || knob.clicked() {
        if let Some(pointer) = ui.ctx().pointer_interact_pos() {
            let offset = pointer - center;
            let mode = if knob.clicked() {
                // Clicking the knob advances one detent; dragging selects by
                // angle. This keeps the dial usable at every size.
                let next = (mode_index(state.mode) + 1) % PanelMode::ALL.len();
                PanelMode::ALL[next]
            } else {
                mode_from_angle(offset.y.atan2(offset.x).to_degrees())
            };
            if mode != state.mode {
                ui.ctx().data_mut(|data| {
                    data.insert_temp(ui.scope_id().with("linc-panel-mode"), Some(mode))
                });
                ui.ctx().request_repaint();
            }
        }
    }
    if knob.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        let scroll = ui.input(|input| input.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let step = if scroll > 0.0 { 1 } else { -1 };
            let index = (mode_index(state.mode) as i32 + step).rem_euclid(8) as usize;
            let mode = PanelMode::ALL[index];
            ui.ctx().data_mut(|data| {
                data.insert_temp(ui.scope_id().with("linc-panel-mode"), Some(mode))
            });
            ui.ctx().request_repaint();
        }
    }
}

/// Feed override scale numerals, as (fraction of the dial sweep, text).
const FEED_NUMERALS: [(f32, &str); 9] = [
    (0.0, "0"),
    (0.13333, "20"),
    (0.26667, "40"),
    (0.4, "60"),
    (0.53333, "80"),
    (0.66667, "100"),
    (0.8, "120"),
    (0.93333, "140"),
    (1.0, "150"),
];

/// Rapid override scale: the physical dial marks F0/25/50/100.
const RAPID_NUMERALS: [(f32, &str); 4] = [(0.0, "F0"), (0.25, "25"), (0.5, "50"), (1.0, "100")];

/// Spindle override scale numerals, as (fraction of the dial sweep, text).
const SPINDLE_NUMERALS: [(f32, &str); 8] = [
    (0.0, "50"),
    (0.14286, "60"),
    (0.28571, "70"),
    (0.42857, "80"),
    (0.57143, "90"),
    (0.71429, "100"),
    (0.85714, "110"),
    (1.0, "120"),
];

#[allow(clippy::too_many_arguments)]
fn draw_override_knob(
    ui: &egui::Ui,
    l: &PanelLayout,
    knob: (f32, f32, f32),
    rect: Rect,
    min: f32,
    max: f32,
    value: f32,
    title: &str,
    id: &'static str,
    numerals: &[(f32, &str)],
) -> Option<f32> {
    let (cx, cy, r) = knob;
    let center = l.p(cx, cy);
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click_and_drag());
    let mut next = value;
    if resp.dragged() {
        next += -resp.drag_delta().y / (120.0 * l.k) * (max - min);
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            next += scroll / (240.0 * l.k) * (max - min);
        }
    }
    next = next.clamp(min, max);
    // Printed scale: tick ring plus numerals on the plate.
    for i in 0..=10 {
        let a = value_angle(i as f32 / 10.0);
        let dir = Vec2::new(a.cos(), a.sin());
        let p0 = center + dir * (r + 6.0) * l.k;
        let p1 = center + dir * (r + 14.0) * l.k;
        ui.painter().line_segment([p0, p1], Stroke::new(1.0 * l.k, MUTED));
    }
    for (norm, text) in numerals {
        let a = value_angle(*norm);
        let pos = center + Vec2::new(a.cos(), a.sin()) * (r + 27.0) * l.k;
        ui.painter()
            .text(pos, Align2::CENTER_CENTER, text, l.font(10.0), MUTED);
    }
    let norm = ((next - min) / (max - min)).clamp(0.0, 1.0);
    paint_knob(ui.painter(), l, center, r, value_angle(norm));
    label(
        ui.painter(),
        l,
        cx,
        cy - r - 44.0,
        13.0,
        title,
        MUTED,
        Align2::CENTER_CENTER,
    );
    label(
        ui.painter(),
        l,
        cx,
        cy + r + 34.0,
        12.0,
        &format!("{:.0}%", next * 100.0),
        TEXT,
        Align2::CENTER_CENTER,
    );
    if (next - value).abs() > 0.0001 {
        Some(next)
    } else {
        None
    }
}

fn draw_7seg_digit(
    painter: &egui::Painter,
    l: &PanelLayout,
    rect: Rect,
    ch: char,
    on_color: Color32,
    off_color: Color32,
) {
    const SEGMENTS: [[bool; 7]; 10] = [
        [true, true, true, true, true, true, false],
        [false, true, true, false, false, false, false],
        [true, true, false, true, true, false, true],
        [true, true, true, true, false, false, true],
        [false, true, true, false, false, true, true],
        [true, false, true, true, false, true, true],
        [true, false, true, true, true, true, true],
        [true, true, true, false, false, false, false],
        [true, true, true, true, true, true, true],
        [true, true, true, true, false, true, true],
    ];
    let segs = if ch == '-' {
        [false, false, false, false, false, false, true]
    } else {
        SEGMENTS[ch.to_digit(10).unwrap_or(0) as usize]
    };
    let seg = |a: (f32, f32), b: (f32, f32)| {
        let p0 = rect.min + Vec2::new(a.0 * rect.width(), a.1 * rect.height());
        let p1 = rect.min + Vec2::new(b.0 * rect.width(), b.1 * rect.height());
        [p0, p1]
    };
    let coords = [
        ((0.08, 0.08), (0.92, 0.08)),
        ((0.92, 0.10), (0.92, 0.47)),
        ((0.92, 0.53), (0.92, 0.90)),
        ((0.08, 0.92), (0.92, 0.92)),
        ((0.08, 0.53), (0.08, 0.90)),
        ((0.08, 0.10), (0.08, 0.47)),
        ((0.08, 0.50), (0.92, 0.50)),
    ];
    for (index, (a, b)) in coords.iter().enumerate() {
        let color = if segs[index] { on_color } else { off_color };
        painter.line_segment(seg(*a, *b), Stroke::new(6.0 * l.k, color));
    }
}

/// A 7-segment display in a smoked near-black bezel. The tool displays are
/// bright red; the status display is the dimmer gray-red of the physical
/// panel. Unlit segments stay barely visible.
#[allow(clippy::too_many_arguments)]
fn draw_display(
    painter: &egui::Painter,
    l: &PanelLayout,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    title: &str,
    value: i32,
    dim: bool,
) {
    label(
        painter,
        l,
        x + w * 0.5,
        y - 18.0,
        12.0,
        title,
        MUTED,
        Align2::CENTER_CENTER,
    );
    let bezel = l.r(x, y, w, h);
    painter.rect(
        bezel,
        5.0 * l.k,
        Color32::from_rgb(0x0b, 0x0d, 0x0e),
        Stroke::new(1.5 * l.k, Color32::from_rgb(0x2e, 0x32, 0x35)),
        egui::StrokeKind::Inside,
    );
    let screen = bezel.shrink(7.0 * l.k);
    painter.rect_filled(screen, 2.0 * l.k, Color32::from_rgb(0x16, 0x02, 0x02));
    let (on_color, off_color) = if dim {
        (
            Color32::from_rgb(0xc0, 0x4a, 0x42),
            Color32::from_rgb(0x3a, 0x12, 0x10),
        )
    } else {
        (
            Color32::from_rgb(0xff, 0x2b, 0x2b),
            Color32::from_rgb(0x46, 0x10, 0x10),
        )
    };
    let text = if (0..=99).contains(&value) {
        format!("{value:02}")
    } else {
        "--".into()
    };
    let digit_w = screen.height() * 0.58;
    let gap = screen.height() * 0.15;
    let total = digit_w * 2.0 + gap;
    let start = screen.center().x - total * 0.5;
    for (index, ch) in text.chars().take(2).enumerate() {
        let digit = Rect::from_min_size(
            Pos2::new(
                start + index as f32 * (digit_w + gap),
                screen.top() + screen.height() * 0.10,
            ),
            Vec2::new(digit_w, screen.height() * 0.80),
        );
        draw_7seg_digit(painter, l, digit, ch, on_color, off_color);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_panel(
    ui: &egui::Ui,
    l: &PanelLayout,
    state: &SimPanelState,
    handle: &mut SimPanelHandle,
    pressed: &mut u64,
    holds: &mut HoldTracker,
    power_pulse: &mut Option<(bool, Instant)>,
    probe_key: bool,
) {
    let it = interactives(l, probe_key);
    // Match this frame's input events against the momentary key rects BEFORE
    // painting, so the state published and the caps drawn this frame already
    // include the newest press or release.
    holds.process(ui, &it.momentary);

    let painter = ui.painter();
    painter.rect_filled(ui.max_rect(), 0.0, SURROUND);
    // Raised outer bezel: a darker rim with a top-edge highlight, the plate
    // inset inside it, and a corner screw in each corner.
    let bezel = l.r(8.0, 8.0, 1664.0, 984.0);
    painter.rect(
        bezel,
        10.0 * l.k,
        BEZEL_RIM,
        Stroke::new(2.0 * l.k, EDGE_HI),
        egui::StrokeKind::Inside,
    );
    let plate = l.r(20.0, 20.0, 1640.0, 960.0);
    painter.rect(
        plate,
        6.0 * l.k,
        PLATE,
        Stroke::new(1.0 * l.k, Color32::from_rgb(0x05, 0x06, 0x06)),
        egui::StrokeKind::Inside,
    );
    painter.line_segment(
        [
            plate.left_top() + Vec2::new(8.0 * l.k, 2.0 * l.k),
            plate.right_top() + Vec2::new(-8.0 * l.k, 2.0 * l.k),
        ],
        Stroke::new(1.0 * l.k, Color32::from_rgb(0x2e, 0x32, 0x35)),
    );
    for (sx, sy) in [(34.0, 34.0), (1646.0, 34.0), (34.0, 966.0), (1646.0, 966.0)] {
        let c = l.p(sx, sy);
        paint_dome(
            painter,
            c,
            7.0 * l.k,
            Color32::from_rgb(0x3d, 0x41, 0x45),
            Color32::from_rgb(0x12, 0x14, 0x15),
        );
        painter.line_segment(
            [c + Vec2::new(-4.0, 0.0) * l.k, c + Vec2::new(4.0, 0.0) * l.k],
            Stroke::new(1.2 * l.k, Color32::from_rgb(0x08, 0x09, 0x0a)),
        );
    }

    // Section headers.
    label(
        painter,
        l,
        45.0,
        34.0,
        13.0,
        "POWER",
        MUTED,
        Align2::LEFT_CENTER,
    );
    label(
        painter,
        l,
        510.0,
        32.0,
        13.0,
        "MODE",
        MUTED,
        Align2::CENTER_CENTER,
    );
    // Same baseline as the override dials' titles (cy - r - 44 = 53).
    label(
        painter,
        l,
        1530.0,
        53.0,
        13.0,
        "PROGRAM PROTECT",
        MUTED,
        Align2::CENTER_CENTER,
    );

    // POWER ON/OFF guarded pushbuttons.
    let power_on = state.power && !state.estop;
    if power_key(
        ui,
        l,
        it.power_on,
        "ON",
        IVORY,
        if power_on { Some(GREEN) } else { None },
        "power-on",
    ) {
        handle.set(|state| {
            state.power = true;
            state.estop = false;
            state.power_on_pulse = true;
            state.power_off_pulse = false;
        });
        *power_pulse = Some((true, Instant::now()));
        *pressed = 0;
    }
    // The ON and OFF keys are mutually exclusive indicators, like the lit
    // pushbuttons on a real cabinet. OFF used to be drawn with a fixed
    // background, so after releasing the E-stop the machine was off but no key
    // showed it.
    if power_key(
        ui,
        l,
        it.power_off,
        "OFF",
        Color32::from_rgb(0x10, 0x12, 0x13),
        if power_on { None } else { Some(AMBER) },
        "power-off",
    ) {
        handle.set(|state| {
            state.power = false;
            state.power_on_pulse = false;
            state.power_off_pulse = true;
        });
        *power_pulse = Some((false, Instant::now()));
    }
    lamp(painter, l, 63.0, 104.0, 8.0, power_on, GREEN);
    lamp(painter, l, 141.0, 104.0, 8.0, !power_on, AMBER);

    // E-stop mushroom. This is an operator-owned latching input, so the latched
    // state has to be visible on the panel itself — not only in the HMI.
    let estop_center = l.p(ESTOP_AT.0, ESTOP_AT.1);
    let latched = state.estop;
    // The wide yellow collar ring the mushroom sits on.
    paint_dome(
        painter,
        estop_center,
        66.0 * l.k,
        YELLOW,
        Color32::from_rgb(0xc4, 0x9c, 0x00),
    );
    painter.circle_stroke(
        estop_center,
        66.0 * l.k,
        Stroke::new(1.5 * l.k, Color32::from_rgb(0x8f, 0x71, 0x00)),
    );
    // A latched mushroom sits lower and darker, like a real pressed button.
    let inner = if latched { 46.0 } else { 55.0 };
    if latched {
        paint_dome(
            painter,
            estop_center,
            inner * l.k,
            Color32::from_rgb(0xa8, 0x26, 0x1f),
            Color32::from_rgb(0x60, 0x0f, 0x0b),
        );
    } else {
        paint_dome(
            painter,
            estop_center,
            inner * l.k,
            Color32::from_rgb(0xff, 0x5f, 0x55),
            Color32::from_rgb(0xb0, 0x21, 0x19),
        );
        paint_specular(painter, estop_center, inner * l.k, 70);
    }
    painter.circle_stroke(
        estop_center,
        inner * l.k,
        Stroke::new(3.0 * l.k, Color32::from_rgb(0x7c, 0x18, 0x13)),
    );
    if latched {
        // Concentric ring marks the latched (twist-to-release) state.
        painter.circle_stroke(estop_center, 30.0 * l.k, Stroke::new(2.0 * l.k, YELLOW));
    }
    label(
        painter,
        l,
        ESTOP_AT.0,
        236.0,
        11.0,
        if latched {
            "E-STOP ACTIVE"
        } else {
            "EMERGENCY STOP"
        },
        if latched { RED } else { MUTED },
        Align2::CENTER_CENTER,
    );
    let estop_response = ui.interact(it.estop, ui.scope_id().with("estop"), Sense::click());
    if estop_response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if estop_response.clicked() {
        handle.set(|state| {
            state.estop = !state.estop;
            if state.estop {
                state.power = false;
            } else {
                // Releasing the mushroom asks the controller to leave E-stop
                // through its own one-shot, so the latch clears without the
                // machine being powered back up. Turning the machine on stays a
                // separate, deliberate act (`POWER ON`).
                state.estop_reset_pulse = true;
            }
        });
    }

    draw_mode_knob(ui, l, state, it.mode_knob);

    if let Some(value) = draw_override_knob(
        ui,
        l,
        FEED_KNOB,
        it.feed,
        0.0,
        1.5,
        state.feed_override as f32,
        "FEEDRATE OVERRIDE %",
        "feed",
        &FEED_NUMERALS,
    ) {
        handle.set(|state| state.feed_override = value.clamp(0.0, 1.5) as f64);
    }
    if let Some(value) = draw_override_knob(
        ui,
        l,
        RAPID_KNOB,
        it.rapid,
        0.0,
        1.0,
        state.rapid_override as f32,
        "RAPID OVERRIDE %",
        "rapid",
        &RAPID_NUMERALS,
    ) {
        handle.set(|state| state.rapid_override = value.clamp(0.0, 1.0) as f64);
    }
    if let Some(value) = draw_override_knob(
        ui,
        l,
        SPINDLE_KNOB,
        it.spindle,
        0.5,
        1.2,
        state.spindle_override as f32,
        "SPINDLE OVERRIDE %",
        "spindle",
        &SPINDLE_NUMERALS,
    ) {
        handle.set(|state| state.spindle_override = value.clamp(0.5, 1.2) as f64);
    }

    // Program protect keyswitch: a metal bezel, a dark face and the position
    // pointer. The toggle logic is unchanged.
    let protect_center = l.p(PROTECT_AT.0, PROTECT_AT.1);
    paint_dome(
        painter,
        protect_center,
        50.0 * l.k,
        Color32::from_rgb(0x9a, 0xa0, 0xa5),
        Color32::from_rgb(0x48, 0x4d, 0x51),
    );
    painter.circle_filled(
        protect_center,
        40.0 * l.k,
        Color32::from_rgb(0x1b, 0x1e, 0x20),
    );
    painter.circle_stroke(
        protect_center,
        40.0 * l.k,
        Stroke::new(2.0 * l.k, KEY_BORDER),
    );
    let a = if state.program_protect { -1.0 } else { 1.0 };
    let tip = protect_center + Vec2::new(0.55 * a, 0.8) * 30.0 * l.k;
    painter.line_segment(
        [protect_center, tip],
        Stroke::new(
            6.0 * l.k,
            if state.program_protect { GREEN } else { AMBER },
        ),
    );
    painter.circle_filled(
        protect_center,
        6.0 * l.k,
        Color32::from_rgb(0x0c, 0x0e, 0x0f),
    );
    label(
        painter,
        l,
        PROTECT_AT.0,
        210.0,
        12.0,
        if state.program_protect { "ON" } else { "OFF" },
        if state.program_protect { GREEN } else { MUTED },
        Align2::CENTER_CENTER,
    );
    if ui
        .interact(it.protect, ui.scope_id().with("protect"), Sense::click())
        .clicked()
    {
        handle.set(|state| state.program_protect = !state.program_protect);
    }

    // Axis and alarm lamps.
    label(
        painter,
        l,
        58.0,
        250.0,
        13.0,
        "AXIS",
        MUTED,
        Align2::LEFT_CENTER,
    );
    let axes: [(u64, &str); 6] = [
        (crate::leds::X, "X"),
        (crate::leds::Y, "Y"),
        (crate::leds::Z, "Z"),
        (crate::leds::A, "A"),
        (crate::leds::B, "B"),
        (crate::leds::C, "C"),
    ];
    for (index, (bit, name)) in axes.iter().enumerate() {
        let x = 40.0 + index as f32 * 48.0;
        lamp(painter, l, x, 285.0, 14.0, state.leds & bit != 0, GREEN);
        label(
            painter,
            l,
            x,
            311.0,
            11.0,
            name,
            TEXT,
            Align2::CENTER_CENTER,
        );
    }
    label(
        painter,
        l,
        58.0,
        350.0,
        13.0,
        "ALARM",
        MUTED,
        Align2::LEFT_CENTER,
    );
    let alarms: [(u64, &str, Color32); 6] = [
        (crate::leds::MC_ALARM, "MC", RED),
        (crate::leds::OIL_ALARM, "OIL", RED),
        (crate::leds::GEAR_ALARM, "GEAR", RED),
        (crate::leds::AIRBLOW_ALARM, "AIR BLOW", RED),
        (crate::leds::RUN, "RUN", AMBER),
        (crate::leds::READY, "READY", GREEN),
    ];
    for (index, (bit, name, color)) in alarms.iter().enumerate() {
        let x = 40.0 + index as f32 * 52.0;
        // The MC alarm lamp also mirrors a latched E-stop, so the panel shows
        // the fault even when no controller is driving the lamps.
        let lit = state.leds & bit != 0 || (*bit == crate::leds::MC_ALARM && state.estop);
        lamp(painter, l, x, 388.0, 14.0, lit, *color);
        label(painter, l, x, 415.0, 9.0, name, TEXT, Align2::CENTER_CENTER);
    }

    draw_display(
        painter,
        l,
        560.0,
        300.0,
        250.0,
        105.0,
        "MAG TOOL",
        state.sseg_tool,
        false,
    );
    draw_display(
        painter,
        l,
        930.0,
        300.0,
        250.0,
        105.0,
        "SPD TOOL",
        state.sseg_next_tool,
        false,
    );
    draw_display(
        painter,
        l,
        1340.0,
        300.0,
        250.0,
        105.0,
        "STATUS",
        state.sseg_status,
        true,
    );

    // The left group is 3 columns; cycle keys occupy the bottom row.
    for ((name, sub, mask, led), rect) in LEFT_KEYS.iter().zip(left_key_rects(l).iter()) {
        let fill = if *mask == buttons::RAPID {
            Color32::from_rgb(0x8a, 0x75, 0x18)
        } else {
            KEY_BG
        };
        // Momentary: the HoldTracker latched the press and releases it only
        // when that same pointer or touch lets go.
        let down = holds.is_held(*mask);
        panel_key(ui, l, *rect, name, *sub, fill, state.leds & led != 0, down, *mask);
        handle.set(|s| s.set_button(*mask, down));
    }
    draw_cycle_plate(
        painter,
        l,
        CYCLE_START_PLATE_AT.0,
        CYCLE_START_PLATE_AT.1,
        CyclePlate::Start,
        state.leds & crate::leds::CYCLE_ON != 0,
    );
    draw_cycle_plate(
        painter,
        l,
        CYCLE_STOP_PLATE_AT.0,
        CYCLE_STOP_PLATE_AT.1,
        CyclePlate::Stop,
        state.leds & crate::leds::CYCLE_OFF != 0,
    );
    if round_key(
        ui,
        l,
        CYCLE_START_AT,
        it.cycle_start,
        "CYCLE START",
        Color32::from_rgb(0xf5, 0xf4, 0xee),
        Color32::from_rgb(0xbd, 0xbb, 0xb3),
        "cycle-start",
    ) {
        handle.set(|s| s.set_button(buttons::CYCLE_START, true));
        *pressed |= buttons::CYCLE_START;
    }
    if round_key(
        ui,
        l,
        CYCLE_STOP_AT,
        it.cycle_stop,
        "CYCLE STOP",
        Color32::from_rgb(0x2c, 0x2f, 0x32),
        Color32::from_rgb(0x0e, 0x10, 0x11),
        "cycle-stop",
    ) {
        handle.set(|s| s.set_button(buttons::CYCLE_STOP, true));
        *pressed |= buttons::CYCLE_STOP;
    }

    // Simulation-only probe contact, gated behind `--probe-key`: a debug aid
    // that must not crowd the panel by default. Momentary, like the other
    // matrix keys: the contact is closed while the key is held, matching how
    // the probe input is asserted on contact and released on retract.
    if probe_key {
        let down = holds.is_held(PROBE_HOLD);
        let rect = l.r(PROBE_KEY_AT.0, PROBE_KEY_AT.1, KEY_W, KEY_H);
        paint_key_cap(
            ui,
            l,
            rect,
            ui.scope_id().with(("toggle-key", "probe-contact")),
            KEY_BG,
            state.probe,
            down,
        );
        ui.painter().text(
            Pos2::new(rect.center().x, rect.top() + rect.height() * 0.36),
            Align2::CENTER_CENTER,
            "PROBE",
            l.font(14.0),
            TEXT,
        );
        ui.painter().text(
            Pos2::new(rect.center().x, rect.top() + rect.height() * 0.70),
            Align2::CENTER_CENTER,
            "CONTACT",
            l.font(8.5),
            if state.probe { GREEN } else { MUTED },
        );
        handle.set(|state| state.probe = down);
    }

    paint_key_grid(
        ui,
        l,
        state,
        &MIDDLE_KEYS,
        &key_grid_rects(l, MIDDLE_X, 3, MIDDLE_KEYS.len()),
        holds,
        handle,
    );
    paint_key_grid(
        ui,
        l,
        state,
        &RIGHT_KEYS,
        &key_grid_rects(l, RIGHT_X, 3, RIGHT_KEYS.len()),
        holds,
        handle,
    );

    // Tiny power/estop status strip keeps the simulation state visible.
    label(
        painter,
        l,
        840.0,
        960.0,
        10.0,
        if state.estop {
            "E-STOP ACTIVE"
        } else if power_on {
            "MACHINE POWER ON"
        } else {
            "MACHINE POWER OFF"
        },
        if state.estop {
            RED
        } else if power_on {
            GREEN
        } else {
            MUTED
        },
        Align2::CENTER_CENTER,
    );

    // Keyboard shortcuts make the panel usable without a mouse.
    let keys = ui.input(|input| {
        (
            input.key_pressed(egui::Key::P),
            input.key_pressed(egui::Key::O),
            input.key_pressed(egui::Key::E),
            [
                input.key_pressed(egui::Key::Num1),
                input.key_pressed(egui::Key::Num2),
                input.key_pressed(egui::Key::Num3),
                input.key_pressed(egui::Key::Num4),
                input.key_pressed(egui::Key::Num5),
                input.key_pressed(egui::Key::Num6),
                input.key_pressed(egui::Key::Num7),
                input.key_pressed(egui::Key::Num8),
            ],
        )
    });
    if keys.0 {
        handle.set(|state| {
            state.power = true;
            state.estop = false;
            state.power_on_pulse = true;
        });
        *power_pulse = Some((true, Instant::now()));
    }
    if keys.1 {
        handle.set(|state| {
            state.power = false;
            state.power_off_pulse = true;
        });
        *power_pulse = Some((false, Instant::now()));
    }
    if keys.2 {
        handle.set(|state| {
            state.estop = !state.estop;
            if state.estop {
                state.power = false;
            }
        });
    }
    if ui.input(|input| input.key_pressed(egui::Key::R)) {
        handle.set(|state| state.set_button(buttons::RAPID, true));
        *pressed |= buttons::RAPID;
    }
    if ui.input(|input| input.key_pressed(egui::Key::C)) {
        handle.set(|state| state.set_button(buttons::CYCLE_START, true));
        *pressed |= buttons::CYCLE_START;
    }
    if ui.input(|input| input.key_pressed(egui::Key::S)) {
        handle.set(|state| state.set_button(buttons::CYCLE_STOP, true));
        *pressed |= buttons::CYCLE_STOP;
    }
    let modes = PanelMode::ALL;
    for (index, active) in keys.3.iter().enumerate() {
        if *active {
            if let Some(mode) = modes.get(index) {
                handle.set(|state| state.mode = *mode);
            }
        }
    }
    if let Some(Some(mode)) = ui.ctx().data_mut(|data| {
        data.remove_temp::<Option<PanelMode>>(ui.scope_id().with("linc-panel-mode"))
    }) {
        handle.set(|state| state.mode = mode);
    }
}

/// Paint one grid of legend keys. Hold state comes from the HoldTracker (see
/// `paint_panel`); the grid only paints and publishes.
fn paint_key_grid(
    ui: &egui::Ui,
    l: &PanelLayout,
    state: &SimPanelState,
    keys: &[(&str, u64, Option<u64>)],
    rects: &[Rect],
    holds: &HoldTracker,
    handle: &mut SimPanelHandle,
) {
    for ((name, mask, led), rect) in keys.iter().zip(rects.iter()) {
        let fill = if *mask == buttons::SPD_SP {
            Color32::from_rgb(0x8d, 0x1c, 0x18)
        } else {
            KEY_BG
        };
        let down = holds.is_held(*mask);
        let led_on = led.map(|bit| state.leds & bit != 0).unwrap_or(down);
        panel_key(ui, l, *rect, name, None, fill, led_on, down, *mask);
        handle.set(|s| s.set_button(*mask, down));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linc_diag::SimControl as _;

    #[test]
    fn a_ready_sim_raises_the_power_up_one_shots() {
        let mut released = SimPanelState::default();
        released.apply_start(SimStart::EstopReleased);
        assert!(!released.estop);
        assert!(released.estop_reset_pulse);
        assert!(!released.power);
        assert!(!released.power_on_pulse);

        let mut powered = SimPanelState::default();
        powered.apply_start(SimStart::Powered);
        assert!(!powered.estop);
        assert!(powered.power);
        assert!(powered.power_on_pulse);
        assert!(!powered.power_off_pulse);

        // A cold cabinet keeps the latched state a real one is found in.
        let mut cold = SimPanelState::default();
        cold.apply_start(SimStart::Cold);
        assert!(cold.estop);
        assert!(!cold.power);
        assert!(!cold.estop_reset_pulse);
    }

    #[test]
    fn the_ready_environment_variable_names_the_starting_point() {
        assert_eq!(SimStart::from_value("estop"), SimStart::EstopReleased);
        assert_eq!(SimStart::from_value(" power "), SimStart::Powered);
        assert_eq!(SimStart::from_value(""), SimStart::Cold);
        assert_eq!(SimStart::from_value("ready"), SimStart::Cold);
    }

    #[test]
    fn simulator_exposes_one_hot_mode_and_home_mapping() {
        let handle = SimPanelHandle::new();
        handle.set(|state| {
            state.mode = PanelMode::Home;
            state.set_button(buttons::RAPID, true);
            state.set_button(buttons::JOG_XP, true);
        });
        let mut driver = SimPanelDriver::new(handle, Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(3));
        let snapshot = driver.poll().unwrap();
        assert_eq!(snapshot.mode, PanelModeValidity::Valid(PanelMode::Home));
        assert!(snapshot.connected);
        assert_eq!(snapshot.home, [true, true, false, false]);
        assert_eq!(snapshot.buttons & buttons::RAPID, 0);
    }

    #[test]
    fn unknown_status_never_fabricates_a_wcs() {
        assert_eq!(status_display_text(-1), "--");
        assert_eq!(status_display_text(54), "54");
        assert_eq!(status_display_text(60), "--");
    }

    #[test]
    fn probe_contact_is_published_in_the_panel_snapshot() {
        let handle = SimPanelHandle::new();
        let mut driver = SimPanelDriver::new(handle.clone(), Duration::from_millis(1));
        handle.set(|state| state.probe = true);
        let snapshot = poll_driver(&mut driver);
        assert!(snapshot.probe);
        handle.set(|state| state.probe = false);
        let snapshot = poll_driver(&mut driver);
        assert!(!snapshot.probe);
    }

    #[test]
    fn display_inputs_are_reflected_in_state() {
        let handle = SimPanelHandle::new();
        let mut driver = SimPanelDriver::new(handle.clone(), Duration::from_millis(1));
        driver
            .write(&PanelOutputs {
                display: PanelDisplay {
                    leds: 0b101,
                    sseg_tool: 12,
                    sseg_next_tool: 13,
                    sseg_status: 55,
                    vel: [1.0, 2.0, 3.0],
                    homing: false,
                },
                ..Default::default()
            })
            .unwrap();
        let state = handle.snapshot();
        assert_eq!(state.leds, 0b101);
        assert_eq!(state.sseg_status, 55);
    }

    fn poll_driver(driver: &mut SimPanelDriver) -> PanelSnapshot {
        // The driver rate-limits to its cycle; one sleep makes the next poll a
        // real one rather than the cached snapshot.
        std::thread::sleep(Duration::from_millis(3));
        driver.poll().unwrap()
    }

    #[test]
    fn a_disconnect_fault_takes_the_panel_offline_and_an_invalid_mode_fails_closed() {
        let handle = SimPanelHandle::new();
        let mut driver = SimPanelDriver::new(handle.clone(), Duration::from_millis(1));

        handle.set(|state| state.faults.set(PanelFault::Disconnect));
        let snapshot = poll_driver(&mut driver);
        assert!(!snapshot.connected);
        assert!(snapshot.communication_error);
        assert_eq!(snapshot.mode, PanelModeValidity::Unknown);

        handle.set(|state| state.faults.clear());
        handle.set(|state| state.faults.set(PanelFault::InvalidMode));
        let snapshot = poll_driver(&mut driver);
        assert!(snapshot.connected, "the link still works");
        assert!(matches!(snapshot.mode, PanelModeValidity::Invalid(_)));
    }

    #[test]
    fn an_unknown_protect_input_reports_itself_as_unknown() {
        let handle = SimPanelHandle::new();
        let mut driver = SimPanelDriver::new(handle, Duration::from_millis(1));
        handle_set_fault(&driver, PanelFault::ProtectUnknown);
        let snapshot = poll_driver(&mut driver);
        assert!(!snapshot.program_protect_known);
        assert!(
            snapshot.protect_effective(),
            "an untrusted protect fails closed"
        );
    }

    fn handle_set_fault(driver: &SimPanelDriver, fault: PanelFault) {
        driver.handle().set(|state| state.faults.set(fault));
    }

    #[test]
    fn a_crc_fault_reports_an_error_without_going_offline() {
        let handle = SimPanelHandle::new();
        let mut driver = SimPanelDriver::new(handle.clone(), Duration::from_millis(1));
        handle.set(|state| state.faults.set(PanelFault::CrcError));
        let snapshot = poll_driver(&mut driver);
        assert!(snapshot.connected);
        assert!(snapshot.communication_error);
    }

    #[test]
    fn a_bounded_press_releases_itself_on_the_driver_cycle() {
        let handle = SimPanelHandle::new();
        handle.set(|state| state.press_for(buttons::CYCLE_START, Duration::from_millis(5)));
        assert!(handle.snapshot().buttons & buttons::CYCLE_START != 0);
        let mut driver = SimPanelDriver::new(handle.clone(), Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(10));
        let snapshot = driver.poll().unwrap();
        assert_eq!(snapshot.buttons & buttons::CYCLE_START, 0);
        assert!(handle.snapshot().holds.is_empty());
    }

    #[test]
    fn a_patch_releases_the_mushroom_with_the_same_one_shot_as_the_window_key() {
        let handle = SimPanelHandle::new();
        let control = SimControlState::new(handle.clone());
        let patch = SimPanelPatch {
            estop: Some(false),
            mode: Some("jog".into()),
            feed_override: Some(0.5),
            ..Default::default()
        };
        let state = linc_diag::SimControl::apply(&control, &patch).expect("patch");
        assert_eq!(state["estop"], false);
        assert_eq!(state["mode"], "JOG");
        assert_eq!(state["feed_override"], 0.5);
        assert!(
            handle.snapshot().estop_reset_pulse,
            "the controller is asked to reset"
        );
    }

    #[test]
    fn a_patch_naming_an_unknown_button_is_refused() {
        let control = SimControlState::new(SimPanelHandle::new());
        let patch = SimPanelPatch {
            press: Some(vec!["make_coffee".into()]),
            ..Default::default()
        };
        let error = linc_diag::SimControl::apply(&control, &patch).expect_err("unknown button");
        assert_eq!(error.code, "INVALID_ARGUMENT");
        assert!(error.message.contains("make_coffee"));

        let error = SimControlState::new(SimPanelHandle::new())
            .apply(&SimPanelPatch {
                mode: Some("TURBO".into()),
                ..Default::default()
            })
            .expect_err("unknown mode");
        assert!(error.message.contains("TURBO"));
    }

    #[test]
    fn reset_returns_the_cabinet_to_estop_latched_and_power_off() {
        let handle = SimPanelHandle::new();
        let control = SimControlState::new(handle.clone());
        control
            .apply(&SimPanelPatch {
                power: Some(true),
                estop: Some(false),
                program_protect: Some(true),
                ..Default::default()
            })
            .expect("patch");
        control.inject_fault(PanelFault::Disconnect).expect("fault");
        control.reset().expect("reset");
        let state = handle.snapshot();
        assert!(state.estop);
        assert!(!state.power);
        assert!(!state.program_protect);
        assert!(state.faults.labels().is_empty());
    }

    // ---- Headless touch and geometry tests ----
    //
    // paint_panel is driven through egui::Context::run with a 1280x720
    // screen_rect, the acceptance size. The CentralPanel gets no frame so its
    // rect matches the root ui eframe hands to SimPanelApp.

    /// A paint_panel harness mirroring SimPanelApp's per-frame call.
    struct PanelFixture {
        ctx: egui::Context,
        handle: SimPanelHandle,
        pressed: u64,
        holds: HoldTracker,
        power_pulse: Option<(bool, Instant)>,
        probe_key: bool,
        time: f64,
    }

    impl PanelFixture {
        fn new(probe_key: bool) -> Self {
            Self {
                ctx: egui::Context::default(),
                handle: SimPanelHandle::new(),
                pressed: 0,
                holds: HoldTracker::default(),
                power_pulse: None,
                probe_key,
                time: 0.0,
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0))),
                events,
                time: Some(self.time),
                focused: true,
                ..Default::default()
            };
            let probe_key = self.probe_key;
            let mut output = self.ctx.run_ui(input, |ui| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let state = self.handle.snapshot();
                        let layout = panel_layout(ui);
                        paint_panel(
                            ui,
                            &layout,
                            &state,
                            &mut self.handle,
                            &mut self.pressed,
                            &mut self.holds,
                            &mut self.power_pulse,
                            probe_key,
                        );
                    });
            });
            // Headless: no renderer consumes the font atlas upload.
            output.textures_delta.clear();
        }

        fn layout(&self) -> PanelLayout {
            layout_for_rect(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0)))
        }

        fn momentary_rect(&self, mask: u64, probe_key: bool) -> Rect {
            interactives(&self.layout(), probe_key)
                .momentary
                .into_iter()
                .find(|(m, _)| *m == mask)
                .map(|(_, rect)| rect)
                .expect("momentary key rect")
        }
    }

    fn touch_event(phase: egui::TouchPhase, id: u64, pos: Pos2) -> egui::Event {
        egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(id),
            phase,
            pos,
            force: None,
        }
    }

    /// Mirrors egui-winit's touch translation: only the FIRST active touch is
    /// also turned into pointer events; further touches stay raw Touch events.
    struct TouchEmu {
        pointer_touch: Option<egui::TouchId>,
    }

    impl TouchEmu {
        fn new() -> Self {
            Self {
                pointer_touch: None,
            }
        }

        fn begin(&mut self, id: u64, pos: Pos2) -> Vec<egui::Event> {
            let mut events = vec![touch_event(egui::TouchPhase::Start, id, pos)];
            if self.pointer_touch.is_none() {
                self.pointer_touch = Some(egui::TouchId(id));
                events.push(egui::Event::PointerMoved(pos));
                events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                });
            }
            events
        }

        fn move_to(&mut self, id: u64, pos: Pos2) -> Vec<egui::Event> {
            let mut events = vec![touch_event(egui::TouchPhase::Move, id, pos)];
            if self.pointer_touch == Some(egui::TouchId(id)) {
                events.push(egui::Event::PointerMoved(pos));
            }
            events
        }

        fn end(&mut self, id: u64, pos: Pos2) -> Vec<egui::Event> {
            let mut events = vec![touch_event(egui::TouchPhase::End, id, pos)];
            if self.pointer_touch == Some(egui::TouchId(id)) {
                self.pointer_touch = None;
                events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                });
                events.push(egui::Event::PointerGone);
            }
            events
        }

        fn tap(&mut self, fix: &mut PanelFixture, id: u64, pos: Pos2) {
            let press = self.begin(id, pos);
            fix.frame(press);
            let release = self.end(id, pos);
            fix.frame(release);
        }
    }

    #[test]
    fn touch_press_holds_a_jog_key_until_touch_end() {
        let mut fix = PanelFixture::new(false);
        let mut touch = TouchEmu::new();
        let jog = fix.momentary_rect(buttons::JOG_XP, false);
        fix.frame(vec![]);
        fix.frame(touch.begin(1, jog.center()));
        assert!(
            fix.handle.snapshot().buttons & buttons::JOG_XP != 0,
            "touching the key asserts its button bit"
        );
        fix.frame(vec![]);
        assert!(
            fix.handle.snapshot().buttons & buttons::JOG_XP != 0,
            "the hold survives frames without input events"
        );
        fix.frame(touch.end(1, jog.center()));
        assert_eq!(
            fix.handle.snapshot().buttons & buttons::JOG_XP,
            0,
            "touch end releases the key"
        );
    }

    #[test]
    fn touch_tap_toggles_program_protect_and_estop() {
        let mut fix = PanelFixture::new(false);
        let mut touch = TouchEmu::new();
        let it = interactives(&fix.layout(), false);
        fix.frame(vec![]);

        touch.tap(&mut fix, 1, it.protect.center());
        assert!(fix.handle.snapshot().program_protect, "tap toggles protect on");
        touch.tap(&mut fix, 2, it.protect.center());
        assert!(
            !fix.handle.snapshot().program_protect,
            "tap toggles protect back off"
        );

        // Cold boot: E-stop latched, power off. A tap releases the mushroom
        // and raises the reset one-shot without powering up.
        assert!(fix.handle.snapshot().estop);
        touch.tap(&mut fix, 3, it.estop.center());
        let state = fix.handle.snapshot();
        assert!(!state.estop, "tap releases the latched E-stop");
        assert!(state.estop_reset_pulse);
        assert!(!state.power);

        touch.tap(&mut fix, 4, it.power_on.center());
        assert!(fix.handle.snapshot().power, "POWER ON tap powers up");
        touch.tap(&mut fix, 5, it.estop.center());
        let state = fix.handle.snapshot();
        assert!(state.estop, "tap latches the E-stop again");
        assert!(!state.power, "latching the E-stop drops machine power");
    }

    #[test]
    fn touch_drag_turns_the_feed_override_knob() {
        let mut fix = PanelFixture::new(false);
        let mut touch = TouchEmu::new();
        let it = interactives(&fix.layout(), false);
        fix.frame(vec![]);
        assert_eq!(fix.handle.snapshot().feed_override, 1.0);

        let mut pos = it.feed.center();
        fix.frame(touch.begin(1, pos));
        for _ in 0..4 {
            pos += Vec2::new(0.0, -12.0);
            fix.frame(touch.move_to(1, pos));
        }
        let after_up = fix.handle.snapshot().feed_override;
        assert!(
            after_up > 1.0,
            "dragging up raises the feed override, got {after_up}"
        );
        for _ in 0..8 {
            pos += Vec2::new(0.0, 12.0);
            fix.frame(touch.move_to(1, pos));
        }
        let after_down = fix.handle.snapshot().feed_override;
        assert!(
            after_down < after_up,
            "dragging back down lowers it: {after_down} < {after_up}"
        );
        assert!((0.0..=1.5).contains(&after_down));
        fix.frame(touch.end(1, pos));
    }

    #[test]
    fn touch_tap_on_the_mode_knob_steps_the_detent() {
        let mut fix = PanelFixture::new(false);
        let mut touch = TouchEmu::new();
        let it = interactives(&fix.layout(), false);
        fix.frame(vec![]);
        assert_eq!(fix.handle.snapshot().mode, PanelMode::Auto);
        touch.tap(&mut fix, 1, it.mode_knob.center());
        assert_eq!(
            fix.handle.snapshot().mode,
            PanelMode::Mdi,
            "a tap steps one detent"
        );
        touch.tap(&mut fix, 2, it.mode_knob.center());
        assert_eq!(fix.handle.snapshot().mode, PanelMode::Jog);
    }

    #[test]
    fn a_second_touch_tap_does_not_release_a_held_jog_key() {
        let mut fix = PanelFixture::new(false);
        let mut touch = TouchEmu::new();
        let jog = fix.momentary_rect(buttons::JOG_XP, false);
        let coolant = fix.momentary_rect(buttons::COOLANT, false);
        fix.frame(vec![]);
        // Finger 1 holds the jog key; finger 2 taps another matrix key.
        fix.frame(touch.begin(1, jog.center()));
        fix.frame(touch.begin(2, coolant.center()));
        let pressed_bits = fix.handle.snapshot().buttons;
        assert!(pressed_bits & buttons::JOG_XP != 0);
        assert!(pressed_bits & buttons::COOLANT != 0);
        // Finger 2 lifts. The old any_released rule cleared every held key
        // here; the jog must survive.
        fix.frame(touch.end(2, coolant.center()));
        let pressed_bits = fix.handle.snapshot().buttons;
        assert!(
            pressed_bits & buttons::JOG_XP != 0,
            "the held jog survives another finger's release"
        );
        assert_eq!(pressed_bits & buttons::COOLANT, 0);
        // Finger 1 lifts: the jog ends.
        fix.frame(touch.end(1, jog.center()));
        assert_eq!(fix.handle.snapshot().buttons & buttons::JOG_XP, 0);
    }

    #[test]
    fn every_interactive_target_is_at_least_44px_at_1280x720() {
        let fix = PanelFixture::new(true);
        let it = interactives(&fix.layout(), true);
        let all: Vec<(&str, Rect)> = vec![
            ("power on", it.power_on),
            ("power off", it.power_off),
            ("e-stop", it.estop),
            ("program protect", it.protect),
            ("mode knob", it.mode_knob),
            ("feed knob", it.feed),
            ("rapid knob", it.rapid),
            ("spindle knob", it.spindle),
            ("cycle start", it.cycle_start),
            ("cycle stop", it.cycle_stop),
        ];
        let mut all: Vec<(String, Rect)> = all
            .into_iter()
            .map(|(name, rect)| (name.to_string(), rect))
            .collect();
        for (mask, rect) in &it.momentary {
            all.push((format!("momentary key mask {mask:#x}"), *rect));
        }
        let mut min_w = f32::INFINITY;
        let mut min_h = f32::INFINITY;
        let mut smallest = String::new();
        for (name, rect) in &all {
            if rect.width() * rect.height() < min_w * min_h {
                min_w = rect.width();
                min_h = rect.height();
                smallest = name.clone();
            }
            assert!(
                rect.width() >= 44.0 && rect.height() >= 44.0,
                "{name} is only {:.1}x{:.1} px at 1280x720",
                rect.width(),
                rect.height()
            );
        }
        eprintln!(
            "smallest interactive target at 1280x720: {smallest} at {min_w:.1}x{min_h:.1} px ({} targets)",
            all.len()
        );
    }

    #[test]
    fn probe_contact_key_is_hidden_without_the_flag() {
        let mut fix = PanelFixture::new(false);
        let mut touch = TouchEmu::new();
        // The spot the key occupies with --probe-key holds nothing now.
        let probe_spot = fix
            .layout()
            .r(PROBE_KEY_AT.0, PROBE_KEY_AT.1, KEY_W, KEY_H);
        let it = interactives(&fix.layout(), false);
        assert!(
            it.momentary
                .iter()
                .all(|(_, rect)| !rect.contains(probe_spot.center())),
            "no interactive target where the probe key would be"
        );
        fix.frame(vec![]);
        fix.frame(touch.begin(1, probe_spot.center()));
        fix.frame(vec![]);
        assert!(
            !fix.handle.snapshot().probe,
            "touching that area does not assert the probe without --probe-key"
        );
        fix.frame(touch.end(1, probe_spot.center()));
    }

    #[test]
    fn probe_contact_key_is_momentary_with_the_flag() {
        let mut fix = PanelFixture::new(true);
        let mut touch = TouchEmu::new();
        let probe = fix.momentary_rect(PROBE_HOLD, true);
        fix.frame(vec![]);
        fix.frame(touch.begin(1, probe.center()));
        assert!(
            fix.handle.snapshot().probe,
            "holding the probe key closes the contact"
        );
        fix.frame(vec![]);
        assert!(fix.handle.snapshot().probe, "the contact stays closed");
        fix.frame(touch.end(1, probe.center()));
        assert!(
            !fix.handle.snapshot().probe,
            "releasing the key opens the contact"
        );
    }
}

#[cfg(test)]
mod dial_tests {
    use super::{mode_angle, mode_from_angle, mode_index};
    use linc_core::model::PanelMode;

    #[test]
    fn dial_angles_map_to_their_own_detent() {
        for mode in PanelMode::ALL {
            let angle = mode_angle(mode_index(mode)).to_degrees();
            assert_eq!(mode_from_angle(angle), mode, "{mode:?} at {angle}");
        }
    }
}
