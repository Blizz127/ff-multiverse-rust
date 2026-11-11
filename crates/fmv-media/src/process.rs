//! Own exactly one child, drain its stderr independently, and reap on every exit.

use crate::MediaError;
use std::ffi::OsString;
use std::io::{Read, Result as IoResult};
use std::path::Path;
use std::process::{Child, ChildStdout, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(5);

pub(crate) fn check(cancel: &AtomicBool, deadline: Instant) -> Result<(), MediaError> {
    if cancel.load(Ordering::Acquire) {
        Err(MediaError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(MediaError::TimedOut)
    } else {
        Ok(())
    }
}

struct OwnedChild {
    child: Arc<Mutex<Child>>,
    abort: Arc<AtomicBool>,
    monitor: Option<JoinHandle<Result<ExitStatus, MediaError>>>,
    stderr: Option<JoinHandle<IoResult<Vec<u8>>>>,
}

#[cfg(unix)]
fn nonblocking(pipe: &impl std::os::fd::AsFd) -> Result<(), MediaError> {
    let flags = rustix::fs::fcntl_getfl(pipe).map_err(std::io::Error::from)?;
    rustix::fs::fcntl_setfl(pipe, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(std::io::Error::from)?;
    Ok(())
}

#[cfg(not(unix))]
fn nonblocking<T>(_: &T) -> Result<(), MediaError> {
    Err(MediaError::Invalid(
        "the bounded subprocess reader currently requires Unix".into(),
    ))
}

#[cfg(unix)]
fn wait_readable(pipe: &ChildStdout) -> IoResult<()> {
    use rustix::event::{poll, PollFd, PollFlags, Timespec};
    let mut ready = [PollFd::new(pipe, PollFlags::IN)];
    let timeout = Timespec {
        tv_sec: 0,
        tv_nsec: 5_000_000,
    };
    match poll(&mut ready, Some(&timeout)) {
        Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn wait_readable(_: &ChildStdout) -> IoResult<()> {
    // Non-Unix startup is refused before constructing a CheckedReader.
    Err(std::io::Error::other(
        "nonblocking movie pipes require Unix",
    ))
}

pub(crate) struct CheckedReader {
    pipe: ChildStdout,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
}

impl Read for CheckedReader {
    fn read(&mut self, out: &mut [u8]) -> IoResult<usize> {
        loop {
            check(&self.cancel, self.deadline).map_err(std::io::Error::other)?;
            match self.pipe.read(out) {
                // A ready pipe wakes immediately; sleeping after every short read
                // would throttle multi-megabyte RGBA frames to the pipe's small capacity.
                // Both readiness and EINTR return through the cancellation/deadline check.
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => wait_readable(&self.pipe)?,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

impl OwnedChild {
    fn start(
        tool: &Path,
        args: &[OsString],
        stderr_cap: usize,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<(Self, CheckedReader), MediaError> {
        check(&cancel, deadline)?;
        let mut child = std::process::Command::new(tool)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(MediaError::Io("decoder stdout was not piped".into()));
        };
        let Some(stderr) = child.stderr.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(MediaError::Io("decoder stderr was not piped".into()));
        };
        let mut owned = Self {
            child: Arc::new(Mutex::new(child)),
            abort: Arc::new(AtomicBool::new(false)),
            monitor: None,
            stderr: None,
        };
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        // Keep only the bounded prefix, but drain the rest so a noisy decoder cannot
        // deadlock while its stdout is being consumed. No shell/descendants are launched.
        let stderr_cancel = Arc::clone(&cancel);
        let stderr_abort = Arc::clone(&owned.abort);
        owned.stderr = Some(
            thread::Builder::new()
                .name("fmv-movie-stderr".into())
                .spawn(move || {
                    drain(stderr, stderr_cap, &stderr_cancel, &stderr_abort, deadline)
                })?,
        );
        let reader_cancel = Arc::clone(&cancel);
        let (process, abort) = (Arc::clone(&owned.child), Arc::clone(&owned.abort));
        owned.monitor = Some(
            thread::Builder::new()
                .name("fmv-movie-watch".into())
                .spawn(move || loop {
                    let reason = check(&cancel, deadline).err().or_else(|| {
                        abort
                            .load(Ordering::Acquire)
                            .then_some(MediaError::Cancelled)
                    });
                    let mut child = process.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(reason) = reason {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(reason);
                    }
                    if let Some(status) = child.try_wait()? {
                        return Ok(status);
                    }
                    drop(child);
                    thread::sleep(POLL);
                })?,
        );
        Ok((
            owned,
            CheckedReader {
                pipe: stdout,
                cancel: reader_cancel,
                deadline,
            },
        ))
    }

    fn finish(&mut self, tool: &Path) -> Result<(), MediaError> {
        let status = self
            .monitor
            .take()
            .ok_or_else(|| MediaError::Io("missing process monitor".into()))?
            .join()
            .map_err(|_| MediaError::WorkerPanic)
            .and_then(|result| result);
        if status.is_err() {
            self.abort.store(true, Ordering::Release);
            let mut child = self.child.lock().unwrap_or_else(|p| p.into_inner());
            let _ = child.kill();
            let _ = child.wait();
        }
        let stderr = self
            .stderr
            .take()
            .ok_or_else(|| MediaError::Io("missing stderr reader".into()))?
            .join()
            .map_err(|_| MediaError::WorkerPanic)??;
        let status = status?;
        // Calls use error-only verbosity. An error diagnostic invalidates a decode
        // even when that FFmpeg version's error-tolerance policy still exits zero.
        if !status.success() || !stderr.is_empty() {
            return Err(MediaError::ToolFailed {
                tool: tool.display().to_string(),
                code: status.code(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            });
        }
        Ok(())
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.abort.store(true, Ordering::Release);
        // Also covers thread-spawn failure or an unwinding consumer. This is only
        // the Child returned by our own spawn; no process-name lookup/group kill.
        {
            let mut child = self.child.lock().unwrap_or_else(|p| p.into_inner());
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.join();
        }
    }
}

fn drain(
    mut input: impl Read,
    cap: usize,
    cancel: &AtomicBool,
    abort: &AtomicBool,
    deadline: Instant,
) -> IoResult<Vec<u8>> {
    let mut kept = Vec::new();
    let mut bytes = [0u8; 8192];
    loop {
        if check(cancel, deadline).is_err() || abort.load(Ordering::Acquire) {
            return Ok(kept);
        }
        let n = match input.read(&mut bytes) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(POLL);
                continue;
            }
            result => result?,
        };
        if n == 0 {
            return Ok(kept);
        }
        let take = n.min(cap.saturating_sub(kept.len()));
        if let Some(prefix) = bytes.get(..take) {
            kept.extend_from_slice(prefix);
        }
    }
}

pub(crate) fn run<T>(
    tool: &Path,
    args: &[OsString],
    stderr_cap: usize,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    read: impl FnOnce(&mut CheckedReader) -> Result<T, MediaError>,
) -> Result<T, MediaError> {
    let (mut child, mut stdout) =
        OwnedChild::start(tool, args, stderr_cap, Arc::clone(&cancel), deadline)?;
    let result = read(&mut stdout);
    drop(stdout);
    if result.is_err() {
        child.abort.store(true, Ordering::Release);
    }
    let exit = child.finish(tool);
    check(&cancel, deadline)?;
    // Preserve a concrete limit/truncation failure rather than its ensuing kill.
    let value = result?;
    exit?;
    Ok(value)
}

pub(crate) fn capture(
    tool: &Path,
    args: &[OsString],
    cap: usize,
    stderr_cap: usize,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
) -> Result<Vec<u8>, MediaError> {
    run(tool, args, stderr_cap, cancel, deadline, |stdout| {
        let mut bytes = Vec::new();
        stdout
            .take(cap.saturating_add(1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > cap {
            return Err(MediaError::Limit("subprocess output"));
        }
        Ok(bytes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // These processes are fixtures owned by the test, never the game. Python is
    // explicitly selected only for tests; production tools are supplied by callers.
    #[cfg(unix)]
    fn python() -> Option<std::path::PathBuf> {
        ["/usr/bin/python3", "/bin/python3"]
            .into_iter()
            .map(std::path::PathBuf::from)
            .find(|p| p.is_file())
    }

    #[cfg(unix)]
    #[test]
    fn failure_after_valid_stdout_is_not_success_and_stderr_cannot_block() {
        let Some(tool) = python() else { return };
        let args = vec![
            "-c".into(),
            "import sys; sys.stderr.write('x'*100000); sys.stdout.buffer.write(b'ok'); sys.exit(7)"
                .into(),
        ];
        let result = capture(
            &tool,
            &args,
            16,
            32,
            Arc::new(AtomicBool::new(false)),
            Instant::now() + Duration::from_secs(5),
        );
        assert!(
            matches!(result, Err(MediaError::ToolFailed { code: Some(7), stderr, .. }) if stderr.len() == 32)
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_reaps_a_blocked_owned_process() {
        let Some(tool) = python() else { return };
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancel);
        let set = thread::spawn(move || {
            thread::sleep(Duration::from_millis(40));
            signal.store(true, Ordering::Release);
        });
        let args = vec!["-c".into(), "import time; time.sleep(30)".into()];
        let start = Instant::now();
        let result = capture(&tool, &args, 16, 32, cancel, start + Duration::from_secs(5));
        set.join().expect("cancellation fixture");
        assert_eq!(result, Err(MediaError::Cancelled));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn lifetime_and_output_caps_end_owned_processes() {
        let Some(tool) = python() else { return };
        let args = vec!["-c".into(), "import time; time.sleep(30)".into()];
        let result = capture(
            &tool,
            &args,
            16,
            32,
            Arc::new(AtomicBool::new(false)),
            Instant::now() + Duration::from_millis(50),
        );
        assert_eq!(result, Err(MediaError::TimedOut));
        let args = vec![
            "-c".into(),
            "import sys; sys.stdout.buffer.write(b'x'*1000000)".into(),
        ];
        assert_eq!(
            capture(
                &tool,
                &args,
                16,
                32,
                Arc::new(AtomicBool::new(false)),
                Instant::now() + Duration::from_secs(5)
            ),
            Err(MediaError::Limit("subprocess output"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn error_diagnostics_refuse_even_a_zero_exit_status() {
        let Some(tool) = python() else { return };
        let args = vec![
            "-c".into(),
            "import sys; sys.stdout.buffer.write(b'ok'); sys.stderr.write('corrupt packet')".into(),
        ];
        assert!(
            matches!(capture(&tool, &args, 16, 32, Arc::new(AtomicBool::new(false)), Instant::now() + Duration::from_secs(5)),
            Err(MediaError::ToolFailed { code: Some(0), stderr, .. }) if stderr == "corrupt packet")
        );
    }

    #[cfg(unix)]
    #[test]
    fn inherited_non_eof_pipes_do_not_remove_reader_deadlines() {
        // An owned fixture pipe without a writer is enough to model a still-open
        // inherited stdout: the checked reader must observe cancellation/deadline,
        // regardless of whether the direct-child status monitor has finished.
        let Some(tool) = python() else { return };
        let cancel = Arc::new(AtomicBool::new(false));
        let mut child = std::process::Command::new(&tool)
            .args(["-c", "import time; time.sleep(30)"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("fixture child");
        let pipe = child.stdout.take().expect("fixture pipe");
        nonblocking(&pipe).expect("nonblocking fixture");
        let mut reader = CheckedReader {
            pipe,
            cancel,
            deadline: Instant::now() + Duration::from_millis(40),
        };
        let result = reader.read(&mut [0u8; 1]);
        let _ = child.kill();
        child.wait().expect("reaped fixture");
        assert!(
            result.is_err(),
            "read deadline does not depend on the process monitor"
        );
    }
}
