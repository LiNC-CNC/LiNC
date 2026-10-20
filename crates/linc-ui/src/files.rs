//! Filesystem side of the Files page.
//!
//! Removable media is discovered from existing udisks2 mount points. Copying
//! uses canonical path checks, rejects symlinks, checks free space, writes a
//! temporary file in the destination and renames it atomically. No shell is
//! invoked and no file is ever executed.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::SystemTime;

use crate::state::CopyProgress;

#[derive(Clone, Debug)]
pub struct UsbDevice {
    pub path: PathBuf,
    pub label: String,
    pub free: Option<u64>,
    pub mount_options: Vec<String>,
}

pub fn discover_usb() -> Result<Option<UsbDevice>, String> {
    if let Some(path) = std::env::var_os("LINC_USB_DIR") {
        let path = PathBuf::from(path);
        if path.is_dir() {
            return Ok(Some(UsbDevice {
                label: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "USB".into()),
                free: free_space(&path),
                mount_options: vec!["nosuid".into(), "nodev".into(), "noexec".into()],
                path,
            }));
        }
        return Ok(None);
    }

    let user = std::env::var("USER").unwrap_or_default();
    let roots = [
        PathBuf::from("/run/media").join(&user),
        PathBuf::from("/media").join(&user),
        PathBuf::from("/media"),
    ];
    let mounts = read_mounts();
    for root in roots {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // Only accept actual mount points, never arbitrary directories.
            if !is_mount_point(&path) {
                continue;
            }
            let mount = mounts
                .iter()
                .find(|mount| mount.path == path)
                .cloned()
                .unwrap_or_default();
            for required in ["nosuid", "nodev", "noexec"] {
                if !mount.options.iter().any(|option| option == required) {
                    return Err(format!(
                        "{} is mounted without {required}; refusing removable media",
                        path.display()
                    ));
                }
            }
            let label = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "USB".into());
            return Ok(Some(UsbDevice {
                path: path.clone(),
                label,
                free: free_space(&path),
                mount_options: mount.options,
            }));
        }
    }
    Ok(None)
}

#[derive(Clone, Debug, Default)]
struct MountInfo {
    path: PathBuf,
    options: Vec<String>,
}

fn read_mounts() -> Vec<MountInfo> {
    let Ok(text) = fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _device = fields.next()?;
            let mountpoint = fields.next()?;
            let _fstype = fields.next()?;
            let options = fields.next()?;
            let mountpoint = mountpoint.replace("\\040", " ");
            Some(MountInfo {
                path: PathBuf::from(mountpoint),
                options: options.split(',').map(str::to_string).collect(),
            })
        })
        .collect()
}

fn is_mount_point(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(parent_metadata) = fs::metadata(parent) else {
        return false;
    };
    metadata.dev() != parent_metadata.dev()
}

pub fn free_space(path: &Path) -> Option<u64> {
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if result != 0 {
        return None;
    }
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[derive(Debug)]
pub enum CopyError {
    UnsafePath(String),
    Symlink(String),
    NoSpace { needed: u64, free: u64 },
    Io(String),
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafePath(path) => write!(f, "unsafe or missing path: {path}"),
            Self::Symlink(path) => write!(f, "symlink rejected: {path}"),
            Self::NoSpace { needed, free } => {
                write!(f, "not enough space: need {needed} B, {free} B free")
            }
            Self::Io(error) => write!(f, "copy failed: {error}"),
        }
    }
}

pub fn validate_source(path: &Path) -> Result<fs::Metadata, CopyError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| CopyError::UnsafePath(path.display().to_string()))?;
    if metadata.file_type().is_symlink() {
        return Err(CopyError::Symlink(path.display().to_string()));
    }
    if !metadata.is_file() {
        return Err(CopyError::UnsafePath(path.display().to_string()));
    }
    Ok(metadata)
}

pub fn validate_destination(root: &Path, path: &Path) -> Result<PathBuf, CopyError> {
    let root = root
        .canonicalize()
        .map_err(|_| CopyError::UnsafePath(root.display().to_string()))?;
    let parent = path
        .parent()
        .ok_or_else(|| CopyError::UnsafePath(path.display().to_string()))?
        .canonicalize()
        .map_err(|_| CopyError::UnsafePath(path.display().to_string()))?;
    if !parent.starts_with(&root) {
        return Err(CopyError::UnsafePath(path.display().to_string()));
    }
    let name = path
        .file_name()
        .ok_or_else(|| CopyError::UnsafePath(path.display().to_string()))?;
    let destination = parent.join(name);
    if destination
        .symlink_metadata()
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(CopyError::Symlink(destination.display().to_string()));
    }
    Ok(destination)
}

/// Copy with progress callbacks, using a temp file in the destination
/// directory and an atomic rename.
pub fn copy_file_with_progress(
    source: &Path,
    destination_root: &Path,
    destination: &Path,
    overwrite: bool,
    progress: &dyn Fn(u64, u64),
) -> Result<(), CopyError> {
    let source_meta = validate_source(source)?;
    let destination = validate_destination(destination_root, destination)?;
    if destination.exists() && !overwrite {
        return Err(CopyError::Io(format!(
            "{} already exists",
            destination.display()
        )));
    }
    let total = source_meta.len();
    if let (Some(free), true) = (free_space(destination_root), total > 0) {
        if free < total {
            return Err(CopyError::NoSpace {
                needed: total,
                free,
            });
        }
    }
    let parent = destination
        .parent()
        .ok_or_else(|| CopyError::UnsafePath(destination.display().to_string()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| CopyError::Io(error.to_string()))?;
    let mut input = fs::File::open(source).map_err(|error| CopyError::Io(error.to_string()))?;
    let mut buffer = vec![0u8; 256 * 1024];
    let mut copied = 0u64;
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|error| CopyError::Io(error.to_string()))?;
        if read == 0 {
            break;
        }
        temporary
            .write_all(&buffer[..read])
            .map_err(|error| CopyError::Io(error.to_string()))?;
        copied += read as u64;
        progress(copied, total);
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| CopyError::Io(error.to_string()))?;
    temporary
        .persist(&destination)
        .map_err(|error| CopyError::Io(error.error.to_string()))?;
    Ok(())
}

pub fn start_copy(
    source: PathBuf,
    destination_root: PathBuf,
    destination: PathBuf,
    overwrite: bool,
    tx: Sender<CopyProgress>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("linc-copy".into())
        .spawn(move || {
            let tx_progress = tx.clone();
            let result = copy_file_with_progress(
                &source,
                &destination_root,
                &destination,
                overwrite,
                &move |copied, total| {
                    let _ = tx_progress.send(CopyProgress::Progress { copied, total });
                },
            );
            match result {
                Ok(()) => {
                    let _ = tx.send(CopyProgress::Done);
                }
                Err(error) => {
                    let _ = tx.send(CopyProgress::Error(error.to_string()));
                }
            }
        })
        .expect("spawn copy thread")
}

pub fn format_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_modified(time: Option<SystemTime>) -> String {
    let Some(time) = time else {
        return "—".into();
    };
    let Ok(duration) = time.duration_since(SystemTime::UNIX_EPOCH) else {
        return "—".into();
    };
    let seconds = duration.as_secs();
    // Local date is not available without a time crate; show MM-DD HH:MM UTC.
    let days = seconds / 86_400;
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rem / 3600,
        (rem / 60) % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + if m <= 2 { 1 } else { 0 }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_rejects_symlinks_and_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let target = dir.path().join("target.ngc");
        fs::write(&target, "G0 X0\n").unwrap();
        let link = root.join("link.ngc");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(validate_source(&link), Err(CopyError::Symlink(_))));
        let outside = dir.path().join("outside.ngc");
        assert!(matches!(
            validate_destination(&root, &outside),
            Err(CopyError::UnsafePath(_))
        ));
    }

    #[test]
    fn atomic_copy_preserves_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let source = dir.path().join("part.ngc");
        fs::write(&source, "G1 X1\n").unwrap();
        let destination = root.join("part.ngc");
        copy_file_with_progress(&source, &root, &destination, false, &|_, _| {}).unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "G1 X1\n");
        assert!(copy_file_with_progress(&source, &root, &destination, false, &|_, _| {}).is_err());
        copy_file_with_progress(&source, &root, &destination, true, &|_, _| {}).unwrap();
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(2048), "2.0 KB");
    }
}
