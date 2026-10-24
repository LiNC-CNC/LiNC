//! Locate OpenCASCADE and build the STEP tessellation helper.
//!
//! OCCT is probed by file existence because this machine has no pkg-config
//! metadata for it: headers under /usr/include/opencascade and shared libraries
//! under /usr/lib. Both the include and the library directory can be overridden:
//!
//!     OPEN_CASCADE_INCLUDE / OCCT_INCLUDE   (authoritative when set)
//!     OPEN_CASCADE_LIB / OCCT_LIB           (authoritative when set)
//!     CXX                                   (compiler, default g++)
//!
//! A missing OCCT is not a build failure. The helper is simply not compiled and
//! linc_sim3d::step falls back to freecadcmd at run time, which is also how a
//! build host without CAD support stays usable.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_INCLUDE_DIRS: &[&str] = &[
    "/usr/include/opencascade",
    "/usr/local/include/opencascade",
    "/opt/opencascade/include/opencascade",
];

const DEFAULT_LIB_DIRS: &[&str] = &[
    "/usr/lib",
    "/usr/lib/x86_64-linux-gnu",
    "/usr/local/lib",
    "/opt/opencascade/lib",
];

/// OCCT 7.8 merged TKSTEP, TKSTEPBase, TKSTEPAttr, TKSTEP209 and TKSTEPDim into
/// TKDESTEP; older releases still ship the separate libraries.
const STEP_LIB_SETS: &[&[&str]] = &[&["TKDESTEP"], &["TKSTEP", "TKSTEPBase", "TKSTEPAttr"]];

const CORE_LIBS: &[&str] = &[
    "TKernel",
    "TKXSBase",
    "TKBRep",
    "TKTopAlgo",
    "TKMesh",
    "TKGeomBase",
    "TKG3d",
    "TKMath",
];

fn main() {
    println!("cargo:rerun-if-changed=cpp/step_tess.cpp");
    for name in [
        "OPEN_CASCADE_INCLUDE",
        "OCCT_INCLUDE",
        "OPEN_CASCADE_LIB",
        "OCCT_LIB",
        "CXX",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    match build_helper() {
        Ok(helper) => println!("cargo:rustc-env=LINC_SIM3D_STEP_HELPER={}", helper.display()),
        Err(reason) => println!(
            "cargo:warning=linc-sim3d: OCCT STEP helper not built ({reason}); STEP import falls back to freecadcmd"
        ),
    }
}

fn build_helper() -> Result<PathBuf, String> {
    let include_dir = find_include_dir()?;
    let (lib_dir, link_args) = find_libs()?;

    let manifest = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").map_err(|error| format!("CARGO_MANIFEST_DIR: {error}"))?,
    );
    let source = manifest.join("cpp").join("step_tess.cpp");
    if !source.is_file() {
        return Err(format!("{} is missing", source.display()));
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR").map_err(|error| format!("OUT_DIR: {error}"))?);
    let helper = out_dir.join("step_tess");
    let compiler = env::var("CXX").unwrap_or_else(|_| "g++".to_string());

    let mut command = Command::new(&compiler);
    command.arg("-std=c++17").arg("-O2");
    command.arg(format!("-I{}", include_dir.display()));
    command.arg(&source);
    command.arg("-o").arg(&helper);
    command.arg(format!("-L{}", lib_dir.display()));
    for arg in &link_args {
        command.arg(arg);
    }
    let output = command
        .output()
        .map_err(|error| format!("cannot run {compiler}: {error}"))?;
    if !output.status.success() {
        return Err(format!("{compiler} failed: {}", tail(&output.stderr)));
    }
    if !helper.is_file() {
        return Err("the compiler produced no helper binary".to_string());
    }
    Ok(helper)
}

fn find_include_dir() -> Result<PathBuf, String> {
    if let Some(dir) = env_dir(&["OPEN_CASCADE_INCLUDE", "OCCT_INCLUDE"]) {
        // An explicit override is authoritative, even when it is wrong: that is
        // how a build is checked without OCCT.
        return if dir.join("STEPControl_Reader.hxx").is_file() {
            Ok(dir)
        } else {
            Err(format!(
                "STEPControl_Reader.hxx is not in {}",
                dir.display()
            ))
        };
    }
    for candidate in DEFAULT_INCLUDE_DIRS {
        let dir = PathBuf::from(candidate);
        if dir.join("STEPControl_Reader.hxx").is_file() {
            return Ok(dir);
        }
    }
    Err("no OCCT headers found (set OPEN_CASCADE_INCLUDE)".to_string())
}

fn find_libs() -> Result<(PathBuf, Vec<String>), String> {
    if let Some(dir) = env_dir(&["OPEN_CASCADE_LIB", "OCCT_LIB"]) {
        return match resolve_libs(&dir) {
            Some(link_args) => Ok((dir, link_args)),
            None => Err(format!("no OCCT libraries in {}", dir.display())),
        };
    }
    for candidate in DEFAULT_LIB_DIRS {
        let dir = PathBuf::from(candidate);
        if let Some(link_args) = resolve_libs(&dir) {
            return Ok((dir, link_args));
        }
    }
    Err("no OCCT libraries found (set OPEN_CASCADE_LIB)".to_string())
}

/// Link arguments for one core set plus the first available STEP library set.
fn resolve_libs(dir: &Path) -> Option<Vec<String>> {
    let mut args = Vec::with_capacity(CORE_LIBS.len() + 3);
    for name in CORE_LIBS {
        args.push(link_arg(dir, name)?);
    }
    for set in STEP_LIB_SETS {
        let mut step_args = Vec::with_capacity(set.len());
        let mut complete = true;
        for name in *set {
            match link_arg(dir, name) {
                Some(arg) => step_args.push(arg),
                None => {
                    complete = false;
                    break;
                }
            }
        }
        if complete {
            args.extend(step_args);
            return Some(args);
        }
    }
    None
}

/// -lNAME when the development symlink exists, else the versioned file itself.
fn link_arg(dir: &Path, name: &str) -> Option<String> {
    if dir.join(format!("lib{name}.so")).is_file() {
        return Some(format!("-l{name}"));
    }
    let prefix = format!("lib{name}.so.");
    let mut versioned: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let file_name = entry.file_name();
        if let Some(file_name) = file_name.to_str() {
            if file_name.starts_with(&prefix) {
                versioned.push(entry.path());
            }
        }
    }
    versioned.sort();
    versioned.pop().map(|path| path.display().to_string())
}

fn env_dir(names: &[&str]) -> Option<PathBuf> {
    for name in names {
        if let Some(value) = env::var_os(name) {
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "no output".to_string();
    }
    let tail: String = trimmed.chars().rev().take(1600).collect();
    tail.chars().rev().collect()
}
