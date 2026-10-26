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

/// One inline run of a paragraph, list item or admonition.
///
/// The generator collapses whitespace, so concatenating a block's spans
/// reproduces that block's plain text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct HelpSpan {
    pub text: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub code: bool,
    /// Internal document anchor this run links to, without the leading hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl HelpSpan {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The admonition types the LinuxCNC documentation uses, each carried by its
/// own cell colour in the source (Probe Basic's _ADMON_REMAP).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum HelpAdmonition {
    Note,
    Tip,
    Warning,
    Important,
    Caution,
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
        #[serde(default)]
        spans: Vec<HelpSpan>,
    },
    Code {
        text: String,
    },
    Table {
        rows: Vec<Vec<String>>,
        /// The first row is a header row, not data.
        #[serde(default)]
        header: bool,
    },
    List {
        items: Vec<HelpItem>,
    },
    Image {
        image: String,
        #[serde(default)]
        text: String,
    },
    Admonition {
        admonition: HelpAdmonition,
        text: String,
        #[serde(default)]
        spans: Vec<HelpSpan>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HelpItem {
    ListItem {
        text: String,
        #[serde(default)]
        spans: Vec<HelpSpan>,
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
    /// Code matching removes spaces and commas, so g2g3 finds G2, G3.

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

    fn block_spans(block: &HelpBlock) -> Vec<&HelpSpan> {
        match block {
            HelpBlock::Para { spans, .. } | HelpBlock::Admonition { spans, .. } => {
                spans.iter().collect()
            }
            HelpBlock::List { items } => items
                .iter()
                .flat_map(|item| match item {
                    HelpItem::ListItem { spans, .. } => spans.iter().collect::<Vec<_>>(),
                    HelpItem::Image { .. } => Vec::new(),
                })
                .collect(),
            _ => Vec::new(),
        }
    }

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

    #[test]
    fn spans_reconstruct_the_plain_text() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let mut checked = 0;
        for section in bundle.sections.values() {
            for block in &section.blocks {
                let plain = match block {
                    HelpBlock::Para { text, .. } | HelpBlock::Admonition { text, .. } => text,
                    HelpBlock::List { items } => {
                        for item in items {
                            if let HelpItem::ListItem { text, spans } = item {
                                let joined: String =
                                    spans.iter().map(|span| span.text.as_str()).collect();
                                assert_eq!(&joined, text, "{}", section.title);
                                checked += 1;
                            }
                        }
                        continue;
                    }
                    _ => continue,
                };
                let joined: String = block_spans(block)
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect();
                assert_eq!(&joined, plain, "{}", section.title);
                checked += 1;
            }
        }
        assert!(checked > 500, "expected the whole bundle, saw {checked}");
    }

    #[test]
    fn inline_spans_keep_emphasis_code_and_links() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let section = bundle.section("gcode:g2-g3").unwrap();
        let spans: Vec<&HelpSpan> = section
            .blocks
            .iter()
            .filter(|block| {
                matches!(block, HelpBlock::Para { spans, .. } if spans.iter().any(|span| span.link.is_some()))
            })
            .flat_map(block_spans)
            .collect();
        assert!(spans.iter().any(|span| span.text == "G2" && span.italic));
        assert!(spans.iter().any(|span| {
            span.text == "feed rate" && span.link.as_deref() == Some("sec:set-feed-rate")
        }));
    }

    #[test]
    fn bundle_carries_bold_code_and_italic_runs() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let spans: Vec<&HelpSpan> = bundle
            .sections
            .values()
            .flat_map(|section| section.blocks.iter())
            .flat_map(block_spans)
            .collect();
        assert!(spans.iter().any(|span| span.bold && !span.text.is_empty()));
        assert!(spans.iter().any(|span| span.code && !span.text.is_empty()));
        assert!(spans.iter().any(|span| span.italic && !span.text.is_empty()));
    }

    #[test]
    fn every_admonition_kind_decodes_to_its_own_type() {
        let cases = [
            ("warning", HelpAdmonition::Warning),
            ("note", HelpAdmonition::Note),
            ("tip", HelpAdmonition::Tip),
            ("important", HelpAdmonition::Important),
            ("caution", HelpAdmonition::Caution),
        ];
        for (name, kind) in cases {
            let json = format!(r#"{{"kind":"admonition","admonition":"{name}","text":"body"}}"#);
            let block: HelpBlock = serde_json::from_str(&json).unwrap();
            assert!(
                matches!(block, HelpBlock::Admonition { admonition, .. } if admonition == kind),
                "{name} decoded to the wrong type"
            );
        }
    }

    #[test]
    fn a_table_says_whether_its_first_row_is_a_header() {
        let with = r#"{"kind":"table","rows":[["a","b"],["1","2"]],"header":true}"#;
        let block: HelpBlock = serde_json::from_str(with).unwrap();
        assert!(matches!(block, HelpBlock::Table { header: true, .. }));
        let without = r#"{"kind":"table","rows":[["a","b"]]}"#;
        let block: HelpBlock = serde_json::from_str(without).unwrap();
        assert!(matches!(block, HelpBlock::Table { header: false, .. }));
    }

    #[test]
    fn an_admonition_without_spans_still_decodes() {
        let json = r#"{"kind":"admonition","admonition":"caution","text":"body"}"#;
        let block: HelpBlock = serde_json::from_str(json).unwrap();
        assert!(matches!(
            block,
            HelpBlock::Admonition {
                admonition: HelpAdmonition::Caution,
                spans,
                ..
            } if spans.is_empty()
        ));
    }

    #[test]
    fn admonition_types_survive_the_bundle() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let mut kinds = BTreeSet::new();
        for block in bundle
            .sections
            .values()
            .flat_map(|section| section.blocks.iter())
        {
            if let HelpBlock::Admonition {
                admonition,
                text,
                spans,
            } = block
            {
                kinds.insert(*admonition);
                assert!(!text.trim().is_empty());
                assert!(!spans.is_empty());
                let first = spans[0].text.trim().to_ascii_lowercase();
                assert!(
                    !matches!(first.as_str(), "warning" | "note" | "tip" | "important" | "caution"),
                    "the type label must not be part of the body"
                );
            }
        }
        assert!(kinds.contains(&HelpAdmonition::Warning));
        assert!(kinds.contains(&HelpAdmonition::Note));
        assert!(kinds.contains(&HelpAdmonition::Important));
    }
}
