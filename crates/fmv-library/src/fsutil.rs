//! Small filesystem helpers: private directories, unique temp names, atomic file replacement.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A name suffix that will not collide in practice: pid + process counter + clock nanos.
pub(crate) fn unique_suffix() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{}-{}-{:08x}", std::process::id(), n, nanos)
}

/// Create a new file (fails if it exists), mode 0600 on unix.
pub(crate) fn open_new_private(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// `create_dir_all`, mode 0700 on unix. The library holds the owner's data, not for sharing.
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(path)
}

/// Best-effort fsync of a directory so a rename inside it survives a crash.
pub(crate) fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Atomically replace `path`: the writer fills a temp file in the SAME directory, which is
/// flushed and fsynced, then renamed over `path`. If the writer (or any later step) fails the
/// temp file is removed and the previous content of `path` is untouched.
pub fn atomic_write_with<F>(path: &Path, write: F) -> io::Result<()>
where
    F: FnOnce(&mut File) -> io::Result<()>,
{
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut attempt = 0u32;
    let (tmp, mut file) = loop {
        let tmp = dir.join(format!(".{name}.tmp-{}", unique_suffix()));
        match open_new_private(&tmp) {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempt < 8 => attempt += 1,
            Err(e) => return Err(e),
        }
    };
    let result = write(&mut file)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(dir);
    Ok(())
}

/// [`atomic_write_with`] for an in-memory buffer.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    atomic_write_with(path, |f| f.write_all(data))
}
