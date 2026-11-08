//! Small filesystem helpers: private directories and files, unique temp names, durable directory
//! creation, atomic file replacement, advisory locks and sweeping of leftovers of killed writers.

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A leftover temp file (`.<name>.tmp-*`, `blob-*.part`) younger than this is never swept: its
/// writer may have only just started. The lock of the thing it belongs to, not this age, is what
/// protects a running writer; the age only covers the instant between "file created" and "lock
/// taken". Kept short so that a re-run right after a killed import already reclaims its leftovers.
pub const STALE_AFTER: Duration = Duration::from_secs(10);

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

/// Open (creating it if needed, never truncating) a file that exists to carry an advisory lock:
/// read+write, mode 0600 on unix. Lock files are never deleted by the importers (the one
/// exception, [`crate::Library::remove_set`], is made safe by [`lock_names_path`]), so a lock
/// can never be taken on a file that no longer has a name.
pub fn open_lock_file(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// `create_dir_all`, mode 0700 on unix. The library holds the owner's data, not for sharing.
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(path)
}

/// [`create_private_dir_all`] that also makes the new directories durable: every directory this
/// call created has its entry fsynced in its parent, so a power loss cannot leave a (durable)
/// manifest or blob that sits in a directory that never made it to disk. A directory that
/// already existed costs nothing extra.
pub fn create_private_dir_durable(path: &Path) -> io::Result<()> {
    // the new components, deepest first
    let mut created: Vec<&Path> = Vec::new();
    let mut cur = Some(path);
    while let Some(p) = cur {
        if p.as_os_str().is_empty() || p.exists() {
            break;
        }
        created.push(p);
        cur = p.parent();
    }
    create_private_dir_all(path)?;
    // top-down: a directory's entry is fsynced in the (by then durable) parent
    for p in created.iter().rev() {
        if let Some(parent) = p.parent().filter(|q| !q.as_os_str().is_empty()) {
            sync_dir(parent);
        }
    }
    Ok(())
}

/// Best-effort fsync of a directory so a rename inside it survives a crash.
pub(crate) fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// What taking an advisory lock came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockOutcome {
    /// This process holds the exclusive lock now.
    Held,
    /// Another holder has it.
    Busy,
    /// The filesystem has no advisory locks (some network and FUSE mounts): nothing is held and
    /// nothing can be learned about other writers. Callers carry on and say so.
    Unsupported,
}

thread_local! {
    static PRETEND_NO_LOCKS: Cell<bool> = const { Cell::new(false) };
}

/// TEST SEAM: make [`try_lock_exclusive`] on this thread answer [`LockOutcome::Unsupported`], as a
/// filesystem without advisory locks would. Not part of the supported interface.
#[doc(hidden)]
pub fn pretend_locks_are_unsupported(on: bool) {
    PRETEND_NO_LOCKS.with(|c| c.set(on));
}

fn lock_unsupported(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    // ENOLCK, ENOSYS and EOPNOTSUPP: what NFS without lockd, and FUSE filesystems that do not
    // implement flock, answer
    #[cfg(target_os = "linux")]
    {
        matches!(e.raw_os_error(), Some(37 | 38 | 95))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Try to take an exclusive advisory lock on `file` without waiting. `Err` only for failures
/// that say nothing about locks being unavailable (those are [`LockOutcome::Unsupported`]).
pub fn try_lock_exclusive(file: &File) -> io::Result<LockOutcome> {
    if PRETEND_NO_LOCKS.with(Cell::get) {
        return Ok(LockOutcome::Unsupported);
    }
    match file.try_lock() {
        Ok(()) => Ok(LockOutcome::Held),
        Err(fs::TryLockError::WouldBlock) => Ok(LockOutcome::Busy),
        Err(fs::TryLockError::Error(e)) if lock_unsupported(&e) => Ok(LockOutcome::Unsupported),
        Err(fs::TryLockError::Error(e)) => Err(e),
    }
}

/// True when `path` still names the very file `held` is open on. A lock taken on a file whose
/// name was removed (or replaced) in the meantime protects nobody: the next writer creates a new
/// file at that name and locks that one.
pub fn lock_names_path(held: &File, path: &Path) -> bool {
    let (Ok(by_name), Ok(by_handle)) = (fs::symlink_metadata(path), held.metadata()) else {
        return false;
    };
    by_name.is_file() && same_file(&by_name, &by_handle)
}

/// Same device and inode (unix); elsewhere there is no cheap identity, so only the kind and the
/// length are compared.
pub(crate) fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        a.is_file() == b.is_file() && a.len() == b.len()
    }
}

/// Atomically replace `path`: the writer fills a temp file in the SAME directory, which is
/// flushed and fsynced, then renamed over `path`. If the writer (or any later step) fails the
/// temp file is removed and the previous content of `path` is untouched. A writer that is KILLED
/// leaves its temp file (`.<name>.tmp-*`) behind: [`sweep_stale_temps`] reclaims it.
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

/// Remove the leftovers `.<final_name>.tmp-*` that [`atomic_write_with`] leaves when its process
/// is killed mid-write, from `dir`. Returns how many were removed.
///
/// The CALLER must hold whatever lock serialises writers of `final_name` (the set lock for a
/// manifest, the session lock for a session file); only regular files at least [`STALE_AFTER`]
/// old are touched, and nothing else in `dir` is.
pub fn sweep_stale_temps(dir: &Path, final_name: &str, now: SystemTime) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let prefix = format!(".{final_name}.tmp-");
    let mut removed = 0u64;
    for ent in entries.flatten() {
        let name = ent.file_name();
        if !name.to_str().is_some_and(|n| n.starts_with(&prefix)) {
            continue;
        }
        let path = ent.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let old_enough = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= STALE_AFTER);
        if meta.is_file() && old_enough && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backdate(path: &Path, secs: u64) {
        let f = OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    /// Finding 10: new directories are private, and creating them is durable.
    #[cfg(unix)]
    #[test]
    fn nested_private_directories_are_created_0700_and_idempotently() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("a").join("b").join("c");
        create_private_dir_durable(&deep).unwrap();
        for p in [tmp.path().join("a"), tmp.path().join("a/b"), deep.clone()] {
            assert_eq!(mode(&p) & 0o077, 0, "{p:?} must not be group/world accessible");
        }
        // a second call is a no-op, and an existing directory keeps whatever mode it has
        fs::set_permissions(&deep, fs::Permissions::from_mode(0o750)).unwrap();
        create_private_dir_durable(&deep).unwrap();
        assert_eq!(mode(&deep), 0o750);
        // a file in the way is an error, not a silent success
        let file = tmp.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(create_private_dir_durable(&file.join("sub")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn lock_files_are_created_0600_and_never_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join(".x.lock");
        let f = open_lock_file(&p).unwrap();
        assert_eq!(mode(&p) & 0o077, 0);
        drop(f);
        fs::write(&p, b"keep").unwrap();
        drop(open_lock_file(&p).unwrap());
        assert_eq!(fs::read(&p).unwrap(), b"keep");
    }

    #[test]
    fn a_lock_is_only_believed_while_its_path_still_names_the_locked_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join(".lock");
        let held = open_lock_file(&p).unwrap();
        assert_eq!(try_lock_exclusive(&held).unwrap(), LockOutcome::Held);
        assert!(lock_names_path(&held, &p));
        let other = open_lock_file(&p).unwrap();
        assert_eq!(try_lock_exclusive(&other).unwrap(), LockOutcome::Busy);
        drop(other);

        // the name is removed: the lock is a ghost (nobody else is kept out by it)
        fs::remove_file(&p).unwrap();
        assert!(!lock_names_path(&held, &p));
        // ... and it stays a ghost when another file takes the name over
        let successor = open_lock_file(&p).unwrap();
        assert!(!lock_names_path(&held, &p));
        assert!(lock_names_path(&successor, &p));
        assert_eq!(try_lock_exclusive(&successor).unwrap(), LockOutcome::Held);
        // a symbolic link at the name is not the locked file either
        #[cfg(unix)]
        {
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(&p, &link).unwrap();
            assert!(!lock_names_path(&successor, &link));
        }
    }

    #[test]
    fn filesystems_without_locks_are_reported_not_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let f = open_lock_file(&tmp.path().join(".lock")).unwrap();
        pretend_locks_are_unsupported(true);
        let outcome = try_lock_exclusive(&f);
        pretend_locks_are_unsupported(false);
        assert_eq!(outcome.unwrap(), LockOutcome::Unsupported);
        assert_eq!(try_lock_exclusive(&f).unwrap(), LockOutcome::Held);
        // the errors of such filesystems are recognised
        assert!(lock_unsupported(&io::Error::from(io::ErrorKind::Unsupported)));
        #[cfg(target_os = "linux")]
        for errno in [37, 38, 95] {
            assert!(lock_unsupported(&io::Error::from_raw_os_error(errno)), "{errno}");
        }
        // ... and an ordinary failure is not mistaken for one
        assert!(!lock_unsupported(&io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(!lock_unsupported(&io::Error::other("boom")));
    }

    /// Finding 10: the temp files of a writer that was killed mid-write are reclaimed.
    #[test]
    fn leftovers_of_killed_atomic_writes_are_swept_but_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mk = |name: &str| {
            let p = dir.join(name);
            fs::write(&p, vec![1u8; 64]).unwrap();
            p
        };
        let stale = mk(".manifest.json.tmp-123-0-aaaaaaaa");
        let stale2 = mk(".manifest.json.tmp-124-1-bbbbbbbb");
        // a writer that has only just started (dated ahead, so that no slow machine can age it)
        let fresh = mk(".manifest.json.tmp-125-2-cccccccc");
        OpenOptions::new()
            .write(true)
            .open(&fresh)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(3600))
            .unwrap();
        let other_target = mk(".session.json.tmp-1-0-dddddddd");
        let real = mk("manifest.json");
        let lookalike = mk("manifest.json.tmp-9"); // no leading dot: not ours
        let dir_named = dir.join(".manifest.json.tmp-dir");
        fs::create_dir(&dir_named).unwrap();
        for p in [&stale, &stale2, &other_target, &real, &lookalike] {
            backdate(p, 3600);
        }
        let swept = sweep_stale_temps(dir, "manifest.json", SystemTime::now());
        assert_eq!(swept, 2);
        assert!(!stale.exists() && !stale2.exists());
        assert!(fresh.exists(), "a young temp file may belong to a writer that just started");
        assert!(other_target.exists(), "only the temp files of the named target are touched");
        assert!(real.exists() && lookalike.exists() && dir_named.is_dir());
        // symbolic links are never followed or removed
        #[cfg(unix)]
        {
            let victim = mk("victim.txt");
            backdate(&victim, 3600);
            let link = dir.join(".manifest.json.tmp-link");
            std::os::unix::fs::symlink(&victim, &link).unwrap();
            assert_eq!(sweep_stale_temps(dir, "manifest.json", SystemTime::now()), 0);
            assert!(link.exists() && victim.exists());
        }
        // a directory that is not there is not an error
        assert_eq!(sweep_stale_temps(&dir.join("missing"), "manifest.json", SystemTime::now()), 0);
    }
}
