//! The diagnostics channel `linc-mcp` reads: the published snapshot, the
//! region rects, and the screenshot capture.

use super::*;

/// Encode an egui capture as a PNG.
pub(super) fn png_from_color_image(
    image: &egui::ColorImage,
) -> Option<linc_core::diag::Screenshot> {
    let [width, height] = image.size;
    if width == 0 || height == 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(width * height * 4);
    for pixel in &image.pixels {
        bytes.extend_from_slice(&pixel.to_array());
    }
    let image = image::RgbaImage::from_raw(width as u32, height as u32, bytes)?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(linc_core::diag::Screenshot {
        png,
        width: width as u32,
        height: height as u32,
        at_ms: linc_core::diag::now_ms(),
    })
}

impl LincApp {
    /// Publish the latest state for the diagnostic server.
    ///
    /// A new backend sample is published at once; between samples the bus is
    /// refreshed at 5 Hz so `age_ms` stays small and a waiting agent can rely on
    /// it, without cloning the snapshot on every repaint.
    pub(super) fn publish_diag(&mut self) {
        let Some(bus) = &self.diag_bus else { return };
        let Ok(mut bus) = bus.lock() else { return };
        let sample = self.snapshot.meta.captured_at_ms;
        let stale = linc_core::diag::now_ms() - bus.captured_at_ms() >= 200;
        if sample != self.diagnostics.last_sample || stale {
            self.diagnostics.last_sample = sample;
            bus.publish_machine(self.snapshot.clone());
        }
        bus.publish_ui(self.diag_ui_snapshot());
    }

    /// Apply the intents an agent queued through `diag.ui_action`.
    ///
    /// These land before the frame draws, so a page switch and the following
    /// screenshot show the same state.
    pub(super) fn apply_diag_actions(&mut self) {
        let Some(bus) = &self.diag_bus else { return };
        let actions = bus
            .lock()
            .map(|mut bus| bus.take_ui_actions())
            .unwrap_or_default();
        for action in actions {
            match action {
                linc_core::diag::UiAction::Page { page } => {
                    if let Some(page) = page_from_name(&page) {
                        self.request_page(page);
                    } else {
                        self.toast(format!("unknown page {page}"), theme::WARNING);
                    }
                }
                linc_core::diag::UiAction::CycleView => self.cycle_toolpath_view(),
                linc_core::diag::UiAction::ShowBounds { visible } => {
                    self.graph_style.bounds_mode = if visible { 0 } else { 3 };
                    self.prefs.preferences.bounds_mode = Some(self.graph_style.bounds_mode);
                    self.prefs.dirty = true;
                }
                linc_core::diag::UiAction::CycleBounds => self.cycle_bounds(),
                linc_core::diag::UiAction::Fit => self.fit_graph(),
                linc_core::diag::UiAction::ClearTrail => self.clear_trail(),
                linc_core::diag::UiAction::SelectLine { line } => self.jump_to_line(line),
            }
        }
    }

    /// Answer a pending `diag.screenshot`.
    ///
    /// egui delivers the capture through a callback after the frame is painted,
    /// which is the only point where the rendered image exists: reading the
    /// window's frame buffer during layout returns the cleared buffer.
    pub(super) fn capture_screenshot(&self, ctx: &egui::Context) {
        let Some(bus) = &self.diag_bus else { return };
        let requested = bus
            .lock()
            .map(|mut bus| bus.take_screenshot_request())
            .unwrap_or(false);
        if !requested {
            return;
        }
        let bus = bus.clone();
        ctx.request_screenshot(move |image| {
            let Some(screenshot) = png_from_color_image(&image) else {
                log::warn!("screenshot captured but could not be encoded");
                return;
            };
            if let Ok(mut bus) = bus.lock() {
                bus.put_screenshot(screenshot);
            }
        });
    }

    /// The UI-side state an agent needs; mirrors `linc_core::diag::UiSnapshot`.
    pub(super) fn diag_ui_snapshot(&self) -> linc_core::diag::UiSnapshot {
        use linc_core::diag::UiSnapshot;
        let layout = match self.profile.variant {
            linc_core::model::MachineVariant::Lathe => "lathe",
            linc_core::model::MachineVariant::Mill => "mill",
        };
        // The controller mode is what the top bar shows; a panel mode that has
        // not decoded yet must not hide it.
        let mode = format!("{:?}", self.snapshot.task.mode);
        UiSnapshot {
            page: self.nav.page.label().into(),
            layout: layout.into(),
            mode,
            program_view: match self.program.view {
                crate::state::ProgramView::Split => "SPLIT".into(),
                crate::state::ProgramView::Code => "CODE".into(),
            },
            view: self.program.graph_view.label().into(),
            zoom: self.program.graph_zoom,
            selected_line: (self.program.selected_line > 0).then_some(self.program.selected_line),
            current_line: {
                let line = self.snapshot.program.executing_line();
                (line > 0).then_some(line)
            },
            selected_axis: None,
            program_name: self.snapshot.program.display_name.clone(),
            program_dirty: self.program.editor.dirty,
            panel_online: self.snapshot.panel.connected,
            fps: self.fps,
            toasts: self
                .overlay
                .toasts
                .iter()
                .map(|toast| toast.text.clone())
                .collect(),
            viewport: self.diagnostics.viewport.clone(),
            regions: self.diagnostics.regions.clone(),
            frame_time_ms: self.diagnostics.frame_time_ms,
        }
    }

    /// Record one shell rectangle for the screen model, in points.
    pub(super) fn note_region(&mut self, name: &str, rect: egui::Rect) {
        self.diagnostics
            .regions
            .push(linc_core::diag::ScreenRegion::new(
                name,
                rect.min.x,
                rect.min.y,
                rect.width(),
                rect.height(),
            ));
    }

    /// Attach the diagnostic bus the app server publishes into.
    pub fn set_diag_bus(&mut self, bus: std::sync::Arc<std::sync::Mutex<linc_core::diag::Bus>>) {
        self.diag_bus = Some(bus);
    }
}

/// Diagnostics page state and the bus bookkeeping `linc-mcp` reads.
pub(crate) struct DiagnosticsUi {
    /// Backend sample stamp of the last published machine snapshot.
    pub(super) last_sample: u64,
    pub tab: DiagnosticsTab,
    pub search: String,
    pub severity: Option<AlarmSeverity>,
    pub source: Option<AlarmSource>,
    pub clear_view: bool,
    pub log: VecDeque<Alarm>,
    /// Window size and scale of the last frame, for `linc_get_screen_model`.
    pub viewport: linc_core::diag::ViewportInfo,
    /// Named shell rectangles from the last frame.
    pub regions: Vec<linc_core::diag::ScreenRegion>,
    pub frame_time_ms: f32,
}
