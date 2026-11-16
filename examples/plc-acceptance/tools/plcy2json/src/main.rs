//! `plcy2json` — a PLCopen XML file to a LiNC project JSON file.
//!
//! Reads a path at run time and writes the same bytes the PLC page's Save writes:
//! the importer's project, with the POU list folded back into the version-1 fields
//! and the ladder laid out as cells, then pretty JSON. Nothing here embeds a
//! corpus file; the corpus is an argument.
//!
//! Usage:
//!     plcy2json <in.xml> <out.json>
//!     plcy2json --dir <dir-of-xml> <out-dir>

use std::path::{Path, PathBuf};
use std::process::ExitCode;

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

fn convert(source: &Path, target: &Path) -> Result<(), String> {
    let xml = std::fs::read_to_string(source)
        .map_err(|error| format!("{}: {error}", source.display()))?;
    let json = canonical(&xml)?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(target, json).map_err(|error| format!("{}: {error}", target.display()))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, from, to] if flag == "--dir" => {
            let from = PathBuf::from(from);
            let to = PathBuf::from(to);
            let mut entries = match std::fs::read_dir(&from) {
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
                match convert(&source, &target) {
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
        [from, to] => match convert(Path::new(from), Path::new(to)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("plcy2json: {error}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: plcy2json <in.xml> <out.json> | plcy2json --dir <in-dir> <out-dir>");
            ExitCode::FAILURE
        }
    }
}
