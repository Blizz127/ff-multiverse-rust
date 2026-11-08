//! Single-writer guard for a state directory.
//!
//! `<root>/LOCK` is created exclusively and holds the owner's pid. The only required content
//! is `<pid>\n`; optional `boot_id=` and `start_ticks=` lines let a later opener tell a dead
//! owner from an unrelated process that was handed the same pid (after a reboot, or in a
//! container where the server is always pid 1).
//!
//! A LOCK whose owner is gone is taken over with a warning; a LOCK whose owner is alive, or
//! cannot be judged, refuses the open. Acquisition is serialized with an advisory `flock`
//! on the state directory so two servers starting at once after a crash cannot both take over.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::{PersistError, Result};
use crate::fsio;

pub(crate) const LOCK_FILE: &str = "LOCK";
const LOCK_READ_CAP: u64 = 4096;
const MAX_ACQUIRE_ATTEMPTS: usize = 4;

/// Holds `<root>/LOCK` for as long as it lives.
#[derive(Debug)]
pub(crate) struct LockGuard {
    path: PathBuf,
    pid: u32,
    start_ticks: Option<u64>,
}

/// Parsed LOCK file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LockInfo {
    pid: u32,
    boot_id: Option<String>,
    start_ticks: Option<u64>,
}

impl LockInfo {
    fn parse(text: &str) -> std::result::Result<LockInfo, String> {
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("").trim();
        let pid: u32 = first
            .parse()
            .map_err(|_| format!("first line must be a pid, got {first:?}"))?;
        if pid == 0 {
            return Err("pid 0 is not a process".into());
        }
        let mut info = LockInfo {
            pid,
            boot_id: None,
            start_ticks: None,
        };
        for line in lines {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "boot_id" => info.boot_id = Some(value.trim().to_owned()),
                "start_ticks" => info.start_ticks = value.trim().parse().ok(),
                _ => {}
            }
        }
        Ok(info)
    }

    fn render(&self) -> String {
        let mut out = format!("{}\n", self.pid);
        if let Some(boot_id) = &self.boot_id {
            out.push_str(&format!("boot_id={boot_id}\n"));
        }
        if let Some(ticks) = self.start_ticks {
            out.push_str(&format!("start_ticks={ticks}\n"));
        }
        out
    }
}

enum Verdict {
    Alive(String),
    Stale(String),
}

/// What `/proc/<pid>` says about a process.
#[derive(Debug, PartialEq, Eq)]
enum ProcState {
    Missing,
    Zombie,
    Running { start_ticks: Option<u64> },
    Unknown,
}

fn proc_state(pid: u32) -> ProcState {
    let path = format!("/proc/{pid}/stat");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        // ESRCH (3) is what reading /proc/<pid>/* returns once the process has just exited.
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(3) => {
            return ProcState::Missing
        }
        Err(_) => return ProcState::Unknown,
    };
    parse_proc_stat(&String::from_utf8_lossy(&bytes))
}

/// `pid (comm) S ppid ... starttime ...`. The comm may contain spaces and parentheses, so
/// everything is located relative to the *last* `)`.
fn parse_proc_stat(stat: &str) -> ProcState {
    let Some(close) = stat.rfind(')') else {
        return ProcState::Unknown;
    };
    let rest = stat.get(close + 1..).unwrap_or("");
    let mut fields = rest.split_whitespace();
    match fields.next() {
        // Z = zombie (exited, not yet reaped), X = dead.
        Some("Z") | Some("X") => ProcState::Zombie,
        Some(_) => {
            // `state` is field 3 and `starttime` is field 22, so it is the 19th after state.
            let start_ticks = fields.nth(18).and_then(|t| t.parse().ok());
            ProcState::Running { start_ticks }
        }
        None => ProcState::Unknown,
    }
}

fn current_boot_id() -> Option<String> {
    let text = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let id = text.trim();
    let sane =
        !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
    sane.then(|| id.to_owned())
}

fn own_start_ticks() -> Option<u64> {
    match proc_state(std::process::id()) {
        ProcState::Running { start_ticks } => start_ticks,
        _ => None,
    }
}

fn judge(info: &LockInfo) -> Verdict {
    // Without /proc nothing can be proven, so be conservative and keep the lock.
    if !Path::new("/proc/self").exists() {
        return Verdict::Alive("cannot check whether the owner is alive without /proc".into());
    }
    if let (Some(theirs), Some(ours)) = (&info.boot_id, current_boot_id()) {
        if *theirs != ours {
            return Verdict::Stale("it was written during an earlier boot".into());
        }
    }
    match proc_state(info.pid) {
        ProcState::Missing => Verdict::Stale(format!("process {} no longer exists", info.pid)),
        ProcState::Zombie => Verdict::Stale(format!("process {} has exited", info.pid)),
        ProcState::Unknown => {
            Verdict::Alive(format!("could not read the state of process {}", info.pid))
        }
        ProcState::Running { start_ticks } => match (info.start_ticks, start_ticks) {
            (Some(recorded), Some(actual)) if recorded != actual => Verdict::Stale(format!(
                "pid {} now belongs to a different process than the one that took the lock",
                info.pid
            )),
            _ => Verdict::Alive(format!("process {} is running", info.pid)),
        },
    }
}

/// Read the LOCK file. `Ok(None)` if it does not exist.
fn read_lock(path: &Path) -> Result<Option<LockInfo>> {
    let Some(bytes) = fsio::read_capped(path, LOCK_READ_CAP)? else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&bytes);
    LockInfo::parse(&text)
        .map(Some)
        .map_err(|detail| PersistError::LockCorrupt {
            path: path.to_path_buf(),
            detail,
        })
}

fn create_lock_file(path: &Path, info: &LockInfo) -> io::Result<()> {
    let mut file = fsio::private_file_options().open(path)?;
    // fsync the contents: after a power cut, a LOCK that survives must not be zero-length,
    // because an empty LOCK is refused as corrupt and would need manual cleanup.
    let written = file
        .write_all(info.render().as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(e) = written {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(e);
    }
    Ok(())
}

/// Advisory exclusive lock on the state directory, held only while acquiring LOCK.
#[cfg(unix)]
struct AcquireSerializer(#[allow(dead_code)] File);

#[cfg(unix)]
fn serialize_acquire(root: &Path) -> Option<AcquireSerializer> {
    let dir = File::open(root).ok()?;
    // Released when the descriptor closes. If the filesystem does not support flock we
    // proceed unserialized; the exclusive create below still refuses a live owner.
    dir.lock().ok()?;
    Some(AcquireSerializer(dir))
}

#[cfg(not(unix))]
struct AcquireSerializer;

#[cfg(not(unix))]
fn serialize_acquire(_root: &Path) -> Option<AcquireSerializer> {
    None
}

impl LockGuard {
    /// Take `<root>/LOCK`, taking over a stale one with a warning. `root` must exist.
    pub(crate) fn acquire(root: &Path, warnings: &mut Vec<String>) -> Result<LockGuard> {
        let path = root.join(LOCK_FILE);
        let _serialize = serialize_acquire(root);
        let me = LockInfo {
            pid: std::process::id(),
            boot_id: current_boot_id(),
            start_ticks: own_start_ticks(),
        };

        for _ in 0..MAX_ACQUIRE_ATTEMPTS {
            match create_lock_file(&path, &me) {
                Ok(()) => {
                    return Ok(LockGuard {
                        path,
                        pid: me.pid,
                        start_ticks: me.start_ticks,
                    })
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(PersistError::io("create lock file", &path, e)),
            }

            // Someone holds, or held, the lock.
            let Some(holder) = read_lock(&path)? else {
                continue; // vanished between the create attempt and the read
            };
            match judge(&holder) {
                Verdict::Alive(detail) => {
                    return Err(PersistError::Locked {
                        path,
                        pid: holder.pid,
                        detail,
                    })
                }
                Verdict::Stale(reason) => {
                    let message = format!("taking over stale lock {}: {reason}", path.display());
                    tracing::warn!("{message}");
                    warnings.push(message);
                    match fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(PersistError::io("remove stale lock", &path, e)),
                    }
                }
            }
        }
        Err(PersistError::LockCorrupt {
            path,
            detail: "could not acquire the lock after repeated attempts; another process keeps \
                     racing for it"
                .into(),
        })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Only remove the file if it is still the one we wrote.
        if let Ok(Some(holder)) = read_lock(&self.path) {
            if holder.pid == self.pid && holder.start_ticks == self.start_ticks {
                let _ = fs::remove_file(&self.path);
                if let Some(dir) = self.path.parent() {
                    let _ = fsio::sync_dir(dir);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_pid_and_full_formats() {
        let bare = LockInfo::parse("1234\n").unwrap();
        assert_eq!(
            bare,
            LockInfo {
                pid: 1234,
                boot_id: None,
                start_ticks: None
            }
        );
        assert_eq!(LockInfo::parse("1234").unwrap().pid, 1234);
        assert_eq!(LockInfo::parse(" 77 \r\n").unwrap().pid, 77);

        let full = LockInfo {
            pid: 9,
            boot_id: Some("abc-123".into()),
            start_ticks: Some(555),
        };
        assert_eq!(LockInfo::parse(&full.render()).unwrap(), full);
        // Unknown and malformed extra lines are ignored.
        let odd = LockInfo::parse("5\nfuture=1\nnoise\nstart_ticks=zzz\n").unwrap();
        assert_eq!((odd.pid, odd.start_ticks), (5, None));
    }

    #[test]
    fn rejects_garbage_lock_contents() {
        for bad in [
            "",
            "\n",
            "abc",
            "0",
            "-1",
            "12 34",
            "99999999999",
            "\u{0}\u{1}",
        ] {
            assert!(LockInfo::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn proc_stat_parsing_survives_hostile_comm_names() {
        let line = |comm: &str| {
            format!(
                "42 ({comm}) S 1 42 42 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 10 \
                 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0"
            )
        };
        for comm in ["bash", "a b c", "evil) S 1 2 (x", "))((", ""] {
            assert_eq!(
                parse_proc_stat(&line(comm)),
                ProcState::Running {
                    start_ticks: Some(987654)
                },
                "{comm:?}"
            );
        }
        assert_eq!(parse_proc_stat("42 (x) Z 1 2"), ProcState::Zombie);
        assert_eq!(parse_proc_stat("42 (x) X 1 2"), ProcState::Zombie);
        assert_eq!(parse_proc_stat("garbage"), ProcState::Unknown);
        assert_eq!(parse_proc_stat("42 (x)"), ProcState::Unknown);
        assert_eq!(
            parse_proc_stat("42 (x) S 1"),
            ProcState::Running { start_ticks: None }
        );
    }

    #[test]
    fn own_process_is_running_with_start_ticks() {
        assert!(matches!(
            proc_state(std::process::id()),
            ProcState::Running {
                start_ticks: Some(_)
            }
        ));
        assert_eq!(proc_state(u32::MAX - 1), ProcState::Missing);
    }
}
