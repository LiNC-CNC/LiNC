//! LiNC egui shell, theme, screens and widgets.

pub mod assets;
pub mod files;
pub mod screens;
pub mod shell;
pub mod state;
pub mod theme;
pub mod udisks2;

pub use shell::{FocusRegion, LincApp, NAV_ITEMS};
pub use state::{LocalBackend, Page, ProgramView, UiBackend};
