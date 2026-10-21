//! Runtime UI preferences, stored separately from the machine profile.
//!
//! CONFIGURATION.md §9: `~/.config/linc/<profile>.preferences.toml` holds
//! window geometry, the last page, the program view, the toolpath view angle,
//! recent files and visible panels. Machine identity, HAL pins, safety flags
//! and the LinuxCNC INI path are never read from here — they come from the
//! profile and the INI.
//!
//! Writes go to a temporary file in the destination directory and are renamed
//! into place, so a crash mid-write cannot leave a torn preferences file.

use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Window geometry. Restored on the next launch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowGeometry {
    pub width: f32,
    pub height: f32,
}

/// The complete set of persisted UI preferences. Anything not listed here is
/// deliberately not persistable.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowGeometry>,
    /// Last non-mode-driven page, e.g. `program`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_page: Option<String>,
    /// `split` or `code`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_view: Option<String>,
    /// `iso3d`, `xy`, `xz` or `lathe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolpath_view: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolpath_zoom: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_files: Vec<String>,
    /// Page keys the operator has chosen to show.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_panels: Vec<String>,
}

impl Preferences {
    pub fn load(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        match toml::from_str::<Self>(&text) {
            Ok(preferences) => preferences,
            Err(error) => {
                log::warn!("ignoring unreadable preferences {}: {error}", path.display());
                Self::default()
            }
        }
    }

    /// Write atomically: temporary file in the destination directory, then
    /// rename. Never leaves a partial file behind.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let Some(parent) = path.parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "preferences path has no parent directory",
            ));
        };
        fs::create_dir_all(parent)?;
        let body = toml::to_string_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(body.as_bytes())?;
        temp.flush()?;
        temp.persist(path)
            .map_err(|error| error.error)?;
        Ok(())
    }

    /// Record a recently opened program, most recent first, bounded.
    pub fn remember_file(&mut self, path: &str, limit: usize) {
        self.recent_files.retain(|entry| entry != path);
        self.recent_files.insert(0, path.to_string());
        self.recent_files.truncate(limit.max(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_unknown_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sim.preferences.toml");
        let mut preferences = Preferences {
            last_page: Some("program".into()),
            program_view: Some("code".into()),
            toolpath_zoom: Some(1.5),
            ..Default::default()
        };
        preferences.remember_file("/nc/demo.nc", 8);
        preferences.save(&path).expect("save");

        let loaded = Preferences::load(&path);
        assert_eq!(loaded, preferences);

        // A preferences file must not be able to smuggle machine identity,
        // HAL pins or safety flags.
        fs::write(&path, "last_page = \"tools\"\nlinuxcnc_ini = \"/etc/passwd\"\n").unwrap();
        let loaded = Preferences::load(&path);
        assert_eq!(loaded, Preferences::default());
    }

    #[test]
    fn recent_files_are_deduplicated_and_bounded() {
        let mut preferences = Preferences::default();
        for index in 0..12 {
            preferences.remember_file(&format!("/nc/{index}.nc"), 5);
        }
        assert_eq!(preferences.recent_files.len(), 5);
        assert_eq!(preferences.recent_files[0], "/nc/11.nc");
        preferences.remember_file("/nc/8.nc", 5);
        assert_eq!(preferences.recent_files[0], "/nc/8.nc");
        assert_eq!(preferences.recent_files.len(), 5);
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("absent.toml");
        assert_eq!(Preferences::load(&path), Preferences::default());
    }
}
