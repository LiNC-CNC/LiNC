//! `plcy2json` — a PLCopen XML file to a LiNC project JSON file.
//!
//! Reads a path at run time and writes the same bytes the PLC page's Save writes:
//! the importer's project, with the POU list folded back into the version-1 fields
//! and the ladder laid out as cells, then pretty JSON. Nothing here embeds a
//! corpus file; the corpus is an argument.
//!
//! With a `.plcy` source beside the XML, the I/O points it aliases take the alias
//! as their operand name and keep the address as their binding: the ladder reads
//! `Enable`, the model still knows it is `%I0.0`. Without an alias the importer's
//! own name is left alone.
//!
//! Usage:
//!     plcy2json <in.xml> <out.json> [<source.plcy>]
//!     plcy2json --dir <dir-of-xml> <out-dir> [--sources <dir-of-plcy>]

use std::path::Path;
use std::process::ExitCode;

/// The alias a source gives an address, as a variable name.
///
/// A PLCYard symbol is often a description — `Open (PB)`, `Reset count` — so the
/// qualifier goes and the spaces become underscores. What is left must be the
/// model's I/O identifier (lowercase, `[a-z][a-z0-9_-]*`, at most 32), because the
/// name is also the HAL pin name; anything else keeps the address spelling and the
/// alias is dropped rather than invented into something the source never said.
fn alias_name(text: &str) -> Option<String> {
    let base = match text.find('(') {
        Some(at) => &text[..at],
        None => text,
    };
    let mut name = String::new();
    for character in base.trim().chars() {
        if character.is_ascii_alphanumeric() {
            name.push(character.to_ascii_lowercase());
        } else if character.is_whitespace() || character == '-' {
            name.push('_');
        }
    }
    let name = name.trim_matches('_').to_string();
    let first = name.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '_') || name.len() > 32 {
        return None;
    }
    Some(name)
}

/// Rename the aliased I/O points: the alias becomes the operand, the address stays
/// the binding. The rename is a whole-token replacement in the project's own JSON,
/// so every place the point appears — a contact, a coil, a watch row, a block pin —
/// changes together and nothing else does.
fn apply_aliases(json: &str, source: &Path) -> Result<(String, usize), String> {
    let text = std::fs::read_to_string(source)
        .map_err(|error| format!("{}: {error}", source.display()))?;
    let source: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", source.display()))?;
    let Some(symbols) = source.get("symbols").and_then(|value| value.as_object()) else {
        return Ok((json.to_string(), 0));
    };
    let project: linc_plc_core::Project =
        serde_json::from_str(json).map_err(|error| format!("project: {error}"))?;
    let mut renamed = json.to_string();
    let mut taken: Vec<String> = project.io.iter().map(|point| point.name.clone()).collect();
    let mut count = 0;
    for (address, value) in symbols {
        let Some(alias_text) = value.as_str() else {
            continue;
        };
        let Some(alias) = alias_name(alias_text) else {
            continue;
        };
        let Some(point) = project
            .io
            .iter()
            .find(|point| point.address.eq_ignore_ascii_case(address))
        else {
            continue;
        };
        if point.name == alias || taken.iter().any(|name| name == &alias) {
            continue;
        }
        let from = format!("\"{}\"", point.name);
        let to = format!("\"{alias}\"");
        if !renamed.contains(&from) {
            continue;
        }
        renamed = renamed.replace(&from, &to);
        taken.retain(|name| name != &point.name);
        taken.push(alias);
        count += 1;
    }
    Ok((renamed, count))
}

fn canonical(xml: &str) -> Result<String, String> {
    let mut project = linc_plc_core::plcopen::import(xml).map_err(|diagnostics| {
        diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    // The same canonicalisation the page's Save does: the POU list is the one
    // representation, and a ladder written as trees is laid out as cells.
    let pous = project.all_pous();
    project.set_pous(pous);
    project.normalise();
    serde_json::to_string_pretty(&project).map_err(|error| error.to_string())
}

fn convert(source: &Path, target: &Path, plcy: Option<&Path>) -> Result<(), String> {
    let xml = std::fs::read_to_string(source)
        .map_err(|error| format!("{}: {error}", source.display()))?;
    let json = canonical(&xml)?;
    let (json, renamed) = match plcy {
        Some(plcy) => apply_aliases(&json, plcy)?,
        None => (json, 0),
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    if renamed > 0 {
        println!("{}: {renamed} operand(s) named by their alias", target.display());
    }
    std::fs::write(target, json).map_err(|error| format!("{}: {error}", target.display()))
}

fn run_dir(from: &Path, to: &Path, sources: Option<&Path>) -> ExitCode {
    let mut entries = match std::fs::read_dir(from) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>(),
        Err(error) => {
            eprintln!("plcy2json: {}: {error}", from.display());
            return ExitCode::FAILURE;
        }
    };
    entries.sort();
    let mut failed = false;
    for source in entries {
        if source.extension().and_then(|ext| ext.to_str()) != Some("xml") {
            continue;
        }
        let name = source.file_name().expect("a file has a name");
        let target = to.join(name).with_extension("json");
        // The alias map lives beside the XML, under the same stem.
        let plcy = sources.map(|dir| dir.join(name).with_extension("plcy"));
        let plcy = plcy.filter(|path| path.is_file());
        match convert(&source, &target, plcy.as_deref()) {
            Ok(()) => println!("{} -> {}", source.display(), target.display()),
            Err(error) => {
                eprintln!("plcy2json: {error}");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match args.as_slice() {
        [flag, from, to, sources_flag, sources] if flag == "--dir" && sources_flag == "--sources" => {
            return run_dir(Path::new(from), Path::new(to), Some(Path::new(sources)))
        }
        [flag, from, to] if flag == "--dir" => return run_dir(Path::new(from), Path::new(to), None),
        [from, to, plcy] => convert(Path::new(from), Path::new(to), Some(Path::new(plcy))),
        [from, to] => convert(Path::new(from), Path::new(to), None),
        _ => {
            eprintln!(
                "usage: plcy2json <in.xml> <out.json> [<source.plcy>] \
                 | plcy2json --dir <in-dir> <out-dir> [--sources <dir-of-plcy>]"
            );
            Err(String::new())
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if !error.is_empty() {
                eprintln!("plcy2json: {error}");
            }
            ExitCode::FAILURE
        }
    }
}
