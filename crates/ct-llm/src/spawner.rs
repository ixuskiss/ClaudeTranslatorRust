//! Process spawning seam for the Claude CLI client (real `std::process` or a test fake).

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

/// Number of trailing stderr bytes kept.
const STDERR_TAIL_BYTES: usize = 4096;

/// Ring buffer keeping only the last `STDERR_TAIL_BYTES` bytes written to it.
#[derive(Default)]
pub struct StderrTail {
    buf: std::collections::VecDeque<u8>,
}

impl StderrTail {
    pub fn push(&mut self, bytes: &[u8]) {
        let bytes = &bytes[bytes.len().saturating_sub(STDERR_TAIL_BYTES)..];
        let overflow = (self.buf.len() + bytes.len()).saturating_sub(STDERR_TAIL_BYTES);
        self.buf.drain(..overflow);
        self.buf.extend(bytes);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// Handle to a spawned CLI process.
pub trait ChildHandle: Send {
    fn is_running(&mut self) -> bool;
    /// Best-effort, non-blocking-ish termination. Must never panic.
    fn terminate(&mut self);
    /// Length (not content) of the captured stderr tail.
    fn stderr_tail_len(&self) -> usize;
    /// Exit code if the process has already exited (diagnostics only).
    fn exit_code(&mut self) -> Option<i32>;
}

pub struct SpawnedCli {
    pub stdin: Box<dyn Write + Send>,
    pub stdout: Box<dyn Read + Send>,
    pub child: Box<dyn ChildHandle>,
}

pub trait CliSpawner: Send + Sync {
    /// `args[0]` is the program, the rest are its arguments.
    fn spawn(&self, args: &[String], cwd: Option<&Path>) -> io::Result<SpawnedCli>;
}

/// Spawns the real CLI: stdin/stdout/stderr piped (only a 4 KB stderr tail is kept),
/// no console window on Windows.
pub struct RealSpawner;

/// `CREATE_NO_WINDOW`: without it a console-less (tray) parent makes Windows open a new
/// console for the CLI's process tree, which breaks the stdin/stdout pipe handshake.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

impl CliSpawner for RealSpawner {
    fn spawn(&self, args: &[String], cwd: Option<&Path>) -> io::Result<SpawnedCli> {
        let (program, rest) = args
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
        let mut cmd = Command::new(program);
        cmd.args(rest)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("child pipes unavailable"));
        };
        let tail = Arc::new(Mutex::new(StderrTail::default()));
        if let Some(mut stderr) = child.stderr.take() {
            let sink = Arc::clone(&tail);
            let spawned = thread::Builder::new()
                .name("claude-cli-stderr".into())
                .spawn(move || {
                    let mut chunk = [0u8; 1024];
                    while let Ok(n) = stderr.read(&mut chunk) {
                        if n == 0 {
                            break;
                        }
                        if let Ok(mut t) = sink.lock() {
                            t.push(&chunk[..n]);
                        }
                    }
                });
            if let Err(e) = spawned {
                tracing::warn!(error = %e, "stderr reader thread failed to start");
            }
        }
        #[cfg(windows)]
        let job = job::attach(&child);
        Ok(SpawnedCli {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            child: Box::new(RealChild {
                child,
                tail,
                #[cfg(windows)]
                job,
            }),
        })
    }
}

/// Job Object that kills the whole CLI process tree (Windows only).
#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    pub struct Job(HANDLE);

    // SAFETY: a job handle is a process-wide kernel object handle usable from any thread.
    unsafe impl Send for Job {}

    impl Job {
        pub fn terminate(&self) -> bool {
            // SAFETY: the handle is valid until Drop.
            unsafe { TerminateJobObject(self.0, 1) }.is_ok()
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is owned by this value and closed exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// Creates a kill-on-close job and puts `child` in it; any failure only warns.
    pub fn attach(child: &Child) -> Option<Job> {
        // SAFETY: plain Win32 call; the handle is wrapped at once.
        let handle = match unsafe { CreateJobObjectW(None, None) } {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(error = %e, "CreateJobObjectW failed; continuing without job");
                return None;
            }
        };
        let job = Job(handle);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is a properly sized, initialised struct for this info class.
        let set = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if let Err(e) = set {
            tracing::warn!(error = %e, "SetInformationJobObject failed; continuing without job");
            return None;
        }
        let process = HANDLE(child.as_raw_handle());
        // SAFETY: both handles are valid; the child handle is owned by `child`.
        if let Err(e) = unsafe { AssignProcessToJobObject(job.0, process) } {
            tracing::warn!(error = %e, "AssignProcessToJobObject failed; continuing without job");
            return None;
        }
        Some(job)
    }
}

struct RealChild {
    child: Child,
    tail: Arc<Mutex<StderrTail>>,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl ChildHandle for RealChild {
    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn terminate(&mut self) {
        #[cfg(windows)]
        if let Some(job) = &self.job
            && job.terminate()
        {
            let _ = self.child.wait();
            return;
        }
        // kill() is TerminateProcess on Windows; only after it succeeded (or the process
        // already exited) is wait() guaranteed to return promptly, so never wait otherwise.
        if !self.is_running() || self.child.kill().is_ok() {
            let _ = self.child.wait();
        }
    }

    fn exit_code(&mut self) -> Option<i32> {
        self.child.try_wait().ok().flatten().and_then(|s| s.code())
    }

    fn stderr_tail_len(&self) -> usize {
        self.tail.lock().map(|t| t.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_ring_keeps_last_4096_bytes() {
        let mut t = StderrTail::default();
        t.push(&[1u8; 3000]);
        t.push(&[2u8; 3000]);
        assert_eq!(t.len(), 4096);
        assert!(t.buf.iter().take(1096).all(|&b| b == 1));
        assert!(t.buf.iter().skip(1096).all(|&b| b == 2));
        t.push(&[3u8; 5000]);
        assert_eq!(t.len(), 4096);
        assert!(t.buf.iter().all(|&b| b == 3));
        assert!(!t.is_empty());
    }

    #[test]
    fn real_spawner_rejects_empty_command() {
        let err = RealSpawner.spawn(&[], None).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
