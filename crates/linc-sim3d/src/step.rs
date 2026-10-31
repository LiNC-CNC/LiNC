//! STEP import: a tessellated mesh in millimetres, with a disk cache.
//!
//! Two backends are tried in order:
//!
//! 1. [StepBackend::Helper] - the OCCT program built by build.rs, which writes
//!    the LINC3D-MESH text format directly.
//! 2. [StepBackend::FreeCad] - freecadcmd running a short script that meshes the
//!    shape with MeshPart and writes an STL, which stl.rs reads back.
//!
//! Every conversion is cached under $XDG_CACHE_HOME/linc/sim3d (or
//! ~/.cache/linc/sim3d) in the same LINC3D-MESH text format, keyed by the file
//! path, its size, its modification time, the tessellation settings and the
//! format version. A cache hit starts no subprocess.
//!
//! The format starts with the header line "LINC3D-MESH 1" and the unit line
//! "MM", then holds one record per line: "V x y z" for a vertex and
//! "F a b c" for a triangle. Positions are millimetres and triangle indices are
//! 0-based. Vertices may be shared or repeated; the reader does not care.

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::stl;
use crate::types::{TriMesh, P3};

/// Version of the LINC3D-MESH file format, written by both backends and by the
/// cache. Bump it whenever the layout or the tessellation changes, so stale
/// cache entries are ignored.
pub const LINC_MESH_FORMAT: u32 = 1;

/// Linear tessellation deflection used by the freecadcmd fallback, in mm.
pub const LINEAR_DEFLECTION_MM: f64 = 0.05;
/// Angular tessellation deflection used by the freecadcmd fallback, in radians.
pub const ANGULAR_DEFLECTION_RAD: f64 = 0.3;

const HELPER_ENV: &str = "LINC_SIM3D_STEP_HELPER";
const CACHE_ENV: &str = "LINC_SIM3D_CACHE_DIR";
const FREECAD_EXE: &str = "freecadcmd";

/// Where a STEP file may be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepBackend {
    /// The OCCT helper built by build.rs; the path is the executable.
    Helper(PathBuf),
    /// freecadcmd in console mode; the path is the executable.
    FreeCad(PathBuf),
}

impl StepBackend {
    /// Short name for logs and errors.
    pub fn label(&self) -> &'static str {
        match self {
            StepBackend::Helper(_) => "occt-helper",
            StepBackend::FreeCad(_) => "freecadcmd",
        }
    }

    /// The executable this backend runs.
    pub fn path(&self) -> &Path {
        match self {
            StepBackend::Helper(path) | StepBackend::FreeCad(path) => path,
        }
    }
}

impl fmt::Display for StepBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.label(), self.path().display())
    }
}

/// Why a STEP import failed.
#[derive(Debug, Error)]
pub enum StepError {
    #[error(
        "no STEP backend is available: build linc-sim3d where OpenCASCADE is installed, or add freecadcmd to PATH"
    )]
    NoBackend,
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{backends} could not convert {path}: {detail}")]
    Conversion {
        path: PathBuf,
        backends: String,
        detail: String,
    },
}

/// A STEP importer: cache directory, backend paths and tessellation settings.
#[derive(Debug, Clone)]
pub struct StepImporter {
    cache_dir: PathBuf,
    helper: Option<PathBuf>,
    freecad: Option<PathBuf>,
    linear_deflection_mm: f64,
    angular_deflection_rad: f64,
}

impl StepImporter {
    /// Discover the environment: the helper from $LINC_SIM3D_STEP_HELPER, then
    /// the path build.rs baked in, then freecadcmd from PATH.
    pub fn discover() -> Self {
        let helper = env::var_os(HELPER_ENV)
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .or_else(|| option_env!("LINC_SIM3D_STEP_HELPER").map(PathBuf::from));
        let freecad = find_in_path(FREECAD_EXE);
        Self {
            cache_dir: default_cache_dir(),
            helper,
            freecad,
            linear_deflection_mm: LINEAR_DEFLECTION_MM,
            angular_deflection_rad: ANGULAR_DEFLECTION_RAD,
        }
    }

    /// The cache directory in use.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// The OCCT helper path, when one is configured.
    pub fn helper(&self) -> Option<&Path> {
        self.helper.as_deref()
    }

    /// The freecadcmd path, when one was found.
    pub fn freecad(&self) -> Option<&Path> {
        self.freecad.as_deref()
    }

    /// Linear tessellation deflection for the freecadcmd fallback, in mm.
    ///
    /// The OCCT helper always uses its own baked-in deflection (0.05 mm and
    /// 0.3 rad); this setting moves the cache key and the fallback only.
    pub fn linear_deflection_mm(&self) -> f64 {
        self.linear_deflection_mm
    }

    /// Angular tessellation deflection for the freecadcmd fallback, in radians.
    pub fn angular_deflection_rad(&self) -> f64 {
        self.angular_deflection_rad
    }

    /// Use another cache directory.
    pub fn with_cache_dir(mut self, dir: PathBuf) -> Self {
        self.cache_dir = dir;
        self
    }

    /// Override the OCCT helper; None disables it and forces the fallback.
    pub fn with_helper(mut self, helper: Option<PathBuf>) -> Self {
        self.helper = helper;
        self
    }

    /// Override the freecadcmd path; None disables the fallback.
    pub fn with_freecad(mut self, freecad: Option<PathBuf>) -> Self {
        self.freecad = freecad;
        self
    }

    /// Override the tessellation settings used by the freecadcmd fallback.
    pub fn with_deflections(mut self, linear_mm: f64, angular_rad: f64) -> Self {
        self.linear_deflection_mm = linear_mm;
        self.angular_deflection_rad = angular_rad;
        self
    }

    /// The backends that will be tried, in order.
    pub fn backends(&self) -> Vec<StepBackend> {
        let mut backends = Vec::new();
        if let Some(helper) = &self.helper {
            backends.push(StepBackend::Helper(helper.clone()));
        }
        if let Some(freecad) = &self.freecad {
            backends.push(StepBackend::FreeCad(freecad.clone()));
        }
        backends
    }

    /// Import a STEP file as a mesh in millimetres.
    pub fn import(&self, path: &Path) -> Result<TriMesh, StepError> {
        let metadata = fs::metadata(path).map_err(|source| StepError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let cache_file = self
            .cache_dir
            .join(format!("{:016x}.lincmesh", self.cache_key(path, &metadata)));
        let name = mesh_name(path);

        if let Some(mesh) = read_cache(&cache_file, &name) {
            return Ok(mesh);
        }

        let backends = self.backends();
        if backends.is_empty() {
            return Err(StepError::NoBackend);
        }
        let scratch = self.scratch_dir()?;
        let mut failures: Vec<String> = Vec::new();
        let mut mesh: Option<TriMesh> = None;
        for backend in &backends {
            let attempt = match backend {
                StepBackend::Helper(helper) => {
                    run_helper(helper, path, &scratch.path, self.linear_deflection_mm)
                }
                StepBackend::FreeCad(freecad) => run_freecad(
                    freecad,
                    path,
                    &scratch.path,
                    self.linear_deflection_mm,
                    self.angular_deflection_rad,
                ),
            };
            match attempt {
                Ok(mut converted) => {
                    converted.name = name.clone();
                    mesh = Some(converted);
                    break;
                }
                Err(detail) => {
                    log::debug!(
                        "sim3d: {} failed for {}: {detail}",
                        backend.label(),
                        path.display()
                    );
                    failures.push(format!("{}: {detail}", backend.label()));
                }
            }
        }

        let mesh = match mesh {
            Some(mesh) => mesh,
            None => {
                return Err(StepError::Conversion {
                    path: path.to_path_buf(),
                    backends: backends
                        .iter()
                        .map(|backend| backend.label())
                        .collect::<Vec<_>>()
                        .join(", "),
                    detail: failures.join(" | "),
                })
            }
        };

        write_cache(&cache_file, &mesh);
        Ok(mesh)
    }

    /// Hash of everything that makes a cached conversion valid.
    fn cache_key(&self, path: &Path, metadata: &fs::Metadata) -> u64 {
        let mut hasher = DefaultHasher::new();
        LINC_MESH_FORMAT.hash(&mut hasher);
        self.linear_deflection_mm.to_bits().hash(&mut hasher);
        self.angular_deflection_rad.to_bits().hash(&mut hasher);
        metadata.len().hash(&mut hasher);
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
        match modified {
            Some(age) => {
                age.as_secs().hash(&mut hasher);
                age.subsec_nanos().hash(&mut hasher);
            }
            None => "no-mtime".hash(&mut hasher),
        }
        let absolute = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        absolute.hash(&mut hasher);
        hasher.finish()
    }

    fn scratch_dir(&self) -> Result<ScratchDir, StepError> {
        if fs::create_dir_all(&self.cache_dir).is_ok() {
            if let Ok(scratch) = ScratchDir::create(&self.cache_dir) {
                return Ok(scratch);
            }
        }
        let fallback = env::temp_dir();
        ScratchDir::create(&fallback).map_err(|source| StepError::Io {
            path: fallback,
            source,
        })
    }
}

/// A directory that removes itself when dropped.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn create(parent: &Path) -> io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|age| age.subsec_nanos())
            .unwrap_or(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".linc-sim3d-tmp-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run_helper(
    helper: &Path,
    step_path: &Path,
    scratch: &Path,
    deflection_mm: f64,
) -> Result<TriMesh, String> {
    let _ = deflection_mm; // baked into the helper at build time
    let output_path = scratch.join("helper.lincmesh");
    let output = Command::new(helper)
        .arg(step_path)
        .arg(&output_path)
        .output()
        .map_err(|error| format!("cannot run {}: {error}", helper.display()))?;
    if !output.status.success() {
        return Err(format!(
            "exit {:?}; stderr: {}",
            output.status.code(),
            trim_tool_output(&output.stderr)
        ));
    }
    let text = fs::read_to_string(&output_path)
        .map_err(|error| format!("helper wrote no output: {error}"))?;
    parse_lincmesh(&text, "step")
}

fn run_freecad(
    freecad: &Path,
    step_path: &Path,
    scratch: &Path,
    deflection_mm: f64,
    deflection_rad: f64,
) -> Result<TriMesh, String> {
    let stl_path = scratch.join("fallback.stl");
    let script = freecad_script(step_path, &stl_path, deflection_mm, deflection_rad);
    let output = Command::new(freecad)
        .arg("-c")
        .arg(format!("exec({})", py_quote(&script)))
        .output()
        .map_err(|error| format!("cannot run {}: {error}", freecad.display()))?;
    let stderr = trim_tool_output(&output.stderr);
    if !output.status.success() {
        return Err(format!("exit {:?}; stderr: {stderr}", output.status.code()));
    }
    if !stl_path.is_file() {
        return Err(format!("wrote no STL; stderr: {stderr}"));
    }
    let mesh = stl::read_stl(&stl_path).map_err(|error| format!("unreadable STL: {error}"))?;
    if mesh.is_empty() {
        return Err(format!("empty STL; stderr: {stderr}"));
    }
    Ok(mesh)
}

/// The python freecadcmd runs: mesh the STEP shape and write an STL.
///
/// freecadcmd is a console build, so no GUI can open; it is only ever started
/// with -c.
fn freecad_script(
    step_path: &Path,
    stl_path: &Path,
    deflection_mm: f64,
    deflection_rad: f64,
) -> String {
    format!(
        "import Part, MeshPart\n\
         shape = Part.Shape()\n\
         shape.read({step})\n\
         mesh = MeshPart.meshFromShape(Shape=shape, LinearDeflection={linear}, AngularDeflection={angular}, Relative=False)\n\
         mesh.write({stl})\n",
        step = py_quote(&step_path.display().to_string()),
        stl = py_quote(&stl_path.display().to_string()),
        linear = py_float(deflection_mm),
        angular = py_float(deflection_rad),
    )
}

fn py_float(value: f64) -> String {
    if value.is_finite() {
        format!("{value:?}")
    } else {
        "0.05".to_string()
    }
}

/// Quote a string as a python double-quoted literal.
fn py_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Drop the harmless freecadcmd chatter and keep anything that looks like a
/// real error.
fn trim_tool_output(stderr: &[u8]) -> String {
    const NOISE: &[&str] = &[
        ".config/FreeCAD",
        "output transcoder",
        "NOTICE: Addon",
        "backup-before-zip-migration",
        "Loading Loobric addon",
    ];
    let text = strip_ansi(&String::from_utf8_lossy(stderr));
    let mut kept: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || NOISE.iter().any(|needle| line.contains(needle)) {
            continue;
        }
        kept.push(line);
    }
    let joined = kept.join("; ");
    if joined.is_empty() {
        return "no output".to_string();
    }
    let mut tail: String = joined.chars().rev().take(1200).collect();
    if joined.chars().count() > 1200 {
        tail.push_str(" ...");
    }
    tail.chars().rev().collect()
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn mesh_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("step")
        .to_string()
}

fn default_cache_dir() -> PathBuf {
    if let Some(dir) = env::var_os(CACHE_ENV) {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let base = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(env::temp_dir);
    base.join("linc").join("sim3d")
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    for dir in env::split_paths(&paths) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn read_cache(cache_file: &Path, name: &str) -> Option<TriMesh> {
    if !cache_file.is_file() {
        return None;
    }
    match fs::read_to_string(cache_file) {
        Ok(text) => match parse_lincmesh(&text, name) {
            Ok(mesh) => {
                log::debug!("sim3d: STEP cache hit {}", cache_file.display());
                Some(mesh)
            }
            Err(detail) => {
                log::warn!(
                    "sim3d: dropping malformed STEP cache {}: {detail}",
                    cache_file.display()
                );
                let _ = fs::remove_file(cache_file);
                None
            }
        },
        Err(error) => {
            log::warn!(
                "sim3d: cannot read STEP cache {}: {error}",
                cache_file.display()
            );
            let _ = fs::remove_file(cache_file);
            None
        }
    }
}

fn write_cache(cache_file: &Path, mesh: &TriMesh) {
    let Some(parent) = cache_file.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = parent.join(format!(
        ".{}-{}.tmp",
        cache_file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("mesh"),
        std::process::id()
    ));
    if fs::write(&temporary, encode_lincmesh(mesh)).is_err() {
        let _ = fs::remove_file(&temporary);
        return;
    }
    if fs::rename(&temporary, cache_file).is_err() {
        let _ = fs::remove_file(&temporary);
    }
}

/// Serialise a mesh in the LINC3D-MESH text format read by [parse_lincmesh].
pub fn encode_lincmesh(mesh: &TriMesh) -> String {
    let mut out = String::with_capacity(mesh.positions.len() * 40 + mesh.indices.len() * 24 + 32);
    out.push_str(&format!("LINC3D-MESH {LINC_MESH_FORMAT}\nMM\n"));
    for [x, y, z] in &mesh.positions {
        out.push_str(&format!("V {x:.9} {y:.9} {z:.9}\n"));
    }
    for [a, b, c] in &mesh.indices {
        out.push_str(&format!("F {a} {b} {c}\n"));
    }
    out
}

/// Read a LINC3D-MESH text file into a mesh.
///
/// The error is a message for the caller to wrap; it names the line that broke.
pub fn parse_lincmesh(text: &str, name: &str) -> Result<TriMesh, String> {
    let mut lines = text.lines();
    let header = lines
        .next()
        .ok_or_else(|| "mesh file is empty".to_string())?;
    let mut header_parts = header.split_whitespace();
    if header_parts.next() != Some("LINC3D-MESH") {
        return Err(format!(
            "line 1: expected a 'LINC3D-MESH' header, got '{header}'"
        ));
    }
    let version = header_parts
        .next()
        .ok_or_else(|| "line 1: the header has no format version".to_string())?
        .parse::<u32>()
        .map_err(|error| format!("line 1: bad format version: {error}"))?;
    if version != LINC_MESH_FORMAT {
        return Err(format!(
            "line 1: mesh format version {version} is not supported (expected {LINC_MESH_FORMAT})"
        ));
    }
    let units = lines
        .next()
        .ok_or_else(|| "line 2: missing unit line".to_string())?;
    if units.trim() != "MM" {
        return Err(format!(
            "line 2: unit '{}' is not supported (only millimetres)",
            units.trim()
        ));
    }

    let mut positions: Vec<P3> = Vec::new();
    let mut indices: Vec<[u32; 3]> = Vec::new();
    for (offset, line) in lines.enumerate() {
        let number = offset + 3;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("V") => {
                let mut xyz = [0.0f64; 3];
                for value in xyz.iter_mut() {
                    let token = parts
                        .next()
                        .ok_or_else(|| format!("line {number}: vertex needs three coordinates"))?;
                    *value = token.parse::<f64>().map_err(|error| {
                        format!("line {number}: bad coordinate '{token}': {error}")
                    })?;
                }
                positions.push(xyz);
            }
            Some("F") => {
                let mut triangle = [0u32; 3];
                for value in triangle.iter_mut() {
                    let token = parts
                        .next()
                        .ok_or_else(|| format!("line {number}: face needs three indices"))?;
                    *value = token
                        .parse::<u32>()
                        .map_err(|error| format!("line {number}: bad index '{token}': {error}"))?;
                }
                indices.push(triangle);
            }
            Some(other) => return Err(format!("line {number}: unknown record '{other}'")),
            None => {}
        }
    }
    if indices.is_empty() {
        return Err("the mesh has no triangles".to_string());
    }
    for (i, triangle) in indices.iter().enumerate() {
        for vertex in triangle {
            if *vertex as usize >= positions.len() {
                return Err(format!(
                    "triangle {i} refers to vertex {vertex}, but only {} vertices were defined",
                    positions.len()
                ));
            }
        }
    }
    Ok(TriMesh::new(name, positions, indices))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_the_text_format() {
        let mesh = TriMesh::box_mesh(
            "box",
            &crate::types::Bounds {
                min: [0.0, 0.0, 0.0],
                max: [20.0, 20.0, 10.0],
            },
        );
        let text = encode_lincmesh(&mesh);
        let parsed = parse_lincmesh(&text, "box").expect("parses");
        assert_eq!(parsed.triangle_count(), 12);
        assert_eq!(parsed.vertex_count(), mesh.vertex_count());
        assert_eq!(parsed.bounds, mesh.bounds);
    }

    #[test]
    fn rejects_a_foreign_format() {
        let error = parse_lincmesh("SOLID x\nendsolid\n", "x").expect_err("not ours");
        assert!(error.contains("LINC3D-MESH"), "{error}");
    }

    #[test]
    fn rejects_a_future_version() {
        let error = parse_lincmesh("LINC3D-MESH 99\nMM\nV 0 0 0\n", "x").expect_err("version");
        assert!(error.contains("version 99"), "{error}");
    }

    #[test]
    fn rejects_an_out_of_range_index() {
        let error =
            parse_lincmesh("LINC3D-MESH 1\nMM\nV 0 0 0\nF 0 1 2\n", "x").expect_err("index");
        assert!(error.contains("refers to vertex 1"), "{error}");
    }

    #[test]
    fn quotes_python_and_keeps_noise_out() {
        assert_eq!(py_quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
        let noisy = "An error occurred during creation of output transcoder. Msg is:\n\
                     unable to open file '/home/x/.config/FreeCAD/v1-1/user.cfg'\n\
                     Traceback (most recent call last): boom\n";
        assert_eq!(
            trim_tool_output(noisy.as_bytes()),
            "Traceback (most recent call last): boom"
        );
    }
}
