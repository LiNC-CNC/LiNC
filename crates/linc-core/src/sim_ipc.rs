//! Local IPC between the standalone simulation panel service and the HMI.
//!
//! In a real machine the panel service owns `cnc_panel.*` HAL pins and the HMI
//! reads them through LinuxCNC. A standalone simulation has no HAL, so the two
//! processes exchange two atomically-replaced JSON files in the user runtime
//! directory:
//!
//! * `*.state.json`   panel -> HMI: the decoded `PanelSnapshot`
//! * `*.display.json` HMI -> panel: LEDs, tool displays, status display, velocity
//!
//! Writes use a temporary file in the same directory plus rename, so readers
//! never observe a torn document.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::model::{PanelDisplay, PanelSnapshot};

#[derive(Clone, Debug)]
pub struct SimPanelIpc {
    state_path: PathBuf,
    display_path: PathBuf,
    last_state: Option<(SystemTime, u64)>,
    last_display: Option<(SystemTime, u64)>,
    last_display_json: Option<String>,
}

impl Default for SimPanelIpc {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SimPanelIpc {
    /// Base path is `LINC_SIM_IPC` when set, otherwise
    /// `$XDG_RUNTIME_DIR/linc/panel-sim`.
    pub fn from_env() -> Self {
        let base = std::env::var_os("LINC_SIM_IPC")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/tmp"));
                runtime.join("linc").join("panel-sim")
            });
        Self::new(&base)
    }

    pub fn new(base: &Path) -> Self {
        let mut state = base.as_os_str().to_os_string();
        state.push(".state.json");
        let mut display = base.as_os_str().to_os_string();
        display.push(".display.json");
        Self {
            state_path: PathBuf::from(state),
            display_path: PathBuf::from(display),
            last_state: None,
            last_display: None,
            last_display_json: None,
        }
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    pub fn display_path(&self) -> &Path {
        &self.display_path
    }

    /// Panel service side: publish the decoded panel state.
    pub fn publish(&self, snapshot: &PanelSnapshot) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(snapshot)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        atomic_write(&self.state_path, &bytes)
    }

    /// HMI side: read the newest panel state; `None` when unchanged/unavailable.
    pub fn read_state(&mut self) -> Option<PanelSnapshot> {
        let bytes = read_if_changed(&self.state_path, &mut self.last_state)?;
        serde_json::from_slice(&bytes).ok()
    }

    /// HMI side: publish display values for the panel window.
    pub fn write_display(&mut self, display: &PanelDisplay) -> std::io::Result<()> {
        let json = serde_json::to_string(display)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if self.last_display_json.as_deref() == Some(json.as_str()) {
            return Ok(());
        }
        atomic_write(&self.display_path, json.as_bytes())?;
        self.last_display_json = Some(json);
        Ok(())
    }

    /// Panel service side: read the newest display command; `None` when unchanged.
    pub fn read_display(&mut self) -> Option<PanelDisplay> {
        let bytes = read_if_changed(&self.display_path, &mut self.last_display)?;
        serde_json::from_slice(&bytes).ok()
    }
}

fn read_if_changed(path: &Path, last: &mut Option<(SystemTime, u64)>) -> Option<Vec<u8>> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let len = metadata.len();
    if last
        .map(|(time, size)| time == modified && size == len)
        .unwrap_or(false)
    {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    *last = Some((modified, len));
    Some(bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PanelMode, PanelModeValidity};

    #[test]
    fn state_roundtrip_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let ipc = SimPanelIpc::new(&dir.path().join("panel-sim"));
        let mut panel = PanelSnapshot {
            connected: true,
            mode: PanelModeValidity::Valid(PanelMode::Mdi),
            feed_override: 0.8,
            power_on: Some(true),
            ..Default::default()
        };
        ipc.publish(&panel).unwrap();
        let mut reader = ipc.clone();
        let read = reader.read_state().unwrap();
        assert!(read.connected);
        assert_eq!(read.mode, PanelModeValidity::Valid(PanelMode::Mdi));
        assert_eq!(read.power_on, Some(true));
        // Unchanged file returns None.
        assert!(reader.read_state().is_none());
        panel.mode = PanelModeValidity::Valid(PanelMode::Auto);
        ipc.publish(&panel).unwrap();
        assert_eq!(
            reader.read_state().unwrap().mode,
            PanelModeValidity::Valid(PanelMode::Auto)
        );
    }

    #[test]
    fn display_roundtrip_and_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let mut ipc = SimPanelIpc::new(&dir.path().join("panel-sim"));
        let display = PanelDisplay {
            leds: 0b101,
            sseg_tool: 12,
            sseg_next_tool: 13,
            sseg_status: 55,
            vel: [1.0, 2.0, 3.0],
            homing: false,
        };
        ipc.write_display(&display).unwrap();
        let mut panel = ipc.clone();
        assert_eq!(panel.read_display(), Some(display));
        assert!(panel.read_display().is_none());
    }
}
