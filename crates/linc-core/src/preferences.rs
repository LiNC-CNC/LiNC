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

use std::collections::BTreeMap;
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
    /// Index into `[DISPLAY] INCREMENTS`; 0 is continuous jog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jog_increment: Option<usize>,
    /// Jog speed as a percentage of `[DISPLAY] DEFAULT_LINEAR_VELOCITY`.
    ///
    /// No longer read: the jog follows the feed override. Kept so an existing
    /// preferences file still parses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jog_speed_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_files: Vec<String>,
    /// `Ctrl+B` cycle position: 0 dimensions only, 1 + machine box, 2 + program
    /// box, 3 all off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds_mode: Option<u8>,
    /// Lathe only: mirror X about the Z centreline in the XZ/TURN toolpath
    /// (`Ctrl+M`). Screen only — no coordinate and no machine value changes.
    /// Absent from a file written before this key existed, which reads as off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lathe_mirror: Option<bool>,
    /// Lathe only: show the X position as a diameter (`2X`) instead of a radius
    /// (`Ctrl+E`). A display factor; the controller still works in radius.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lathe_diameter: Option<bool>,
    /// Page keys the operator has chosen to show.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_panels: Vec<String>,
    /// LiNC-only tool type labels, keyed by tool number as text (`"12"`).
    ///
    /// The LinuxCNC tool table has no type column: LiNC infers MILL/DRILL from
    /// the diameter and TURN/BORING/TOOL from the lathe geometry. The operator
    /// can override the label here; it is presentation, never machine data.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_kinds: BTreeMap<String, String>,
    /// Typed probing feeds, clearances, hints and UI mode. Probe enablement,
    /// tool identity and measured calibration remain profile/controller-owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<crate::probe::ProbeParameters>,
}

impl Preferences {
    pub fn load(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        match toml::from_str::<Self>(&text) {
            Ok(preferences) => preferences,
            Err(error) => {
                log::warn!(
                    "ignoring unreadable preferences {}: {error}",
                    path.display()
                );
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
        temp.persist(path).map_err(|error| error.error)?;
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
        fs::write(
            &path,
            "last_page = \"tools\"\nlinuxcnc_ini = \"/etc/passwd\"\n",
        )
        .unwrap();
        let loaded = Preferences::load(&path);
        assert_eq!(loaded, Preferences::default());
    }

    #[test]
    fn lathe_display_toggles_round_trip_and_an_old_file_still_parses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lathe.preferences.toml");
        let preferences = Preferences {
            lathe_mirror: Some(true),
            lathe_diameter: Some(false),
            ..Default::default()
        };
        preferences.save(&path).expect("save");
        assert_eq!(Preferences::load(&path), preferences);

        // A file from before the two keys existed: every present key is kept
        // and the new ones read as absent, not as an unreadable file.
        fs::write(
            &path,
            "last_page = \"machine\"\nbounds_mode = 1\ntoolpath_view = \"lathe\"\n",
        )
        .unwrap();
        let loaded = Preferences::load(&path);
        assert_eq!(loaded.last_page.as_deref(), Some("machine"));
        assert_eq!(loaded.bounds_mode, Some(1));
        assert_eq!(loaded.lathe_mirror, None);
        assert_eq!(loaded.lathe_diameter, None);
    }

    #[test]
    fn tool_kind_labels_survive_a_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sim.preferences.toml");
        let mut preferences = Preferences::default();
        preferences.tool_kinds.insert("12".into(), "DRILL".into());
        preferences.save(&path).expect("save");
        let loaded = Preferences::load(&path);
        assert_eq!(
            loaded.tool_kinds.get("12").map(String::as_str),
            Some("DRILL")
        );
    }

    #[test]
    fn probe_parameters_round_trip_without_machine_owned_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("probe.preferences.toml");
        let mut probe = crate::probe::ProbeParameters::default();
        probe.probe_tool_number = 42;
        probe.fast_feed = 375.0;
        probe.slow_feed = 0.0;
        probe.diameter_hint = 62.5;
        probe.calibration_offset = 0.125;
        probe.wcs_rotation = true;
        let preferences = Preferences {
            probe: Some(probe),
            ..Default::default()
        };
        preferences.save(&path).expect("save");
        let text = fs::read_to_string(&path).expect("read");
        assert!(!text.contains("probe_tool_number"));
        assert!(!text.contains("calibration_offset"));
        let loaded = Preferences::load(&path).probe.expect("probe preferences");
        assert_eq!(loaded.probe_tool_number, crate::probe::EXPECTED_PROBE_TOOL);
        assert_eq!(loaded.calibration_offset, 0.0);
        assert_eq!(loaded.fast_feed, 375.0);
        assert_eq!(loaded.slow_feed, 0.0);
        assert_eq!(loaded.diameter_hint, 62.5);
        assert!(loaded.wcs_rotation);
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
