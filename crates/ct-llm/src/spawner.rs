//! Process spawning seam for the Claude CLI client (real `std::process` or a test fake).

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Handle to a spawned CLI process.
pub trait ChildHandle: Send {
    fn is_running(&mut self) -> bool;
    /// Best-effort, non-blocking-ish termination. Must never panic.
    fn terminate(&mut self);
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

/// Spawns the real CLI: stdin/stdout piped, stderr discarded, no console window on Windows.
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
            .stderr(Stdio::null());
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
        Ok(SpawnedCli {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            child: Box::new(RealChild(child)),
        })
    }
}

struct RealChild(Child);

impl ChildHandle for RealChild {
    fn is_running(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }

    fn terminate(&mut self) {
        // kill() is TerminateProcess on Windows; only after it succeeded (or the process
        // already exited) is wait() guaranteed to return promptly, so never wait otherwise.
        if !self.is_running() || self.0.kill().is_ok() {
            let _ = self.0.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_spawner_rejects_empty_command() {
        let err = RealSpawner.spawn(&[], None).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
