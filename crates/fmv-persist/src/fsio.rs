//! Crash-safe file primitives: atomic replace, bounded reads, private directories.
//!
//! Every durable write in the store goes through [`atomic_write`]: write a temp file in the
//! same directory, fsync it, rename it over the target, fsync the directory. A crash at any
//! point leaves either the old file or the new file, never a torn one. Temp files left by a
//! crash are named so they can be recognized and swept on the next open.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{PersistError, Result};
use crate::model::SCHEMA_VERSION;

/// Temp files are `.<target name>.tmp-<pid>-<seq>`.
pub(crate) const TMP_MARKER: &str = ".tmp-";
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
const TMP_CREATE_ATTEMPTS: usize = 16;

pub(crate) fn is_temp_name(name: &str) -> bool {
    name.starts_with('.') && name.contains(TMP_MARKER)
}

/// Atomically replace (or create) `path` with `bytes`, durably.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_replace(path, |file| file.write_all(bytes))
}

/// Like [`atomic_write`], but the content is produced by `fill` straight into the (buffered)
/// temp file, for files too large to hold in memory. If `fill` fails nothing is replaced.
pub(crate) fn atomic_write_with(
    path: &Path,
    fill: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<()> {
    atomic_replace(path, |file| {
        let mut buffered = io::BufWriter::with_capacity(1 << 20, file);
        fill(&mut buffered)?;
        buffered.flush()
    })
}

/// The shared core: write a temp file next to `path` with `write`, fsync it, rename it over
/// `path` and fsync the directory.
fn atomic_replace(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> Result<()> {
    atomic_replace_with_checkpoints(path, write, |_| Ok(()))
}

/// Exercise interruptions in the same replacement path used by production writes.
#[cfg(test)]
pub(crate) fn atomic_write_with_checkpoints(path: &Path, bytes: &[u8], checkpoint: impl FnMut(u8) -> io::Result<()>) -> Result<()> {
    atomic_replace_with_checkpoints(path, |file| file.write_all(bytes), checkpoint)
}

fn atomic_replace_with_checkpoints(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
    mut checkpoint: impl FnMut(u8) -> io::Result<()>,
) -> Result<()> {
    let (dir, name) = match (path.parent(), path.file_name().and_then(|n| n.to_str())) {
        (Some(dir), Some(name)) => (dir, name),
        _ => {
            return Err(PersistError::io(
                "write",
                path,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path has no parent or file name",
                ),
            ))
        }
    };

    checkpoint(0).map_err(|e| PersistError::io("before atomic write", path, e))?;
    let (tmp_path, mut file) = create_temp(dir, name)?;
    let written = write(&mut file).and_then(|()| file.sync_all()).and_then(|()| checkpoint(1));
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp_path);
        return Err(PersistError::io("write", &tmp_path, e));
    }
    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(PersistError::io("rename into place", path, e));
    }
    checkpoint(2).map_err(|e| PersistError::io("after atomic rename", path, e))?;
    // The rename is only durable once the directory entry is.
    sync_dir(dir).map_err(|e| PersistError::io("fsync directory", dir, e))?;
    checkpoint(3).map_err(|e| PersistError::io("after atomic directory sync", dir, e))
}

fn create_temp(dir: &Path, name: &str) -> Result<(PathBuf, File)> {
    let pid = std::process::id();
    let mut last_err = None;
    for _ in 0..TMP_CREATE_ATTEMPTS {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = dir.join(format!(".{name}{TMP_MARKER}{pid}-{seq}"));
        match private_file_options().open(&tmp_path) {
            Ok(file) => return Ok((tmp_path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last_err = Some(e),
            Err(e) => return Err(PersistError::io("create temp file", &tmp_path, e)),
        }
    }
    Err(PersistError::io(
        "create temp file",
        dir,
        last_err.unwrap_or_else(|| io::Error::other("no free temp file name")),
    ))
}

/// Exclusive create, owner-only on unix (token hashes live in these files).
pub(crate) fn private_file_options() -> OpenOptions {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts
}

/// fsync a directory so a rename or create inside it survives power loss.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Create `path` (and missing parents) if needed. The final directory is owner-only on unix;
/// the new entry is made durable by syncing its parent.
pub(crate) fn ensure_dir(path: &Path) -> Result<()> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_dir() => return Ok(()),
        Ok(_) => return Err(PersistError::corrupt(path, "expected a directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(PersistError::io("stat", path, e)),
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| PersistError::io("create directory", parent, e))?;
        }
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {}
        // Lost a race with another creator; fine as long as it is a directory now.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => return Ok(()),
        Err(e) => return Err(PersistError::io("create directory", path, e)),
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            sync_dir(parent).map_err(|e| PersistError::io("fsync directory", parent, e))?;
        }
    }
    Ok(())
}

/// Read a regular file, refusing anything over `max` bytes. `Ok(None)` when it does not exist.
pub(crate) fn read_capped(path: &Path, max: u64) -> Result<Option<Vec<u8>>> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PersistError::io("open", path, e)),
    };
    let meta = file
        .metadata()
        .map_err(|e| PersistError::io("stat", path, e))?;
    if !meta.is_file() {
        return Err(PersistError::corrupt(path, "not a regular file"));
    }
    if meta.len() > max {
        return Err(PersistError::TooLarge {
            path: path.to_path_buf(),
            size: meta.len(),
            max,
        });
    }
    let mut buf = Vec::new();
    // Bounded even if the file grows between the stat and the read.
    (&mut file)
        .take(max.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|e| PersistError::io("read", path, e))?;
    let size = buf.len() as u64;
    if size > max {
        return Err(PersistError::TooLarge {
            path: path.to_path_buf(),
            size,
            max,
        });
    }
    Ok(Some(buf))
}

/// Serialize as pretty JSON (operators read these files) and write atomically.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T, max: u64) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| PersistError::corrupt(path, format!("cannot serialize record: {e}")))?;
    bytes.push(b'\n');
    if bytes.len() as u64 > max {
        return Err(PersistError::TooLarge {
            path: path.to_path_buf(),
            size: bytes.len() as u64,
            max,
        });
    }
    atomic_write(path, &bytes)
}

#[derive(Deserialize)]
struct VersionProbe {
    schema_version: Option<serde_json::Value>,
}

/// Validate `schema_version` before the typed decode, so a record from a newer build is
/// reported as such instead of as an unhelpful shape error.
pub(crate) fn check_schema_version(path: &Path, bytes: &[u8]) -> Result<u32> {
    check_schema_version_upto(path, bytes, SCHEMA_VERSION)
}

/// [`check_schema_version`] for a file kind whose newest known version is `supported` (the
/// index has its own version, independent of the record files).
pub(crate) fn check_schema_version_upto(path: &Path, bytes: &[u8], supported: u32) -> Result<u32> {
    let probe: VersionProbe = serde_json::from_slice(bytes)
        .map_err(|e| PersistError::corrupt(path, format!("not a valid JSON record: {e}")))?;
    let value = probe
        .schema_version
        .ok_or_else(|| PersistError::corrupt(path, "missing schema_version"))?;
    let found = value.as_u64().ok_or_else(|| {
        PersistError::corrupt(
            path,
            format!("schema_version must be an unsigned integer, got {value}"),
        )
    })?;
    let found = u32::try_from(found).map_err(|_| {
        PersistError::corrupt(path, format!("schema_version {found} is out of range"))
    })?;
    if found == 0 {
        Err(PersistError::InvalidSchema {
            path: path.to_path_buf(),
            found,
            supported,
        })
    } else if found > supported {
        Err(PersistError::FutureSchema {
            path: path.to_path_buf(),
            found,
            supported,
        })
    } else {
        Ok(found)
    }
}

/// Load and decode a record. `Ok(None)` when the file does not exist.
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path, max: u64) -> Result<Option<T>> {
    let Some(bytes) = read_capped(path, max)? else {
        return Ok(None);
    };
    check_schema_version(path, &bytes)?;
    serde_json::from_slice::<T>(&bytes)
        .map(Some)
        .map_err(|e| PersistError::corrupt(path, format!("invalid record: {e}")))
}

/// Remove temp files left behind by an interrupted write. Returns what was removed.
pub(crate) fn sweep_temp_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(PersistError::io("list directory", dir, e)),
    };
    let mut removed = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| PersistError::io("list directory", dir, e))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_temp_name(name) {
            continue;
        }
        // `file_type` does not follow symlinks, so a planted link is never touched.
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            let path = entry.path();
            fs::remove_file(&path).map_err(|e| PersistError::io("remove temp file", &path, e))?;
            removed.push(path);
        }
    }
    Ok(removed)
}

/// Names of regular `*.json` files directly inside `dir`, sorted. Missing dir -> empty.
pub(crate) fn list_json_files(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(PersistError::io("list directory", dir, e)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| PersistError::io("list directory", dir, e))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if name.ends_with(".json") && !is_temp_name(name) && is_file {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Names of subdirectories of `dir`, sorted. Missing dir -> empty.
pub(crate) fn list_subdirs(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(PersistError::io("list directory", dir, e)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| PersistError::io("list directory", dir, e))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_files_under(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_temp_name(&name) {
                found.push(entry.path());
            }
        }
        found
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.json");
        for i in 0..20u8 {
            atomic_write(&target, &[i; 64]).unwrap();
            assert_eq!(fs::read(&target).unwrap(), vec![i; 64]);
        }
        assert!(temp_files_under(dir.path()).is_empty());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_write_cleans_up_its_temp_file_and_keeps_old_data() {
        let dir = tempfile::tempdir().unwrap();
        // A directory in the way makes the final rename fail after the temp file exists.
        let blocked = dir.path().join("blocked.json");
        fs::create_dir(&blocked).unwrap();
        fs::write(blocked.join("keep"), b"x").unwrap();
        let err = atomic_write(&blocked, b"new").unwrap_err();
        assert!(matches!(err, PersistError::Io { .. }), "{err}");
        assert!(temp_files_under(dir.path()).is_empty());
        assert!(blocked.join("keep").exists());

        let missing_dir = dir.path().join("nope").join("x.json");
        assert!(atomic_write(&missing_dir, b"new").is_err());
        assert!(temp_files_under(dir.path()).is_empty());
    }

    #[test]
    fn temp_names_are_recognized_and_swept() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".x.json.tmp-1-2"), b"junk").unwrap();
        fs::write(dir.path().join("x.json"), b"keep").unwrap();
        fs::write(dir.path().join(".hidden"), b"keep").unwrap();
        fs::write(dir.path().join("LOCK"), b"keep").unwrap();
        let removed = sweep_temp_files(dir.path()).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(dir.path().join("x.json").exists());
        assert!(dir.path().join(".hidden").exists());
        assert!(dir.path().join("LOCK").exists());
    }

    #[test]
    fn read_capped_enforces_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        fs::write(&p, vec![7u8; 100]).unwrap();
        assert_eq!(read_capped(&p, 100).unwrap().unwrap().len(), 100);
        assert!(matches!(
            read_capped(&p, 99).unwrap_err(),
            PersistError::TooLarge {
                size: 100,
                max: 99,
                ..
            }
        ));
        assert!(read_capped(&dir.path().join("absent"), 10)
            .unwrap()
            .is_none());
        assert!(matches!(
            read_capped(dir.path(), 10).unwrap_err(),
            PersistError::Corrupt { .. }
        ));
    }

    #[test]
    fn schema_version_edge_cases_never_panic() {
        let p = Path::new("x.json");
        let check = |doc: &str| check_schema_version(p, doc.as_bytes());
        assert_eq!(check(r#"{"schema_version":1}"#).unwrap(), 1);
        assert!(matches!(
            check(r#"{"schema_version":2,"anything":[1,2]}"#),
            Err(PersistError::FutureSchema {
                found: 2,
                supported: 1,
                ..
            })
        ));
        assert!(matches!(
            check(r#"{"schema_version":0}"#),
            Err(PersistError::InvalidSchema { found: 0, .. })
        ));
        for corrupt in [
            "",
            "{",
            "null",
            "[]",
            "7",
            "\"x\"",
            r#"{}"#,
            r#"{"schema_version":null}"#,
            r#"{"schema_version":"1"}"#,
            r#"{"schema_version":1.0}"#,
            r#"{"schema_version":-1}"#,
            r#"{"schema_version":4294967296}"#,
            r#"{"schema_version":18446744073709551616}"#,
            r#"{"schema_version":[1]}"#,
            r#"{"schema_version":1,"schema_version":1}"#,
        ] {
            assert!(
                matches!(check(corrupt), Err(PersistError::Corrupt { .. })),
                "{corrupt}"
            );
        }
        assert!(matches!(
            check_schema_version(p, &[0xff, 0xfe, 0x00]),
            Err(PersistError::Corrupt { .. })
        ));
        let deep = format!("{}1{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(check(&deep).is_err());
    }
}
