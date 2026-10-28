//! Panel-only homing and PROG.HOME mapping.
//!
//! The HMI displays state and documents the mapping. It never issues home
//! commands and never intercepts the panel's PROG.HOME MDI path.

use crate::ini::AxisIni;
use crate::model::{AxisMap, MachineSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomingKey {
    Rapid,
    XPlus,
    XMinus,
    YPlus,
    YMinus,
    ZPlus,
    ZMinus,
}

impl HomingKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rapid => "RAPID",
            Self::XPlus => "X+",
            Self::XMinus => "X-",
            Self::YPlus => "Y+",
            Self::YMinus => "Y-",
            Self::ZPlus => "Z+",
            Self::ZMinus => "Z-",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HomeMapping {
    pub key: HomingKey,
    pub home_pin: &'static str,
    pub home_action: &'static str,
    pub proghome_pin: &'static str,
    pub proghome_mdi: &'static str,
}

pub const MAPPINGS: [HomeMapping; 7] = [
    HomeMapping {
        key: HomingKey::Rapid,
        home_pin: "cnc_panel.home-all",
        home_action: "HOME ALL",
        proghome_pin: "cnc_panel.proghome-all",
        // The machine's own routine: G53 G0 Z0 first, then work X0 Y0. A single
        // G0 X0Y0Z0 would plunge X and Y at whatever Z the tool is at.
        proghome_mdi: "o<whome> call",
    },
    HomeMapping {
        key: HomingKey::XPlus,
        home_pin: "cnc_panel.home-x",
        home_action: "HOME X",
        proghome_pin: "cnc_panel.proghome-x",
        proghome_mdi: "G0 X0",
    },
    HomeMapping {
        key: HomingKey::XMinus,
        home_pin: "cnc_panel.home-x",
        home_action: "HOME X",
        proghome_pin: "cnc_panel.proghome-x",
        proghome_mdi: "G0 X0",
    },
    HomeMapping {
        key: HomingKey::YPlus,
        home_pin: "cnc_panel.home-y",
        home_action: "HOME Y",
        proghome_pin: "cnc_panel.proghome-y",
        proghome_mdi: "G0 Y0",
    },
    HomeMapping {
        key: HomingKey::YMinus,
        home_pin: "cnc_panel.home-y",
        home_action: "HOME Y",
        proghome_pin: "cnc_panel.proghome-y",
        proghome_mdi: "G0 Y0",
    },
    HomeMapping {
        key: HomingKey::ZPlus,
        home_pin: "cnc_panel.home-z",
        home_action: "HOME Z",
        proghome_pin: "cnc_panel.proghome-z",
        proghome_mdi: "G0 Z0",
    },
    HomeMapping {
        key: HomingKey::ZMinus,
        home_pin: "cnc_panel.home-z",
        home_action: "HOME Z",
        proghome_pin: "cnc_panel.proghome-z",
        proghome_mdi: "G0 Z0",
    },
];

pub fn mapping(key: HomingKey) -> HomeMapping {
    MAPPINGS
        .iter()
        .copied()
        .find(|mapping| mapping.key == key)
        .expect("all home keys mapped")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisHomeStatus {
    Homed,
    Homing,
    NotHomed,
    Unknown,
    NotConfigured,
}

impl AxisHomeStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Homed => "HOMED",
            Self::Homing => "HOMING",
            Self::NotHomed => "NOT HOMED",
            Self::Unknown => "UNKNOWN",
            Self::NotConfigured => "NOT CONFIGURED",
        }
    }
}

#[derive(Clone, Debug)]
pub struct HomeAxisRow {
    pub name: String,
    pub status: AxisHomeStatus,
    pub sequence: u32,
    pub use_index: bool,
    pub search_vel: f64,
    pub latch_vel: f64,
    pub position: f64,
}

pub fn axis_rows(snapshot: &MachineSnapshot, ini_axes: &[AxisIni]) -> Vec<HomeAxisRow> {
    ini_axes
        .iter()
        .map(|axis| {
            let status = if snapshot
                .machine
                .homing
                .get(&axis.name)
                .copied()
                .unwrap_or(false)
            {
                AxisHomeStatus::Homing
            } else {
                match snapshot.machine.homed.get(&axis.name).copied() {
                    Some(true) => AxisHomeStatus::Homed,
                    Some(false) => AxisHomeStatus::NotHomed,
                    None => AxisHomeStatus::Unknown,
                }
            };
            HomeAxisRow {
                name: axis.name.clone(),
                status,
                sequence: axis.home_sequence,
                use_index: axis.home_use_index,
                search_vel: axis.home_search_vel,
                latch_vel: axis.home_latch_vel,
                position: snapshot
                    .positions
                    .axes
                    .get(&axis.name)
                    .map(|position| position.work)
                    .unwrap_or(0.0),
            }
        })
        .collect()
}

pub fn all_homed(homed: &AxisMap<bool>) -> bool {
    !homed.is_empty() && homed.iter().all(|(_, value)| *value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings_match_the_machine_contract() {
        assert_eq!(mapping(HomingKey::Rapid).proghome_mdi, "o<whome> call");
        assert_eq!(mapping(HomingKey::XPlus).proghome_mdi, "G0 X0");
        assert_eq!(mapping(HomingKey::YMinus).proghome_mdi, "G0 Y0");
        assert_eq!(mapping(HomingKey::ZPlus).home_pin, "cnc_panel.home-z");
    }

    #[test]
    fn homed_requires_every_configured_axis() {
        let mut homed = AxisMap::new();
        homed.insert("X", true);
        homed.insert("Y", false);
        assert!(!all_homed(&homed));
        homed.insert("Y", true);
        assert!(all_homed(&homed));
    }
}
