//! Offline LinuxCNC G-code reference bundle.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::gcode::normalize_search;

pub const BUNDLE_JSON: &str = include_str!("../assets/gcode_help.json");

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HelpBundle {
    pub source: String,
    pub bundle: String,
    pub version: String,
    pub attribution: String,
    pub counts: HelpCounts,
    pub groups: Vec<HelpGroup>,
    pub sections: BTreeMap<String, HelpSection>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct HelpCounts {
    pub sections: usize,
    pub groups: usize,
    pub rows: usize,
    pub figures: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HelpGroup {
    pub name: String,
    pub note: String,
    pub rows: Vec<HelpRow>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HelpRow {
    pub anchor: String,
    pub code: String,
    pub params: String,
    pub desc: String,
    pub meaning: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HelpSection {
    pub title: String,
    pub blocks: Vec<HelpBlock>,
    pub related: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HelpBlock {
    Heading {
        text: String,
        #[serde(default)]
        level: u8,
    },
    Para {
        text: String,
    },
    Code {
        text: String,
    },
    Table {
        rows: Vec<Vec<String>>,
    },
    List {
        items: Vec<HelpItem>,
    },
    Image {
        image: String,
        #[serde(default)]
        text: String,
    },
    Note {
        text: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HelpItem {
    ListItem {
        text: String,
    },
    Image {
        image: String,
        #[serde(default)]
        text: String,
    },
}

impl HelpBundle {
    pub fn load_bundled() -> Result<Self, serde_json::Error> {
        serde_json::from_str(BUNDLE_JSON)
    }

    pub fn section(&self, anchor: &str) -> Option<&HelpSection> {
        self.sections.get(anchor)
    }

    pub fn row(&self, anchor: &str) -> Option<&HelpRow> {
        self.groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .find(|row| row.anchor == anchor)
    }

    /// Search code, parameters, description, meaning and group names.
    /// Code matching removes spaces and commas, so `g2g3` finds `G2, G3`.
    pub fn filter(&self, query: &str) -> Vec<HelpGroup> {
        let query = query.trim();
        if query.is_empty() {
            return self.groups.clone();
        }
        let lower = query.to_ascii_lowercase();
        let normalized = normalize_search(query);
        let mut out = Vec::new();
        for group in &self.groups {
            let group_text = format!("{} {}", group.name, group.note).to_ascii_lowercase();
            let mut rows = Vec::new();
            for row in &group.rows {
                let hay = format!("{} {} {} {}", row.code, row.params, row.desc, row.meaning)
                    .to_ascii_lowercase();
                if (!normalized.is_empty() && normalize_search(&row.code).contains(&normalized))
                    || hay.contains(&lower)
                    || group_text.contains(&lower)
                {
                    rows.push(row.clone());
                }
            }
            if !rows.is_empty() {
                out.push(HelpGroup {
                    name: group.name.clone(),
                    note: group.note.clone(),
                    rows,
                });
            }
        }
        out
    }

    /// Anchors reachable from a section, including internal document links.
    pub fn related_anchors(&self, anchor: &str) -> Vec<String> {
        self.section(anchor)
            .map(|section| section.related.clone())
            .unwrap_or_default()
    }

    pub fn all_anchors(&self) -> BTreeSet<String> {
        self.groups
            .iter()
            .flat_map(|group| group.rows.iter().map(|row| row.anchor.clone()))
            .collect()
    }

    pub fn figures(&self) -> Vec<&str> {
        let mut figures = BTreeSet::new();
        for section in self.sections.values() {
            for block in &section.blocks {
                match block {
                    HelpBlock::Image { image, .. } => {
                        figures.insert(image.as_str());
                    }
                    HelpBlock::List { items } => {
                        for item in items {
                            if let HelpItem::Image { image, .. } = item {
                                figures.insert(image.as_str());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        figures.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_matches_the_accepted_contents() {
        let bundle = HelpBundle::load_bundled().unwrap();
        assert_eq!(bundle.counts.sections, 125);
        assert_eq!(bundle.counts.rows, 89);
        assert_eq!(bundle.counts.figures, 14);
        assert_eq!(bundle.figures().len(), 14);
        assert!(bundle.section("gcode:g2-g3").is_some());
    }

    #[test]
    fn code_search_ignores_spaces_and_commas() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let groups = bundle.filter("g2g3");
        assert!(groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .any(|row| row.code == "G2, G3"));
    }

    #[test]
    fn text_search_finds_descriptions() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let groups = bundle.filter("dwell");
        assert!(groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .any(|row| row.code == "G4"));
    }

    #[test]
    fn related_links_stay_inside_bundle() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let related = bundle.related_anchors("gcode:g41-g42");
        assert!(related
            .iter()
            .all(|anchor| bundle.section(anchor).is_some()));
    }
}
