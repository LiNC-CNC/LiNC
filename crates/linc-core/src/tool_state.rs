//! Machine state that outlives a LiNC session.
//!
//! LinuxCNC starts a fresh interpreter with `tool_in_spindle = 0`, even when
//! the previous session left a tool in the spindle. That number is machine
//! state, not a UI preference, so it lives in its own file beside the
//! preferences file:
//!
//! `sim.preferences.toml` -> `sim.state.toml`
//!
//! The backend records every tool the controller reports, including `0` after
//! an explicit unload, and restores a remembered nonzero tool on the next
//! session. The write is atomic, so a crash cannot leave a torn state file.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file LinuxCNC does not keep for us: the last tool in the spindle.
#[derive(Clone, Debug, Default)]
pub struct ToolState {
    path: PathBuf,
    tool: Option<u16>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTool {
    #[serde(default)]
    tool: u16,
}

impl ToolState {
    /// Load the remembered tool. A missing or unreadable file means no memory,
    /// not an error: the controller is still authoritative.
    pub fn load(path: PathBuf) -> Self {
        let tool = fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str::<StoredTool>(&text).ok())
            .map(|stored| stored.tool);
        Self { path, tool }
    }

    /// The remembered tool number, if a state file carries one.
    pub fn tool(&self) -> Option<u16> {
        self.tool
    }

    /// Remember `tool`. A write failure is logged, not returned: losing the
    /// memory of a tool must not stop the machine or the HMI.
    pub fn record(&mut self, tool: u16) {
        if self.tool == Some(tool) {
            return;
        }
        if let Err(error) = self.write(tool) {
            log::warn!(
                "cannot write machine state {}: {error}",
                self.path.display()
            );
        }
        self.tool = Some(tool);
    }

    fn write(&self, tool: u16) -> std::io::Result<()> {
        let Some(parent) = self.path.parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "machine-state path has no parent directory",
            ));
        };
        fs::create_dir_all(parent)?;
        let body = format!(
            "# LiNC machine state; written by the HMI. Edit only while LiNC is stopped.\n\
             tool = {tool}\n"
        );
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(body.as_bytes())?;
        temp.flush()?;
        temp.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

/// State path derived from the preferences path: `part.preferences.toml`
/// becomes `part.state.toml` in the same directory.
pub fn state_path(preferences_path: &Path) -> PathBuf {
    let parent = preferences_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = preferences_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("linc");
    let stem = stem.strip_suffix(".preferences").unwrap_or(stem);
    parent.join(format!("{stem}.state.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_remembered_tool_round_trips_and_t0_is_a_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sim.state.toml");
        let mut state = ToolState::load(path.clone());
        assert_eq!(state.tool(), None, "no file means no memory");

        state.record(12);
        assert_eq!(ToolState::load(path.clone()).tool(), Some(12));

        state.record(0);
        assert_eq!(
            ToolState::load(path).tool(),
            Some(0),
            "an explicit unload is remembered, not treated as no file"
        );
    }

    #[test]
    fn an_unreadable_file_leaves_the_controller_authoritative() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sim.state.toml");
        fs::write(&path, "tool = \"not a number\"\n").unwrap();
        assert_eq!(ToolState::load(path).tool(), None);
    }

    #[test]
    fn the_state_file_sits_beside_the_preferences_file() {
        assert_eq!(
            state_path(Path::new(
                "/home/operator/.config/linc/sim.preferences.toml"
            )),
            Path::new("/home/operator/.config/linc/sim.state.toml")
        );
        assert_eq!(
            state_path(Path::new("/tmp/custom.toml")),
            Path::new("/tmp/custom.state.toml")
        );
    }
}
