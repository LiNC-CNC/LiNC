//! Dense tables whose column widths are resolved once.
//!
//! `egui::Grid` sizes each column to its widest cell, which is why a table of
//! editable cells lines up on nothing: a header text, a text field with its own
//! minimum and a button each end at a different x. Here every header and every
//! cell is given exactly its column width, so the rows share one grid. The model
//! follows SolidCraft's table (measured widths, one flexible column, minimum
//! content widths, horizontal scrolling on a narrow card).

use egui::{Align, Layout, Response, Sense, Ui, Vec2};

use crate::theme;

/// One column: its heading, its preferred width and the width it may not go
/// below.
pub struct Column<'a> {
    label: &'a str,
    preferred: f32,
    min: f32,
    flex: bool,
}

impl<'a> Column<'a> {
    /// Fixed-width column.
    pub fn new(label: &'a str, width: f32) -> Self {
        Self {
            label,
            preferred: width,
            min: width,
            flex: false,
        }
    }

    /// Column that takes the width left over after the fixed ones, never less
    /// than `min`.
    pub fn flex(label: &'a str, min: f32) -> Self {
        Self {
            label,
            preferred: min,
            min,
            flex: true,
        }
    }

    /// Raise the minimum, for a column whose cells need more room than its
    /// heading.
    pub fn min(mut self, min: f32) -> Self {
        self.min = self.min.max(min);
        self
    }

    pub fn label(&self) -> &str {
        self.label
    }
}

pub struct Table<'a> {
    columns: &'a [Column<'a>],
    widths: Vec<f32>,
    row_height: f32,
    gap: f32,
}

impl<'a> Table<'a> {
    /// Resolve the column widths against the space available.
    pub fn new(columns: &'a [Column<'a>], available: f32) -> Self {
        const GAP: f32 = 6.0;
        let gaps = GAP * columns.len().saturating_sub(1) as f32;
        let minimum: f32 = columns.iter().map(|column| column.min).sum::<f32>() + gaps;
        let fixed: f32 = columns
            .iter()
            .filter(|column| !column.flex)
            .map(|column| column.preferred)
            .sum();
        let flex_count = columns.iter().filter(|column| column.flex).count();
        let widths = if flex_count == 0 || minimum >= available {
            // Too narrow for the minimums: keep them and let the caller scroll.
            columns.iter().map(|column| column.min).collect()
        } else {
            let shared = ((available - gaps - fixed) / flex_count as f32).max(0.0);
            columns
                .iter()
                .map(|column| {
                    if column.flex {
                        shared.max(column.min)
                    } else {
                        column.preferred
                    }
                })
                .collect()
        };
        Self {
            columns,
            widths,
            row_height: 32.0,
            gap: GAP,
        }
    }

    pub fn row_height(mut self, height: f32) -> Self {
        self.row_height = height;
        self
    }

    pub fn total_width(&self) -> f32 {
        self.widths.iter().sum::<f32>() + self.gap * self.widths.len().saturating_sub(1) as f32
    }

    /// Heading row, on the same grid as the body.
    pub fn header(&self, ui: &mut Ui) {
        self.line(ui, false, |ui, row| {
            for (index, column) in self.columns.iter().enumerate() {
                row.cell(ui, index, Align::Min, |ui| {
                    ui.label(
                        egui::RichText::new(column.label)
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                });
            }
        });
    }

    /// One row. Returns its response so the caller can select it on click.
    pub fn row(
        &self,
        ui: &mut Ui,
        selected: bool,
        add: impl FnOnce(&mut Ui, &Row<'_>),
    ) -> Response {
        self.line(ui, selected, add)
    }

    pub fn width(&self, index: usize) -> f32 {
        self.widths.get(index).copied().unwrap_or(0.0)
    }

    fn line(
        &self,
        ui: &mut Ui,
        selected: bool,
        add: impl FnOnce(&mut Ui, &Row<'_>),
    ) -> Response {
        // Exactly the columns' width: `available_width` inside a horizontal
        // scroll area is unbounded, and taking it stretched every row past the
        // card and pushed the last column out of view.
        let width = self.total_width();
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, self.row_height), Sense::click());
        if selected {
            ui.painter().rect_stroke(
                rect,
                egui::CornerRadius::same(3),
                egui::Stroke::new(1.0, theme::BORDER),
                egui::StrokeKind::Inside,
            );
        }
        let row = Row {
            left: rect.left(),
            top: rect.top(),
            widths: &self.widths,
            gap: self.gap,
            row_height: self.row_height,
            clip: rect,
        };
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        child.set_clip_rect(rect);
        child.spacing_mut().item_spacing.x = 0.0;
        add(&mut child, &row);
        response
    }
}

/// The row being drawn, so a cell lands on its column's x rather than wherever
/// the layout happens to put it.
pub struct Row<'a> {
    left: f32,
    top: f32,
    widths: &'a [f32],
    gap: f32,
    row_height: f32,
    clip: egui::Rect,
}

impl Row<'_> {
    pub fn width(&self, index: usize) -> f32 {
        self.widths.get(index).copied().unwrap_or(0.0)
    }

    /// Draw one cell into its column rect.
    pub fn cell(&self, ui: &mut Ui, index: usize, align: Align, add: impl FnOnce(&mut Ui)) {
        let width = self.width(index);
        let offset: f32 = self.widths.iter().take(index).sum::<f32>() + self.gap * index as f32;
        let rect = egui::Rect::from_min_size(
            egui::pos2(self.left + offset, self.top),
            Vec2::new(width, self.row_height),
        );
        let layout = match align {
            Align::Max => Layout::right_to_left(Align::Center),
            Align::Center => Layout::centered_and_justified(egui::Direction::LeftToRight),
            _ => Layout::left_to_right(Align::Center),
        };
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(layout),
        );
        child.set_clip_rect(self.clip);
        child.spacing_mut().item_spacing.x = 0.0;
        add(&mut child);
    }
}
