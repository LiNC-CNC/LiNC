//! Simulation CNC control panel. This is a separate process/window from the
//! HMI and creates the same `cnc_panel` HAL surface as the physical driver.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use linc_core::model::{
    now_ms, PanelCapabilities, PanelLinkStats, PanelMode, PanelModeValidity, PanelSnapshot,
};

use crate::{
    base_capabilities, buttons, make_snapshot, PanelDisplay, PanelDriver, PanelError, PanelOutputs,
};

#[derive(Clone, Debug)]
pub struct SimPanelState {
    pub connected: bool,
    pub error: bool,
    pub power: bool,
    pub power_on_pulse: bool,
    pub power_off_pulse: bool,
    pub estop: bool,
    pub mode: PanelMode,
    pub feed_override: f64,
    pub rapid_override: f64,
    pub spindle_override: f64,
    pub program_protect: bool,
    pub buttons: u64,
    pub leds: u64,
    pub sseg_tool: i32,
    pub sseg_next_tool: i32,
    pub sseg_status: i32,
    pub velocity: [f64; 3],
    pub last_cycle_ms: f64,
}

impl Default for SimPanelState {
    fn default() -> Self {
        Self {
            connected: true,
            error: false,
            power: true,
            power_on_pulse: false,
            power_off_pulse: false,
            estop: false,
            mode: PanelMode::Auto,
            feed_override: 1.0,
            rapid_override: 1.0,
            spindle_override: 1.0,
            program_protect: false,
            buttons: 0,
            leds: 0,
            sseg_tool: 12,
            sseg_next_tool: 0,
            sseg_status: 54,
            velocity: [0.0; 3],
            last_cycle_ms: 0.0,
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

    pub fn led(&self, mask: u64) -> bool {
        self.leds & mask != 0
    }
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

pub struct SimPanelDriver {
    handle: SimPanelHandle,
    caps: PanelCapabilities,
    last_poll: Instant,
    cycle_time: Duration,
    last_display: PanelDisplay,
    outputs: PanelOutputs,
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

    fn outputs_from_state(&self, state: &SimPanelState) -> PanelOutputs {
        let mode = if state.connected {
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
            display: self.last_display.clone(),
        }
    }
}

impl PanelDriver for SimPanelDriver {
    fn observe_display(&mut self, display: &PanelDisplay) {
        self.handle.set(|state| {
            // Do not fight an explicit POWER ON/OFF pulse with one cycle of
            // stale E-stop/READY lamps.
            if state.power_on_pulse || state.power_off_pulse {
                return;
            }
            state.power = display.leds & crate::leds::READY != 0;
            state.estop = display.leds & crate::leds::MC_ALARM != 0;
        });
    }

    fn poll(&mut self) -> Result<PanelSnapshot, PanelError> {
        if self.last_poll.elapsed() < self.cycle_time {
            let state = self.handle.snapshot();
            return Ok(make_snapshot(
                state.connected,
                state.error,
                &self.outputs,
                self.caps.clone(),
                PanelLinkStats {
                    cycle_time_ms: state.last_cycle_ms,
                    port: "virtual".into(),
                    available: false,
                    ..Default::default()
                },
            ));
        }
        self.last_poll = Instant::now();
        let state = self.handle.snapshot();
        let outputs = self.outputs_from_state(&state);
        self.outputs = outputs.clone();
        let mut snapshot = make_snapshot(
            state.connected,
            state.error,
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

pub fn simulator_window(
    handle: SimPanelHandle,
    title: &str,
    scale: f32,
    shutdown: Arc<AtomicBool>,
) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1344.0, 800.0])
            .with_title(title),
        ..Default::default()
    };
    eframe::run_native(
        title,
        options,
        Box::new(move |cc| Ok(Box::new(SimPanelApp::new(cc, handle, scale, shutdown)))),
    )
}

struct SimPanelApp {
    handle: SimPanelHandle,
    pressed: u64,
    power_pulse: Option<(bool, Instant)>,
    last_frame: Instant,
    shutdown: Arc<AtomicBool>,
}

impl SimPanelApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        handle: SimPanelHandle,
        scale: f32,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        if (scale - 1.0).abs() > f32::EPSILON {
            cc.egui_ctx.set_zoom_factor(scale);
        }
        Self {
            handle,
            pressed: 0,
            power_pulse: None,
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
        if self.pressed != 0 && self.last_frame.elapsed().as_millis() > 80 {
            let pressed = self.pressed;
            self.handle.set(|state| state.set_button(pressed, false));
            self.pressed = 0;
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
            &mut self.power_pulse,
        );
        ctx.request_repaint_after(Duration::from_millis(60));
    }
}

const PANEL_BG: Color32 = Color32::from_rgb(0x30, 0x34, 0x36);
const KEY_BG: Color32 = Color32::from_rgb(0x1d, 0x21, 0x23);
const KEY_BORDER: Color32 = Color32::from_rgb(0x0d, 0x0f, 0x10);
const TEXT: Color32 = Color32::from_rgb(0xf2, 0xf3, 0xf4);
const MUTED: Color32 = Color32::from_rgb(0x9b, 0xa1, 0xa4);
const GREEN: Color32 = Color32::from_rgb(0x41, 0xe6, 0x5f);
const RED: Color32 = Color32::from_rgb(0xff, 0x3b, 0x30);
const AMBER: Color32 = Color32::from_rgb(0xff, 0xb0, 0x20);
const YELLOW: Color32 = Color32::from_rgb(0xff, 0xd6, 0x0a);
const BLUE: Color32 = Color32::from_rgb(0x4d, 0xa3, 0xff);

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

fn panel_layout(ui: &egui::Ui) -> PanelLayout {
    let avail = ui.available_rect_before_wrap();
    let k = (avail.width() / 1680.0)
        .min(avail.height() / 1000.0)
        .max(0.2);
    let size = Vec2::new(1680.0 * k, 1000.0 * k);
    PanelLayout {
        origin: avail.center() - size / 2.0,
        k,
    }
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
    painter.circle_filled(center, radius, Color32::from_rgb(0x12, 0x14, 0x15));
    painter.circle_stroke(center, radius, Stroke::new(1.0 * l.k, KEY_BORDER));
    painter.circle_filled(
        center,
        radius * 0.78,
        if on {
            color
        } else {
            Color32::from_rgb(0x2b, 0x2f, 0x30)
        },
    );
    if on {
        painter.circle_stroke(
            center,
            radius * 0.55,
            Stroke::new(1.0 * l.k, Color32::WHITE.gamma_multiply(0.45)),
        );
    }
}

fn rounded_button(
    ui: &egui::Ui,
    l: &PanelLayout,
    rect: Rect,
    id: egui::Id,
    fill: Color32,
    border: Color32,
) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click());
    let fill = if resp.hovered() {
        fill.gamma_multiply(1.22)
    } else {
        fill
    };
    ui.painter().rect(
        rect,
        4.0 * l.k,
        fill,
        Stroke::new(1.0 * l.k, if resp.hovered() { BLUE } else { border }),
        egui::StrokeKind::Inside,
    );
    resp
}

fn panel_key(
    ui: &egui::Ui,
    l: &PanelLayout,
    state: &SimPanelState,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    label_text: &str,
    sub: Option<&str>,
    mask: u64,
    fill: Color32,
    led: Option<u64>,
) -> bool {
    let rect = l.r(x, y, w, h);
    let resp = rounded_button(
        ui,
        l,
        rect,
        ui.scope_id().with(("panel-key", mask, x as i32, y as i32)),
        fill,
        KEY_BORDER,
    );
    let led_on = led
        .map(|bit| state.leds & bit != 0)
        .unwrap_or(state.buttons & mask != 0);
    ui.painter().circle_filled(
        rect.left_top() + Vec2::new(12.0 * l.k, 12.0 * l.k),
        3.5 * l.k,
        if led_on {
            GREEN
        } else {
            Color32::from_rgb(0x38, 0x3e, 0x41)
        },
    );
    if led_on {
        ui.painter().circle_stroke(
            rect.left_top() + Vec2::new(12.0 * l.k, 12.0 * l.k),
            5.2 * l.k,
            Stroke::new(1.0 * l.k, GREEN.gamma_multiply(0.55)),
        );
    }
    let cy = if sub.is_some() {
        y + h * 0.40
    } else {
        y + h * 0.5
    };
    label(
        ui.painter(),
        l,
        x + w * 0.5,
        cy,
        14.0,
        label_text,
        TEXT,
        Align2::CENTER_CENTER,
    );
    if let Some(sub) = sub {
        label(
            ui.painter(),
            l,
            x + w * 0.5,
            y + h * 0.70,
            18.0,
            sub,
            TEXT,
            Align2::CENTER_CENTER,
        );
    }
    resp.clicked()
}

fn power_key(
    ui: &egui::Ui,
    l: &PanelLayout,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    label_text: &str,
    fill: Color32,
    id: &'static str,
) -> bool {
    let rect = l.r(x, y, w, h);
    let resp = rounded_button(
        ui,
        l,
        rect,
        ui.scope_id().with((id, x as i32, y as i32)),
        fill,
        KEY_BORDER,
    );
    label(
        ui.painter(),
        l,
        x + w * 0.5,
        y + h * 0.5,
        13.0,
        label_text,
        if fill == GREEN {
            Color32::from_rgb(0x08, 0x20, 0x0c)
        } else {
            TEXT
        },
        Align2::CENTER_CENTER,
    );
    resp.clicked()
}

fn round_key(
    ui: &egui::Ui,
    l: &PanelLayout,
    x: f32,
    y: f32,
    r: f32,
    label_text: &str,
    fill: Color32,
    id: &'static str,
    led_on: bool,
) -> bool {
    let rect = Rect::from_center_size(l.p(x, y), Vec2::splat(2.0 * r * l.k));
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    let center = l.p(x, y);
    ui.painter().circle_filled(center, r * l.k, fill);
    ui.painter().circle_stroke(
        center,
        r * l.k,
        Stroke::new(2.0 * l.k, Color32::from_rgb(0x0c, 0x0e, 0x0f)),
    );
    ui.painter().circle_filled(
        center + Vec2::new(0.0, -r * 0.55 * l.k),
        4.0 * l.k,
        if led_on {
            GREEN
        } else {
            Color32::from_rgb(0x38, 0x3e, 0x41)
        },
    );
    label(
        ui.painter(),
        l,
        x,
        y + r + 22.0,
        13.0,
        label_text,
        TEXT,
        Align2::CENTER_CENTER,
    );
    resp.clicked()
}

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
    // Angles match the printed labels around the rotary.
    const ANGLES: [f32; 8] = [110.0, 135.0, 180.0, -115.0, 0.0, -65.0, 45.0, 80.0];
    ANGLES[index.min(7)].to_radians()
}

fn draw_mode_knob(ui: &egui::Ui, l: &PanelLayout, state: &SimPanelState) {
    let (cx, cy, r) = (510.0, 145.0, 72.0);
    let center = l.p(cx, cy);
    ui.painter()
        .circle_filled(center, r * l.k, Color32::from_rgb(0x20, 0x23, 0x24));
    ui.painter().circle_stroke(
        center,
        r * l.k,
        Stroke::new(3.0 * l.k, Color32::from_rgb(0x0c, 0x0e, 0x0f)),
    );
    for i in 0..=16 {
        let a = (-135.0 + i as f32 * 270.0 / 16.0).to_radians();
        let inner = r * 0.86;
        let outer = r * 0.97;
        let p0 = center + Vec2::new(a.cos(), a.sin()) * inner * l.k;
        let p1 = center + Vec2::new(a.cos(), a.sin()) * outer * l.k;
        ui.painter()
            .line_segment([p0, p1], Stroke::new(1.0 * l.k, MUTED));
    }
    let a = mode_angle(mode_index(state.mode));
    let tip = center + Vec2::new(a.cos(), a.sin()) * r * 0.72 * l.k;
    ui.painter()
        .line_segment([center, tip], Stroke::new(4.0 * l.k, Color32::WHITE));
    ui.painter()
        .circle_filled(center, 7.0 * l.k, Color32::from_rgb(0x0c, 0x0e, 0x0f));

    let modes = [
        (PanelMode::Jog, "JOG", 450.0, 64.0),
        (PanelMode::Mpg, "MPG", 570.0, 64.0),
        (PanelMode::Mdi, "MDI", 402.0, 145.0),
        (PanelMode::Inc, "INC", 618.0, 145.0),
        (PanelMode::Auto, "AUTO", 402.0, 226.0),
        (PanelMode::Home, "HOME", 618.0, 226.0),
        (PanelMode::Edit, "EDIT", 450.0, 252.0),
        (PanelMode::ProgHome, "PROG.HOME", 570.0, 252.0),
    ];
    for (mode, name, x, y) in modes {
        let active = state.mode == mode;
        label(
            ui.painter(),
            l,
            x,
            y,
            12.0,
            name,
            if active { BLUE } else { TEXT },
            Align2::CENTER_CENTER,
        );
        let hit = l.r(x - 38.0, y - 10.0, 76.0, 20.0);
        if ui
            .interact(
                hit,
                ui.scope_id().with(("mode-label", name)),
                Sense::click(),
            )
            .clicked()
        {
            // handled by caller through a marker key; the painter has no handle.
            ui.ctx().data_mut(|data| {
                data.insert_temp(ui.scope_id().with("linc-panel-mode"), Some(mode))
            });
            ui.ctx().request_repaint();
        }
    }
    label(
        ui.painter(),
        l,
        510.0,
        32.0,
        13.0,
        "MODE",
        MUTED,
        Align2::CENTER_CENTER,
    );
}

fn draw_override_knob(
    ui: &egui::Ui,
    l: &PanelLayout,
    cx: f32,
    cy: f32,
    r: f32,
    min: f32,
    max: f32,
    value: f32,
    title: &str,
    id: &'static str,
) -> Option<f32> {
    let center = l.p(cx, cy);
    let rect = Rect::from_center_size(center, Vec2::splat(2.0 * r * l.k + 28.0 * l.k));
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
    ui.painter()
        .circle_filled(center, r * l.k, Color32::from_rgb(0x20, 0x23, 0x24));
    ui.painter().circle_stroke(
        center,
        r * l.k,
        Stroke::new(3.0 * l.k, Color32::from_rgb(0x0c, 0x0e, 0x0f)),
    );
    for i in 0..=10 {
        let a = (-135.0 + i as f32 * 270.0 / 10.0).to_radians();
        let p0 = center + Vec2::new(a.cos(), a.sin()) * r * 0.84 * l.k;
        let p1 = center + Vec2::new(a.cos(), a.sin()) * r * 0.98 * l.k;
        ui.painter()
            .line_segment([p0, p1], Stroke::new(1.0 * l.k, MUTED));
    }
    let norm = ((next - min) / (max - min)).clamp(0.0, 1.0);
    let a = (-135.0 + norm * 270.0).to_radians();
    let tip = center + Vec2::new(a.cos(), a.sin()) * r * 0.74 * l.k;
    ui.painter()
        .line_segment([center, tip], Stroke::new(4.0 * l.k, Color32::WHITE));
    ui.painter()
        .circle_filled(center, 7.0 * l.k, Color32::from_rgb(0x0c, 0x0e, 0x0f));
    label(
        ui.painter(),
        l,
        cx,
        cy - r - 24.0,
        13.0,
        title,
        MUTED,
        Align2::CENTER_CENTER,
    );
    label(
        ui.painter(),
        l,
        cx,
        cy + r + 20.0,
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
        let color = if segs[index] {
            on_color
        } else {
            Color32::from_rgb(0x46, 0x10, 0x10)
        };
        painter.line_segment(seg(*a, *b), Stroke::new(6.0 * l.k, color));
    }
}

fn draw_display(
    painter: &egui::Painter,
    l: &PanelLayout,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    title: &str,
    value: i32,
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
    let rect = l.r(x, y, w, h);
    painter.rect(
        rect,
        3.0 * l.k,
        Color32::from_rgb(0x1a, 0x03, 0x03),
        Stroke::new(2.0 * l.k, Color32::from_rgb(0x07, 0x08, 0x09)),
        egui::StrokeKind::Inside,
    );
    let text = if (0..=99).contains(&value) {
        format!("{value:02}")
    } else {
        "--".into()
    };
    let digit_w = h * 0.46;
    let gap = h * 0.12;
    let total = digit_w * 2.0 + gap;
    let start = rect.center().x - total * 0.5;
    for (index, ch) in text.chars().take(2).enumerate() {
        let digit = Rect::from_min_size(
            Pos2::new(
                start + index as f32 * (digit_w + gap),
                rect.top() + h * 0.10,
            ),
            Vec2::new(digit_w, h * 0.80),
        );
        draw_7seg_digit(painter, l, digit, ch, Color32::from_rgb(0xff, 0x2b, 0x2b));
    }
}

fn paint_panel(
    ui: &egui::Ui,
    l: &PanelLayout,
    state: &SimPanelState,
    handle: &mut SimPanelHandle,
    pressed: &mut u64,
    power_pulse: &mut Option<(bool, Instant)>,
) {
    let painter = ui.painter();
    painter.rect_filled(ui.max_rect(), 0.0, PANEL_BG);
    let panel = l.r(8.0, 8.0, 1664.0, 984.0);
    painter.rect(
        panel,
        6.0 * l.k,
        Color32::from_rgb(0x34, 0x38, 0x3a),
        Stroke::new(2.0 * l.k, KEY_BORDER),
        egui::StrokeKind::Inside,
    );
    // top labels
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
    label(
        painter,
        l,
        1530.0,
        34.0,
        13.0,
        "PROGRAM PROTECT",
        MUTED,
        Align2::CENTER_CENTER,
    );

    // POWER ON/OFF buttons
    let power_on = state.power && !state.estop;
    if power_key(
        ui,
        l,
        35.0,
        75.0,
        62.0,
        56.0,
        "ON",
        if power_on { GREEN } else { KEY_BG },
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
    if power_key(ui, l, 105.0, 75.0, 62.0, 56.0, "OFF", KEY_BG, "power-off") {
        handle.set(|state| {
            state.power = false;
            state.power_on_pulse = false;
            state.power_off_pulse = true;
        });
        *power_pulse = Some((false, Instant::now()));
    }
    lamp(painter, l, 27.0, 48.0, 7.0, power_on, GREEN);

    // E-stop mushroom
    let estop_center = l.p(245.0, 155.0);
    painter.circle_filled(estop_center, 66.0 * l.k, YELLOW);
    painter.circle_filled(estop_center, 55.0 * l.k, RED);
    painter.circle_stroke(
        estop_center,
        55.0 * l.k,
        Stroke::new(3.0 * l.k, Color32::from_rgb(0x9b, 0x1f, 0x18)),
    );
    label(
        painter,
        l,
        245.0,
        236.0,
        11.0,
        "EMERGENCY STOP",
        MUTED,
        Align2::CENTER_CENTER,
    );
    let estop_rect = Rect::from_center_size(estop_center, Vec2::splat(132.0 * l.k));
    if ui
        .interact(estop_rect, ui.scope_id().with("estop"), Sense::click())
        .clicked()
    {
        handle.set(|state| {
            state.estop = !state.estop;
            if state.estop {
                state.power = false;
            }
        });
    }

    draw_mode_knob(ui, l, state);

    if let Some(value) = draw_override_knob(
        ui,
        l,
        800.0,
        145.0,
        48.0,
        0.0,
        1.5,
        state.feed_override as f32,
        "FEEDRATE OVERRIDE %",
        "feed",
    ) {
        handle.set(|state| state.feed_override = value.clamp(0.0, 1.5) as f64);
    }
    if let Some(value) = draw_override_knob(
        ui,
        l,
        1030.0,
        145.0,
        48.0,
        0.0,
        1.0,
        state.rapid_override as f32,
        "RAPID OVERRIDE %",
        "rapid",
    ) {
        handle.set(|state| state.rapid_override = value.clamp(0.0, 1.0) as f64);
    }
    if let Some(value) = draw_override_knob(
        ui,
        l,
        1260.0,
        145.0,
        48.0,
        0.5,
        1.2,
        state.spindle_override as f32,
        "SPINDLE OVERRIDE %",
        "spindle",
    ) {
        handle.set(|state| state.spindle_override = value.clamp(0.5, 1.2) as f64);
    }

    // Program protect key
    let protect_center = l.p(1530.0, 145.0);
    painter.circle_filled(
        protect_center,
        48.0 * l.k,
        Color32::from_rgb(0x1b, 0x1e, 0x20),
    );
    painter.circle_stroke(
        protect_center,
        48.0 * l.k,
        Stroke::new(2.0 * l.k, KEY_BORDER),
    );
    let a = if state.program_protect { -1.0 } else { 1.0 };
    let tip = protect_center + Vec2::new(0.55 * a, 0.8) * 34.0 * l.k;
    painter.line_segment(
        [protect_center, tip],
        Stroke::new(6.0 * l.k, if state.program_protect { GREEN } else { AMBER }),
    );
    painter.circle_filled(
        protect_center,
        6.0 * l.k,
        Color32::from_rgb(0x0c, 0x0e, 0x0f),
    );
    label(
        painter,
        l,
        1530.0,
        210.0,
        12.0,
        if state.program_protect { "ON" } else { "OFF" },
        if state.program_protect { GREEN } else { MUTED },
        Align2::CENTER_CENTER,
    );
    let protect_rect = Rect::from_center_size(protect_center, Vec2::splat(110.0 * l.k));
    if ui
        .interact(protect_rect, ui.scope_id().with("protect"), Sense::click())
        .clicked()
    {
        handle.set(|state| state.program_protect = !state.program_protect);
    }

    // Axis and alarm lamps
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
        lamp(painter, l, x, 388.0, 14.0, state.leds & bit != 0, *color);
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
    );

    // Key matrix, matching the machine panel layout.
    let left = [
        ("B", Some("v"), buttons::JOG_BN, crate::leds::JOG_BN),
        (
            "SAFE DOOR",
            None,
            buttons::SAFE_DOOR,
            crate::leds::SAFE_DOOR,
        ),
        ("B", Some("^"), buttons::JOG_BP, crate::leds::JOG_BP),
        ("A", Some("v"), buttons::JOG_AN, crate::leds::JOG_AN),
        ("Z", Some("v"), buttons::JOG_ZN, crate::leds::JOG_ZN),
        ("Y", Some("v"), buttons::JOG_YN, crate::leds::JOG_YN),
        ("X", Some("v"), buttons::JOG_XN, crate::leds::JOG_XN),
        ("RAPID", None, buttons::RAPID, crate::leds::RAPID),
        ("X", Some("^"), buttons::JOG_XP, crate::leds::JOG_XP),
        ("Y", Some("v"), buttons::JOG_YN, crate::leds::JOG_YN),
        ("Z", Some("^"), buttons::JOG_ZP, crate::leds::JOG_ZP),
        ("A", Some("^"), buttons::JOG_AP, crate::leds::JOG_AP),
    ];
    // The left group is 3 columns; cycle keys occupy the bottom row.
    let left_rects = [
        (28.0, 500.0),
        (188.0, 500.0),
        (348.0, 500.0),
        (28.0, 570.0),
        (188.0, 570.0),
        (348.0, 570.0),
        (28.0, 640.0),
        (188.0, 640.0),
        (348.0, 640.0),
        (28.0, 710.0),
        (188.0, 710.0),
        (348.0, 710.0),
    ];
    for ((name, sub, mask, led), (x, y)) in left.iter().zip(left_rects.iter()) {
        let fill = if *mask == buttons::RAPID {
            Color32::from_rgb(0x8a, 0x75, 0x18)
        } else {
            KEY_BG
        };
        if panel_key(
            ui,
            l,
            state,
            *x,
            *y,
            145.0,
            60.0,
            name,
            *sub,
            *mask,
            fill,
            Some(*led),
        ) {
            handle.set(|s| s.set_button(*mask, true));
            *pressed |= *mask;
        }
    }
    if round_key(
        ui,
        l,
        95.0,
        830.0,
        42.0,
        "CYCLE START",
        Color32::from_rgb(0xf0, 0xf0, 0xeb),
        "cycle-start",
        state.leds & crate::leds::CYCLE_ON != 0,
    ) {
        handle.set(|s| s.set_button(buttons::CYCLE_START, true));
        *pressed |= buttons::CYCLE_START;
    }
    if round_key(
        ui,
        l,
        220.0,
        830.0,
        42.0,
        "CYCLE STOP",
        Color32::from_rgb(0x10, 0x12, 0x13),
        "cycle-stop",
        state.leds & crate::leds::CYCLE_OFF != 0,
    ) {
        handle.set(|s| s.set_button(buttons::CYCLE_STOP, true));
        *pressed |= buttons::CYCLE_STOP;
    }
    lamp(
        painter,
        l,
        360.0,
        815.0,
        9.0,
        state.leds & crate::leds::CYCLE_ON != 0,
        GREEN,
    );

    let middle = [
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
    draw_key_grid(ui, l, state, &middle, 540.0, 500.0, 3, handle, pressed);

    let right = [
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
    draw_key_grid(ui, l, state, &right, 1050.0, 500.0, 3, handle, pressed);

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

fn draw_key_grid(
    ui: &egui::Ui,
    l: &PanelLayout,
    state: &SimPanelState,
    keys: &[(&str, u64, Option<u64>)],
    x0: f32,
    y0: f32,
    columns: usize,
    handle: &mut SimPanelHandle,
    pressed: &mut u64,
) {
    for (index, (name, mask, led)) in keys.iter().enumerate() {
        let col = index % columns;
        let row = index / columns;
        let x = x0 + col as f32 * 150.0;
        let y = y0 + row as f32 * 70.0;
        let fill = if *mask == buttons::SPD_SP {
            Color32::from_rgb(0x8d, 0x1c, 0x18)
        } else if *mask == buttons::CORRIDOR {
            Color32::from_rgb(0x8a, 0x62, 0x18)
        } else {
            KEY_BG
        };
        if panel_key(
            ui, l, state, x, y, 145.0, 60.0, name, None, *mask, fill, *led,
        ) {
            handle.set(|s| s.set_button(*mask, true));
            *pressed |= *mask;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
