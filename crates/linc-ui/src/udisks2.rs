//! Minimal udisks2 D-Bus client for removable-media auto-mount/eject.
//!
//! This intentionally uses the system bus object manager and never invokes a
//! shell or `udisksctl`. Mounts are requested with `nosuid,nodev,noexec`; eject
//! uses the normal Drive.Eject method (no force-unmount).

use std::collections::HashMap;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

fn connection() -> Result<Connection, String> {
    Connection::system().map_err(|error| error.to_string())
}

fn object_manager(conn: &Connection) -> Result<Proxy<'_>, String> {
    Proxy::new(
        conn,
        "org.freedesktop.UDisks2",
        "/org/freedesktop/UDisks2",
        "org.freedesktop.DBus.ObjectManager",
    )
    .map_err(|error| error.to_string())
}

type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

fn objects(conn: &Connection) -> Result<ManagedObjects, String> {
    object_manager(conn)?
        .call("GetManagedObjects", &())
        .map_err(|error| error.to_string())
}

fn as_path(bytes: &[u8]) -> String {
    let bytes = bytes.split(|byte| *byte == 0).next().unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Mount the first removable filesystem that is not already mounted.
pub fn mount_first_removable() -> Result<Option<String>, String> {
    let conn = connection()?;
    let objects = objects(&conn)?;
    for (path, interfaces) in objects {
        if !interfaces.contains_key("org.freedesktop.UDisks2.Block") {
            continue;
        }
        let block = Proxy::new(
            &conn,
            "org.freedesktop.UDisks2",
            path.as_str(),
            "org.freedesktop.UDisks2.Block",
        )
        .map_err(|error| error.to_string())?;
        let id_usage: String = block.get_property("IdUsage").unwrap_or_default();
        if id_usage != "filesystem" {
            continue;
        }
        let drive_path: OwnedObjectPath = match block.get_property("Drive") {
            Ok(path) => path,
            Err(_) => continue,
        };
        if drive_path.as_str() == "/" {
            continue;
        }
        let drive = Proxy::new(
            &conn,
            "org.freedesktop.UDisks2",
            drive_path.as_str(),
            "org.freedesktop.UDisks2.Drive",
        )
        .map_err(|error| error.to_string())?;
        let removable: bool = drive.get_property("Removable").unwrap_or(false);
        if !removable {
            continue;
        }
        let filesystem = Proxy::new(
            &conn,
            "org.freedesktop.UDisks2",
            path.as_str(),
            "org.freedesktop.UDisks2.Filesystem",
        )
        .map_err(|error| error.to_string())?;
        let existing: Vec<Vec<u8>> = filesystem.get_property("MountPoints").unwrap_or_default();
        if let Some(point) = existing.first() {
            return Ok(Some(as_path(point)));
        }
        let mut options: HashMap<String, OwnedValue> = HashMap::new();
        let value = Value::from("nosuid,nodev,noexec")
            .try_into()
            .map_err(|error: zbus::zvariant::Error| error.to_string())?;
        options.insert("options".into(), value);
        let mounted: Vec<Vec<u8>> = filesystem
            .call("Mount", &options)
            .map_err(|error| error.to_string())?;
        if let Some(point) = mounted.first() {
            return Ok(Some(as_path(point)));
        }
    }
    Ok(None)
}

/// Eject the drive containing a mounted path using udisks2.
pub fn eject_mount(mount_path: &str) -> Result<(), String> {
    let conn = connection()?;
    let objects = objects(&conn)?;
    let needle = mount_path.trim_end_matches('/');
    for (path, interfaces) in objects {
        if !interfaces.contains_key("org.freedesktop.UDisks2.Filesystem") {
            continue;
        }
        let filesystem = Proxy::new(
            &conn,
            "org.freedesktop.UDisks2",
            path.as_str(),
            "org.freedesktop.UDisks2.Filesystem",
        )
        .map_err(|error| error.to_string())?;
        let points: Vec<Vec<u8>> = filesystem.get_property("MountPoints").unwrap_or_default();
        if !points.iter().any(|point| as_path(point) == needle) {
            continue;
        }
        let block = Proxy::new(
            &conn,
            "org.freedesktop.UDisks2",
            path.as_str(),
            "org.freedesktop.UDisks2.Block",
        )
        .map_err(|error| error.to_string())?;
        let drive_path: OwnedObjectPath = block
            .get_property("Drive")
            .map_err(|error| error.to_string())?;
        let drive = Proxy::new(
            &conn,
            "org.freedesktop.UDisks2",
            drive_path.as_str(),
            "org.freedesktop.UDisks2.Drive",
        )
        .map_err(|error| error.to_string())?;
        let options: HashMap<String, OwnedValue> = HashMap::new();
        let _: () = drive
            .call("Eject", &options)
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    Err("no mounted removable filesystem found".into())
}
