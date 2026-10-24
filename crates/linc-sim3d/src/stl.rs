//! Minimal STL reader (binary and ASCII).
//!
//! STL carries no units, so the result is taken verbatim as millimetres, which
//! is what both LiNC backends emit. Normals are ignored: nothing downstream
//! needs them, and the collision code works on triangles alone.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::types::TriMesh;
use crate::types::P3;

/// Why an STL file could not be read.
#[derive(Debug, Error)]
pub enum StlError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("malformed STL: {0}")]
    Malformed(String),
}

/// Read a binary or ASCII STL file into a mesh of millimetres.
///
/// The mesh is named after the file stem.
pub fn read_stl(path: &Path) -> Result<TriMesh, StlError> {
    let bytes = fs::read(path).map_err(|source| StlError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("stl");
    parse_stl(&bytes, name)
}

/// Parse STL bytes; name becomes the mesh name.
pub fn parse_stl(bytes: &[u8], name: impl Into<String>) -> Result<TriMesh, StlError> {
    let positions = if is_binary(bytes) {
        parse_binary(bytes)?
    } else {
        parse_ascii(bytes)?
    };
    if positions.is_empty() {
        return Err(StlError::Malformed("no triangles found".to_string()));
    }
    let indices = (0..positions.len() as u32 / 3)
        .map(|i| [i * 3, i * 3 + 1, i * 3 + 2])
        .collect();
    Ok(TriMesh::new(name, positions, indices))
}

/// True when the byte stream is a binary STL.
///
/// The exact size rule (84 + 50 bytes per facet) decides first; ASCII files
/// may also start with "solid", so the header word alone proves nothing. A file
/// that does not match the size rule and does not start with "solid" is
/// treated as binary anyway, so a truncated binary file reports the triangle
/// count it claims instead of being misread as ASCII.
pub fn is_binary(bytes: &[u8]) -> bool {
    if bytes.len() < 84 {
        return false;
    }
    let count = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    if bytes.len() == 84 + count * 50 {
        return true;
    }
    !bytes.starts_with(b"solid")
}

fn parse_binary(bytes: &[u8]) -> Result<Vec<P3>, StlError> {
    let count = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    let expected = 84 + count * 50;
    if bytes.len() < expected {
        return Err(StlError::Malformed(format!(
            "binary STL claims {count} triangles ({expected} bytes) but is {} bytes",
            bytes.len()
        )));
    }
    let mut positions = Vec::with_capacity(count * 3);
    for t in 0..count {
        let base = 84 + t * 50 + 12; // skip the facet normal
        for v in 0..3 {
            let offset = base + v * 12;
            let mut xyz = [0.0f64; 3];
            for (axis, value) in xyz.iter_mut().enumerate() {
                let at = offset + axis * 4;
                let raw =
                    f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
                *value = raw as f64;
            }
            positions.push(xyz);
        }
    }
    Ok(positions)
}

fn parse_ascii(bytes: &[u8]) -> Result<Vec<P3>, StlError> {
    let text = String::from_utf8_lossy(bytes);
    let mut positions = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("vertex") {
            continue;
        }
        let mut xyz = [0.0f64; 3];
        for value in xyz.iter_mut() {
            let token = parts.next().ok_or_else(|| {
                StlError::Malformed(format!(
                    "line {}: vertex needs three coordinates",
                    line_number + 1
                ))
            })?;
            *value = token.parse::<f64>().map_err(|error| {
                StlError::Malformed(format!(
                    "line {}: bad coordinate '{token}': {error}",
                    line_number + 1
                ))
            })?;
        }
        positions.push(xyz);
    }
    if positions.len() % 3 != 0 {
        return Err(StlError::Malformed(format!(
            "{} vertex lines is not a whole number of triangles",
            positions.len()
        )));
    }
    Ok(positions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_single_triangle() {
        let text = b"solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n";
        let mesh = parse_stl(text, "t").expect("ascii stl");
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(
            mesh.triangle(0),
            Some([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]])
        );
    }

    #[test]
    fn truncated_binary_is_reported() {
        let mut bytes = vec![0u8; 84];
        bytes[80..84].copy_from_slice(&2u32.to_le_bytes());
        let error = parse_stl(&bytes, "t").expect_err("truncated");
        assert!(error.to_string().contains("claims 2 triangles"), "{error}");
    }

    #[test]
    fn empty_ascii_is_reported() {
        let error = parse_stl(b"solid empty\nendsolid\n", "t").expect_err("empty");
        assert!(error.to_string().contains("no triangles"), "{error}");
    }

    #[test]
    fn binary_size_rule() {
        let mut bytes = vec![0u8; 84 + 50];
        bytes[80..84].copy_from_slice(&1u32.to_le_bytes());
        assert!(is_binary(&bytes));
        assert!(!is_binary(b"solid x\n"));
    }
}
