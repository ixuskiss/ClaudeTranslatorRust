//! Long-lived Claude Code CLI subprocess client (stream-json over stdin/stdout).
//!
//! Concurrency design: every public call takes `Mutex<Inner>`, so at most one turn (or
//! one initialize request) is in flight on the process at a time. Each spawned process
//! gets its own reader thread and its own channel: the thread parses stdout lines and
//! sends `Some(event)` per JSON line, then `None` at EOF. Callers wait with a deadline
//! (`recv_timeout`), so nothing blocks forever. Stdin is likewise owned by a per-process
//! writer thread: callers hand it a line and wait for its acknowledgement within the same
//! turn deadline, so a CLI that stops reading stdin cannot block a caller (or the lock).
//! Any failure tears the process down (writer channel closed so stdin drops, child
//! terminated, receiver dropped — neither thread is joined; the reader exits at stdout
//! EOF or on its next failed send, the writer when its channel closes or a write fails
//! after termination), so the next call starts
//! a fresh process with a fresh channel and can never see a stale turn's events.
//! Before reusing a live process, leftover events are drained so they can't be
//! mistaken for the next turn's.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use ct_core::config::ClaudeCliConfig;
use serde_json::Value;

use crate::models::{LIST_MODELS_REQUEST_ID, ModelInfo, parse_model_info};
use crate::provider::CliTransport;
use crate::spawner::{ChildHandle, CliSpawner, SpawnedCli};

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum CliError {
    #[error("Claude CLI timed out")]
    Timeout,
    #[error("Claude CLI process ended unexpectedly")]
    ProcessEnded,
    #[error("failed to start Claude CLI: {0}")]
    Spawn(String),
    #[error("Claude CLI I/O error: {0}")]
    Io(String),
    #[error("Claude CLI rejected the request: {0}")]
    Rejected(String),
}

/// Full argv for the resident CLI. `--tools ""` disables all tools; `--safe-mode` skips
/// CLAUDE.md/skills/plugins/hooks/MCP (pure overhead for one isolated prompt per turn)
/// without affecting auth.
pub fn build_command(config: &ClaudeCliConfig) -> Vec<String> {
    let mut cmd: Vec<String> = [
        config.cli_path.as_str(),
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--tools",
        "",
        "--safe-mode",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if let Some(model) = config.model.as_deref().filter(|m| !m.is_empty()) {
        cmd.extend(["--model".to_string(), model.to_string()]);
    }
    if let Some(effort) = config.effort.as_deref().filter(|e| !e.is_empty()) {
        cmd.extend(["--effort".to_string(), effort.to_string()]);
    }
    cmd
}

/// One stream-json user turn. Written by hand to keep the documented key order
/// (serde_json's default map sorts keys); the content is escaped by serde_json, which
/// leaves non-ASCII as raw UTF-8.
fn user_message(content: &str) -> String {
    format!(
        r#"{{"type":"user","message":{{"role":"user","content":{}}}}}"#,
        Value::from(content)
    )
}

fn initialize_request() -> String {
    format!(
        r#"{{"type":"control_request","request_id":{},"request":{{"subtype":"initialize"}}}}"#,
        Value::from(LIST_MODELS_REQUEST_ID)
    )
}

fn deadline_after(timeout: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(timeout)
        .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 3600))
}

fn event_type(event: &Value) -> Option<&str> {
    event.get("type").and_then(Value::as_str)
}

/// `None` unless `event` is the control_response to our initialize request.
fn parse_models_response(event: &Value) -> Option<Result<Vec<ModelInfo>, CliError>> {
    if event_type(event) != Some("control_response") {
        return None;
    }
    let response = event.get("response")?.as_object()?;
    if response.get("request_id").and_then(Value::as_str) != Some(LIST_MODELS_REQUEST_ID) {
        return None;
    }
    if response.get("subtype").and_then(Value::as_str) != Some("success") {
        return Some(Err(CliError::Rejected(
            "initialize request was rejected".into(),
        )));
    }
    let Some(raw_models) = response
        .get("response")
        .and_then(|p| p.get("models"))
        .and_then(Value::as_array)
    else {
        return Some(Err(CliError::Rejected(
            "initialize response has no model list".into(),
        )));
    };
    let models: Vec<ModelInfo> = raw_models.iter().filter_map(parse_model_info).collect();
    if models.is_empty() {
        return Some(Err(CliError::Rejected(
            "initialize response has no usable models".into(),
        )));
    }
    Some(Ok(models))
}

/// Reader thread body: one `Some(event)` per JSON stdout line, `None` at EOF/read error.
/// Blank, non-UTF-8 and non-JSON lines are skipped (never logged: they may hold content).
fn read_loop(stdout: Box<dyn Read + Send>, tx: Sender<Option<Value>>) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            tracing::warn!("Claude CLI emitted a non-UTF-8 stdout line, ignoring");
            continue;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(event) => {
                if tx.send(Some(event)).is_err() {
                    return; // client dropped this process
                }
            }
            Err(_) => tracing::warn!("Claude CLI emitted a non-JSON stdout line, ignoring"),
        }
    }
    let _ = tx.send(None);
}

/// One stdin line for the writer thread plus where to acknowledge it.
type WriteRequest = (Vec<u8>, Sender<Result<(), String>>);

/// Writer thread body: owns stdin, writes each requested line and acknowledges it.
/// Exits when the request channel closes (process torn down) or a write fails; a write
/// blocked on a hung CLI is released when the child is terminated (broken pipe).
fn write_loop(mut stdin: Box<dyn Write + Send>, requests: Receiver<WriteRequest>) {
    for (line, ack) in requests {
        let outcome = stdin
            .write_all(&line)
            .and_then(|()| stdin.flush())
            .map_err(|e| e.to_string());
        let failed = outcome.is_err();
        let _ = ack.send(outcome);
        if failed {
            return;
        }
    }
}

/// Logs a diagnostic (exit code and stderr tail length only, never content) and
/// returns `ProcessEnded`.
fn process_ended(child: &mut dyn ChildHandle) -> CliError {
    tracing::warn!(
        exit_code = ?child.exit_code(),
        stderr_tail_len = child.stderr_tail_len(),
        "claude CLI process ended"
    );
    CliError::ProcessEnded
}

struct Proc {
    writer: Sender<WriteRequest>,
    child: Box<dyn ChildHandle>,
    events: Receiver<Option<Value>>,
}

enum ModelsState {
    NotRequested,
    /// initialize sent on this process, response not seen yet (e.g. list_models timed out).
    Pending,
    Ready(Vec<ModelInfo>),
}

struct Inner {
    proc: Option<Proc>,
    turns_since_clear: u32,
    models: ModelsState,
}

impl Inner {
    fn close(&mut self) {
        if let Some(Proc {
            writer,
            mut child,
            events,
        }) = self.proc.take()
        {
            // Closing the request channel ends the writer thread, which drops stdin.
            drop(writer);
            child.terminate();
            drop(events);
        }
    }

    /// Reuses a live process (after draining leftovers) or spawns a fresh one.
    fn ensure_started(
        &mut self,
        args: &[String],
        cwd: Option<&Path>,
        spawner: &dyn CliSpawner,
    ) -> Result<(), CliError> {
        let alive = self.proc.as_mut().is_some_and(|p| p.child.is_running());
        if alive && self.drain() {
            return Ok(());
        }
        self.close();

        let SpawnedCli {
            stdin,
            stdout,
            mut child,
        } = spawner
            .spawn(args, cwd)
            .map_err(|e| CliError::Spawn(e.to_string()))?;
        let (writer, requests) = crossbeam_channel::unbounded();
        if let Err(e) = thread::Builder::new()
            .name("claude-cli-writer".into())
            .spawn(move || write_loop(stdin, requests))
        {
            child.terminate();
            return Err(CliError::Spawn(e.to_string()));
        }
        let (tx, events) = crossbeam_channel::unbounded();
        if let Err(e) = thread::Builder::new()
            .name("claude-cli-reader".into())
            .spawn(move || read_loop(stdout, tx))
        {
            drop(writer);
            child.terminate();
            return Err(CliError::Spawn(e.to_string()));
        }
        self.proc = Some(Proc {
            writer,
            child,
            events,
        });
        // A fresh process has no history to clear and has not answered initialize.
        self.turns_since_clear = 0;
        self.models = ModelsState::NotRequested;
        Ok(())
    }

    /// Consumes events left over from earlier turns. Returns false if the process ended.
    fn drain(&mut self) -> bool {
        loop {
            let next = match self.proc.as_ref() {
                Some(p) => p.events.try_recv(),
                None => return false,
            };
            match next {
                Ok(Some(event)) => self.observe(&event),
                Ok(None) | Err(TryRecvError::Disconnected) => return false,
                Err(TryRecvError::Empty) => return true,
            }
        }
    }

    /// Records a late initialize response that arrived while waiting for something else.
    fn observe(&mut self, event: &Value) {
        if !matches!(self.models, ModelsState::Pending) {
            return;
        }
        match parse_models_response(event) {
            Some(Ok(models)) => self.models = ModelsState::Ready(models),
            Some(Err(_)) => self.models = ModelsState::NotRequested,
            None => {}
        }
    }

    fn next_event(&mut self, deadline: Instant) -> Result<Value, CliError> {
        // Checked first so a CLI that keeps streaming non-result events (always one
        // queued) cannot keep a turn alive past its deadline.
        let now = Instant::now();
        if now >= deadline {
            return Err(CliError::Timeout);
        }
        let proc = self.proc.as_mut().ok_or(CliError::ProcessEnded)?;
        match proc.events.recv_timeout(deadline - now) {
            Ok(Some(event)) => Ok(event),
            Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                Err(process_ended(proc.child.as_mut()))
            }
            Err(RecvTimeoutError::Timeout) => Err(CliError::Timeout),
        }
    }

    /// Hands one line to the writer thread and waits for its acknowledgement until
    /// `deadline` (a CLI hung at boot stops reading stdin once the pipe buffer fills).
    fn send(&mut self, message: String, deadline: Instant) -> Result<(), CliError> {
        let proc = self.proc.as_mut().ok_or(CliError::ProcessEnded)?;
        let mut line = message.into_bytes();
        line.push(b'\n');
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        if proc.writer.send((line, ack_tx)).is_err() {
            return Err(process_ended(proc.child.as_mut()));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match ack_rx.recv_timeout(remaining) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(CliError::Io(e)),
            Err(RecvTimeoutError::Timeout) => Err(CliError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(process_ended(proc.child.as_mut())),
        }
    }

    /// Sends one user turn and waits for exactly its `result` event (everything else
    /// is skipped); the write and the wait share one deadline.
    fn turn(&mut self, content: &str, timeout: Duration) -> Result<Value, CliError> {
        let deadline = deadline_after(timeout);
        self.send(user_message(content), deadline)?;
        loop {
            let event = self.next_event(deadline)?;
            if event_type(&event) == Some("result") {
                return Ok(event);
            }
            self.observe(&event);
        }
    }

    fn run_turn(&mut self, config: &ClaudeCliConfig, prompt: &str) -> Result<Value, CliError> {
        let every = config.clear_every_n_turns;
        if every > 0 && self.turns_since_clear >= every {
            // /clear is a turn like any other: it ends with its own (empty) result event,
            // which must be consumed so it is never mistaken for the prompt's result.
            self.turn("/clear", config.timeout)?;
            self.turns_since_clear = 0;
        }
        let event = self.turn(prompt, config.timeout)?;
        self.turns_since_clear = self.turns_since_clear.saturating_add(1);
        Ok(event)
    }

    fn await_models(&mut self, deadline: Instant) -> Result<Vec<ModelInfo>, CliError> {
        loop {
            let event = self.next_event(deadline)?;
            match parse_models_response(&event) {
                Some(Ok(models)) => {
                    self.models = ModelsState::Ready(models.clone());
                    return Ok(models);
                }
                Some(Err(e)) => {
                    self.models = ModelsState::NotRequested;
                    return Err(e);
                }
                None => {}
            }
        }
    }
}

/// Owns one long-lived `claude -p --input-format stream-json ...` subprocess and
/// exchanges one prompt per turn over it, reusing the session (and prompt cache).
/// Never returns a partial or guessed result: on timeout/crash/pipe failure the call
/// fails, the process is torn down, and the next call restarts it.
pub struct ClaudeCliClient {
    config: ClaudeCliConfig,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    spawner: Arc<dyn CliSpawner>,
    inner: Mutex<Inner>,
}

impl ClaudeCliClient {
    pub fn new(
        config: ClaudeCliConfig,
        cwd: Option<PathBuf>,
        spawner: Arc<dyn CliSpawner>,
    ) -> Self {
        Self {
            args: build_command(&config),
            config,
            cwd,
            spawner,
            inner: Mutex::new(Inner {
                proc: None,
                turns_since_clear: 0,
                models: ModelsState::NotRequested,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn start(&self, inner: &mut Inner) -> Result<(), CliError> {
        inner.ensure_started(&self.args, self.cwd.as_deref(), self.spawner.as_ref())
    }

    /// Starts the process now (if not running) so the first real ask doesn't pay the
    /// boot cost. Never errors: a failure just means the next call tries again.
    pub fn warm_up(&self) {
        let mut inner = self.lock();
        if let Err(e) = self.start(&mut inner) {
            tracing::debug!(error = %e, "Claude CLI warm_up failed");
        }
    }

    /// The model list `/model` shows, via the stream-json `initialize` control request.
    /// Sent at most once per process and cached. A plain timeout keeps the process
    /// (no turn is in flight and ask skips the late response); any other error tears
    /// it down so the next call restarts cleanly.
    pub fn list_models(&self, timeout: Duration) -> Result<Vec<ModelInfo>, CliError> {
        let mut inner = self.lock();
        self.start(&mut inner)?;
        let deadline = deadline_after(timeout);
        match &inner.models {
            ModelsState::Ready(models) => return Ok(models.clone()),
            ModelsState::Pending => {}
            ModelsState::NotRequested => {
                // A write that does not complete in time (hung CLI) tears down too.
                if let Err(e) = inner.send(initialize_request(), deadline) {
                    tracing::warn!(error = %e, "Claude CLI list_models write failed; process reset");
                    inner.close();
                    return Err(e);
                }
                inner.models = ModelsState::Pending;
            }
        }
        match inner.await_models(deadline) {
            Ok(models) => Ok(models),
            Err(CliError::Timeout) => {
                tracing::warn!("Claude CLI list_models timed out; keeping process");
                Err(CliError::Timeout)
            }
            Err(e) => {
                tracing::warn!(error = %e, "Claude CLI list_models failed; process reset");
                inner.close();
                Err(e)
            }
        }
    }

    pub fn close(&self) {
        self.lock().close();
    }
}

impl CliTransport for ClaudeCliClient {
    /// Sends one prompt as a turn and returns its terminal `result` event.
    fn ask(&self, prompt: &str) -> Result<Value, CliError> {
        let mut inner = self.lock();
        self.start(&mut inner)?;
        let outcome = inner.run_turn(&self.config, prompt);
        if let Err(e) = &outcome {
            tracing::warn!(error = %e, "Claude CLI turn failed; process reset");
            inner.close();
        }
        outcome
    }
}

impl Drop for ClaudeCliClient {
    fn drop(&mut self) {
        self.inner
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::LIST_MODELS_REQUEST_ID;
    use crate::spawner::{ChildHandle, SpawnedCli};
    use serde_json::json;
    use std::io::{self, BufRead, BufReader, PipeWriter, Write};
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Instant;

    // ---------- fake process ----------

    /// The fake CLI's stdout.
    struct Out(PipeWriter);
    impl Out {
        fn json(&mut self, v: Value) {
            let _ = writeln!(self.0, "{v}");
        }
        fn raw(&mut self, bytes: &[u8]) {
            let _ = self.0.write_all(bytes);
        }
    }

    /// Called once per stdin line; writes response lines. Returning `false` ends the
    /// fake process (stdout EOF, stdin closed).
    type Script = Arc<dyn Fn(&str, &mut Out) -> bool + Send + Sync>;

    #[derive(Default)]
    struct Record {
        spawns: usize,
        args: Vec<Vec<String>>,
        cwds: Vec<Option<PathBuf>>,
        /// (spawn index, raw stdin line)
        lines: Vec<(usize, String)>,
        terminated: Vec<Arc<AtomicBool>>,
        stdin_closed: Vec<Arc<AtomicBool>>,
        running: Vec<Arc<AtomicBool>>,
    }

    struct FakeSpawner {
        scripts: Vec<Script>,
        fail_spawns: Vec<usize>,
        /// Spawn indexes whose stdin never accepts a write (until the child is terminated).
        blocked_stdin: Vec<usize>,
        rec: Arc<Mutex<Record>>,
    }

    /// Stdin of a CLI hung at boot: every write blocks until the child is terminated,
    /// then fails like a broken pipe.
    struct BlockedStdin(Arc<AtomicBool>);
    impl Write for BlockedStdin {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            while !self.0.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(5));
            }
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "terminated"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FakeChild {
        running: Arc<AtomicBool>,
        terminated: Arc<AtomicBool>,
    }

    impl ChildHandle for FakeChild {
        fn is_running(&mut self) -> bool {
            self.running.load(Ordering::SeqCst)
        }
        fn terminate(&mut self) {
            self.terminated.store(true, Ordering::SeqCst);
            self.running.store(false, Ordering::SeqCst);
        }
        fn stderr_tail_len(&self) -> usize {
            0
        }
        fn exit_code(&mut self) -> Option<i32> {
            None
        }
    }

    impl CliSpawner for FakeSpawner {
        fn spawn(&self, args: &[String], cwd: Option<&Path>) -> io::Result<SpawnedCli> {
            let idx = {
                let mut r = self.rec.lock().unwrap();
                let i = r.spawns;
                r.spawns += 1;
                r.args.push(args.to_vec());
                r.cwds.push(cwd.map(Path::to_path_buf));
                i
            };
            if self.fail_spawns.contains(&idx) {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no such cli"));
            }
            let (in_r, in_w) = io::pipe()?;
            let (out_r, out_w) = io::pipe()?;
            let running = Arc::new(AtomicBool::new(true));
            let terminated = Arc::new(AtomicBool::new(false));
            let stdin_closed = Arc::new(AtomicBool::new(false));
            {
                let mut r = self.rec.lock().unwrap();
                r.terminated.push(terminated.clone());
                r.stdin_closed.push(stdin_closed.clone());
                r.running.push(running.clone());
            }
            let script = self.scripts[idx.min(self.scripts.len() - 1)].clone();
            let rec = self.rec.clone();
            let alive = running.clone();
            thread::spawn(move || {
                let mut out = Out(out_w);
                let mut eof = true;
                for line in BufReader::new(in_r).lines() {
                    let Ok(line) = line else { break };
                    rec.lock().unwrap().lines.push((idx, line.clone()));
                    if !script(&line, &mut out) {
                        eof = false;
                        break;
                    }
                }
                if eof {
                    stdin_closed.store(true, Ordering::SeqCst);
                }
                alive.store(false, Ordering::SeqCst);
            });
            let stdin: Box<dyn Write + Send> = if self.blocked_stdin.contains(&idx) {
                Box::new(BlockedStdin(terminated.clone()))
            } else {
                Box::new(in_w)
            };
            Ok(SpawnedCli {
                stdin,
                stdout: Box::new(out_r),
                child: Box::new(FakeChild {
                    running,
                    terminated,
                }),
            })
        }
    }

    // ---------- helpers ----------

    fn init_event() -> Value {
        json!({"type": "system", "subtype": "init"})
    }

    /// Observed real behavior: /clear ends with an ordinary `result` event of its own.
    fn clear_result() -> Value {
        json!({"type": "result", "is_error": false, "result": "", "local_command": "clear"})
    }

    fn result(text: &str) -> Value {
        json!({"type": "result", "subtype": "success", "is_error": false, "result": text})
    }

    fn models_response(models: Value, request_id: &str) -> Value {
        json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": request_id, "response": {"models": models}}})
    }

    fn opus_raw() -> Value {
        json!({"value": "opus", "displayName": "Opus 5.5", "description": "d",
               "supportsEffort": true, "supportedEffortLevels": ["low", "max"]})
    }

    fn haiku_raw() -> Value {
        json!({"value": "haiku", "displayName": "Haiku 4.5", "description": "fast"})
    }

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    fn user_content(v: &Value) -> Option<String> {
        (v["type"] == "user").then(|| v["message"]["content"].as_str().unwrap().to_string())
    }

    /// Answers user turns with `r:<content>` (and /clear with its own empty result) and
    /// the initialize control request with `models_line`.
    fn responder(models_line: Value) -> Script {
        Arc::new(move |line, out| {
            let v = parse(line);
            if let Some(c) = user_content(&v) {
                if c == "/clear" {
                    out.json(clear_result());
                } else {
                    out.json(init_event());
                    out.json(result(&format!("r:{c}")));
                }
            } else if v["type"] == "control_request" {
                out.json(models_line.clone());
            }
            true
        })
    }

    fn echo() -> Script {
        responder(models_response(json!([opus_raw()]), LIST_MODELS_REQUEST_ID))
    }

    fn cfg(timeout_ms: u64, clear_every: u32) -> ClaudeCliConfig {
        ClaudeCliConfig {
            timeout: Duration::from_millis(timeout_ms),
            clear_every_n_turns: clear_every,
            ..ClaudeCliConfig::default()
        }
    }

    fn make(
        config: ClaudeCliConfig,
        scripts: Vec<Script>,
        fail_spawns: Vec<usize>,
    ) -> (ClaudeCliClient, Arc<Mutex<Record>>) {
        make_blocked(config, scripts, fail_spawns, vec![])
    }

    fn make_blocked(
        config: ClaudeCliConfig,
        scripts: Vec<Script>,
        fail_spawns: Vec<usize>,
        blocked_stdin: Vec<usize>,
    ) -> (ClaudeCliClient, Arc<Mutex<Record>>) {
        let rec = Arc::new(Mutex::new(Record::default()));
        let spawner = FakeSpawner {
            scripts,
            fail_spawns,
            blocked_stdin,
            rec: rec.clone(),
        };
        (ClaudeCliClient::new(config, None, Arc::new(spawner)), rec)
    }

    /// User-message contents (or `<initialize>`) received by spawn `idx`.
    fn sent(rec: &Arc<Mutex<Record>>, idx: usize) -> Vec<String> {
        rec.lock()
            .unwrap()
            .lines
            .iter()
            .filter(|(i, _)| *i == idx)
            .map(|(_, l)| {
                let v = parse(l);
                user_content(&v).unwrap_or_else(|| v["request"]["subtype"].to_string())
            })
            .collect()
    }

    fn spawns(rec: &Arc<Mutex<Record>>) -> usize {
        rec.lock().unwrap().spawns
    }

    fn terminated(rec: &Arc<Mutex<Record>>, idx: usize) -> bool {
        rec.lock().unwrap().terminated[idx].load(Ordering::SeqCst)
    }

    fn text(r: Result<Value, CliError>) -> String {
        r.unwrap()["result"].as_str().unwrap().to_string()
    }

    /// Waits (bounded) until `f` holds; returns whether it did.
    fn eventually(f: impl Fn() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if f() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    // ---------- command ----------

    #[test]
    fn build_command_exact() {
        let c = ClaudeCliConfig {
            cli_path: "claude".into(),
            model: Some("sonnet".into()),
            effort: Some("low".into()),
            ..ClaudeCliConfig::default()
        };
        assert_eq!(
            build_command(&c),
            [
                "claude",
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--tools",
                "",
                "--safe-mode",
                "--model",
                "sonnet",
                "--effort",
                "low"
            ]
        );
    }

    #[test]
    fn build_command_omits_model_and_effort_when_unset() {
        let cmd = build_command(&ClaudeCliConfig::default());
        assert!(!cmd.contains(&"--model".to_string()));
        assert!(!cmd.contains(&"--effort".to_string()));
        assert!(cmd.contains(&"--safe-mode".to_string()));
        assert_eq!(cmd.last().map(String::as_str), Some("--safe-mode"));
    }

    #[test]
    fn build_command_passes_configured_model_and_effort() {
        let c = ClaudeCliConfig {
            model: Some("opus".into()),
            ..ClaudeCliConfig::default()
        };
        let cmd = build_command(&c);
        let i = cmd.iter().position(|a| a == "--model").unwrap();
        assert_eq!(cmd[i + 1], "opus");
        assert!(!cmd.contains(&"--effort".to_string()));
        let c = ClaudeCliConfig {
            effort: Some("high".into()),
            ..ClaudeCliConfig::default()
        };
        let cmd = build_command(&c);
        let i = cmd.iter().position(|a| a == "--effort").unwrap();
        assert_eq!(cmd[i + 1], "high");
    }

    #[test]
    fn spawner_receives_command_and_cwd() {
        let rec = Arc::new(Mutex::new(Record::default()));
        let spawner = FakeSpawner {
            scripts: vec![echo()],
            fail_spawns: vec![],
            blocked_stdin: vec![],
            rec: rec.clone(),
        };
        let config = ClaudeCliConfig {
            model: Some("opus".into()),
            ..cfg(2000, 20)
        };
        let client = ClaudeCliClient::new(
            config.clone(),
            Some(PathBuf::from("C:/neutral")),
            Arc::new(spawner),
        );
        client.ask("hi").unwrap();
        let r = rec.lock().unwrap();
        assert_eq!(r.args, vec![build_command(&config)]);
        assert_eq!(r.cwds, vec![Some(PathBuf::from("C:/neutral"))]);
    }

    // ---------- ask ----------

    #[test]
    fn user_message_is_stream_json_with_utf8() {
        let (client, rec) = make(cfg(2000, 20), vec![echo()], vec![]);
        assert_eq!(text(client.ask("안녕")), "r:안녕");
        let line = rec.lock().unwrap().lines[0].1.clone();
        assert_eq!(
            line,
            r#"{"type":"user","message":{"role":"user","content":"안녕"}}"#
        );
    }

    #[test]
    fn prompt_with_quotes_and_newlines_is_one_escaped_line() {
        let (client, rec) = make(cfg(2000, 20), vec![echo()], vec![]);
        let prompt = "say \"hi\"\nline2\\ end";
        assert_eq!(text(client.ask(prompt)), format!("r:{prompt}"));
        assert_eq!(rec.lock().unwrap().lines.len(), 1);
    }

    #[test]
    fn ask_returns_result_event() {
        let (client, _rec) = make(cfg(2000, 20), vec![echo()], vec![]);
        let ev = client.ask("hello").unwrap();
        assert_eq!(ev["type"], "result");
        assert_eq!(ev["is_error"], false);
        assert_eq!(ev["result"], "r:hello");
    }

    #[test]
    fn ask_ignores_events_before_result() {
        let script: Script = Arc::new(|_, out| {
            out.json(init_event());
            out.json(json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "..."}]}}));
            out.json(json!({"type": "rate_limit_event", "rate_limit_info": {}}));
            out.json(result("done"));
            true
        });
        let (client, _rec) = make(cfg(2000, 20), vec![script], vec![]);
        assert_eq!(text(client.ask("hello")), "done");
    }

    #[test]
    fn non_json_lines_are_ignored() {
        let script: Script = Arc::new(|_, out| {
            out.raw(b"not json at all\n");
            out.raw(b"\n   \n");
            out.raw(b"\xff\xfe broken utf8\n");
            out.json(result("ok"));
            true
        });
        let (client, _rec) = make(cfg(2000, 20), vec![script], vec![]);
        assert_eq!(text(client.ask("hello")), "ok");
    }

    #[test]
    fn ask_timeout_returns_timeout_terminates_and_never_returns_late_result() {
        // First process answers only after 600ms; the configured timeout is 200ms.
        let slow: Script = Arc::new(|line, out| {
            thread::sleep(Duration::from_millis(600));
            let c = user_content(&parse(line)).unwrap();
            out.json(result(&format!("r:{c}")));
            true
        });
        let (client, rec) = make(cfg(200, 20), vec![slow, echo()], vec![]);
        let start = Instant::now();
        assert_eq!(client.ask("t1"), Err(CliError::Timeout));
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        assert!(terminated(&rec, 0));
        // The next call restarts and gets its own turn's result, never t1's late one.
        assert_eq!(text(client.ask("t2")), "r:t2");
        assert_eq!(spawns(&rec), 2);
    }

    #[test]
    fn blocked_stdin_write_times_out_and_next_call_restarts() {
        let (client, rec) = make_blocked(cfg(200, 20), vec![echo()], vec![], vec![0]);
        let start = Instant::now();
        assert_eq!(client.ask("t1"), Err(CliError::Timeout));
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        assert!(terminated(&rec, 0));
        assert_eq!(text(client.ask("t2")), "r:t2");
        assert_eq!(spawns(&rec), 2);
    }

    #[test]
    fn blocked_stdin_initialize_times_out_and_tears_down() {
        let (client, rec) = make_blocked(cfg(5000, 20), vec![echo()], vec![], vec![0]);
        let start = Instant::now();
        assert_eq!(
            client.list_models(Duration::from_millis(200)),
            Err(CliError::Timeout)
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(terminated(&rec, 0));
        assert_eq!(text(client.ask("hi")), "r:hi");
        assert_eq!(spawns(&rec), 2);
    }

    #[test]
    fn endless_non_result_events_time_out() {
        // The CLI streams non-result events as fast as it can and never ends the turn.
        let flood: Script = Arc::new(|_, out| {
            let line = format!("{}\n", json!({"type": "assistant", "message": {}}));
            while out.0.write_all(line.as_bytes()).is_ok() {}
            false
        });
        let (client, rec) = make(cfg(200, 20), vec![flood, echo()], vec![]);
        let start = Instant::now();
        assert_eq!(client.ask("t1"), Err(CliError::Timeout));
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        assert!(terminated(&rec, 0));
        assert_eq!(text(client.ask("t2")), "r:t2");
    }

    #[test]
    fn next_event_after_deadline_is_timeout_even_with_queued_events() {
        let (tx, events) = crossbeam_channel::unbounded();
        tx.send(Some(init_event())).unwrap();
        let (writer, _writer_rx) = crossbeam_channel::unbounded();
        let mut inner = Inner {
            proc: Some(Proc {
                writer,
                child: Box::new(FakeChild {
                    running: Arc::new(AtomicBool::new(true)),
                    terminated: Arc::new(AtomicBool::new(false)),
                }),
                events,
            }),
            turns_since_clear: 0,
            models: ModelsState::NotRequested,
        };
        let past = Instant::now();
        thread::sleep(Duration::from_millis(5));
        assert_eq!(inner.next_event(past), Err(CliError::Timeout));
    }

    #[test]
    fn eof_is_process_ended_and_next_call_restarts() {
        let crashing: Script = Arc::new(|_, out| {
            out.json(init_event());
            false
        });
        let (client, rec) = make(cfg(2000, 20), vec![crashing, echo()], vec![]);
        assert_eq!(client.ask("first"), Err(CliError::ProcessEnded));
        assert!(terminated(&rec, 0));
        assert_eq!(text(client.ask("second")), "r:second");
        assert_eq!(spawns(&rec), 2);
    }

    #[test]
    fn dead_process_between_calls_is_restarted_before_sending() {
        // Process answers t1 and then exits on its own (stdout EOF) before t2.
        let once: Script = Arc::new(|line, out| {
            let c = user_content(&parse(line)).unwrap();
            out.json(result(&format!("r:{c}")));
            false
        });
        let (client, rec) = make(cfg(2000, 20), vec![once, echo()], vec![]);
        assert_eq!(text(client.ask("t1")), "r:t1");
        let running = rec.lock().unwrap().running[0].clone();
        assert!(eventually(|| !running.load(Ordering::SeqCst)));
        assert_eq!(text(client.ask("t2")), "r:t2");
        assert_eq!(spawns(&rec), 2);
        assert_eq!(sent(&rec, 1), vec!["t2"]);
    }

    #[test]
    fn spawn_failure_is_error_and_next_call_retries() {
        let (client, rec) = make(cfg(2000, 20), vec![echo()], vec![0]);
        assert!(matches!(client.ask("t1"), Err(CliError::Spawn(_))));
        assert_eq!(spawns(&rec), 1);
        assert_eq!(text(client.ask("t2")), "r:t2");
        assert_eq!(spawns(&rec), 2);
    }

    // ---------- periodic /clear ----------

    #[test]
    fn clear_not_sent_before_reaching_n_turns() {
        let (client, rec) = make(cfg(2000, 3), vec![echo()], vec![]);
        for t in ["t1", "t2", "t3"] {
            client.ask(t).unwrap();
        }
        assert_eq!(sent(&rec, 0), ["t1", "t2", "t3"]);
    }

    #[test]
    fn clear_sent_every_n_turns_and_its_result_consumed() {
        let (client, rec) = make(cfg(2000, 3), vec![echo()], vec![]);
        let mut results = Vec::new();
        for t in ["t1", "t2", "t3", "t4", "t5", "t6", "t7"] {
            results.push(text(client.ask(t)));
        }
        assert_eq!(
            sent(&rec, 0),
            ["t1", "t2", "t3", "/clear", "t4", "t5", "t6", "/clear", "t7"]
        );
        // /clear's own (empty) result is never returned as a turn's result.
        assert_eq!(
            results,
            ["r:t1", "r:t2", "r:t3", "r:t4", "r:t5", "r:t6", "r:t7"]
        );
    }

    #[test]
    fn zero_disables_clear() {
        let (client, rec) = make(cfg(2000, 0), vec![echo()], vec![]);
        for _ in 0..5 {
            client.ask("translate this").unwrap();
        }
        assert_eq!(sent(&rec, 0), vec!["translate this"; 5]);
    }

    #[test]
    fn restart_resets_clear_counter_and_model_cache() {
        let (client, rec) = make(cfg(2000, 2), vec![echo()], vec![]);
        client.list_models(Duration::from_secs(2)).unwrap();
        client.ask("t1").unwrap();
        client.ask("t2").unwrap();
        client.close();
        client.ask("t3").unwrap();
        client.list_models(Duration::from_secs(2)).unwrap();
        assert_eq!(sent(&rec, 0), [r#""initialize""#, "t1", "t2"]);
        // Fresh process: no /clear before t3, and initialize is sent again.
        assert_eq!(sent(&rec, 1), ["t3", r#""initialize""#]);
    }

    // ---------- lifecycle ----------

    #[test]
    fn close_terminates_closes_stdin_and_is_idempotent() {
        let (client, rec) = make(cfg(2000, 20), vec![echo()], vec![]);
        client.ask("hello").unwrap();
        client.close();
        client.close();
        assert!(terminated(&rec, 0));
        let stdin_closed = rec.lock().unwrap().stdin_closed[0].clone();
        assert!(eventually(|| stdin_closed.load(Ordering::SeqCst)));
    }

    #[test]
    fn drop_terminates_process() {
        let (client, rec) = make(cfg(2000, 20), vec![echo()], vec![]);
        client.ask("hello").unwrap();
        drop(client);
        assert!(terminated(&rec, 0));
    }

    #[test]
    fn warm_up_starts_process_once() {
        let (client, rec) = make(cfg(2000, 20), vec![echo()], vec![]);
        client.warm_up();
        assert_eq!(spawns(&rec), 1);
        client.warm_up();
        assert_eq!(text(client.ask("hello")), "r:hello");
        assert_eq!(spawns(&rec), 1);
    }

    #[test]
    fn warm_up_never_errors_on_spawn_failure() {
        let (client, rec) = make(cfg(2000, 20), vec![echo()], vec![0]);
        client.warm_up();
        assert_eq!(spawns(&rec), 1);
        assert_eq!(text(client.ask("hello")), "r:hello");
        assert_eq!(spawns(&rec), 2);
    }

    #[test]
    fn concurrent_asks_are_serialized() {
        let (client, rec) = make(cfg(2000, 20), vec![echo()], vec![]);
        let client = Arc::new(client);
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let c = client.clone();
                thread::spawn(move || (i, c.ask(&format!("p{i}"))))
            })
            .collect();
        for h in handles {
            let (i, r) = h.join().unwrap();
            assert_eq!(text(r), format!("r:p{i}"));
        }
        assert_eq!(spawns(&rec), 1);
        assert_eq!(sent(&rec, 0).len(), 8);
    }

    // ---------- list_models ----------

    #[test]
    fn list_models_sends_initialize_and_parses_models() {
        let script = responder(models_response(
            json!([opus_raw(), haiku_raw()]),
            LIST_MODELS_REQUEST_ID,
        ));
        let (client, rec) = make(cfg(2000, 20), vec![script], vec![]);
        let models = client.list_models(Duration::from_secs(2)).unwrap();
        let line = parse(&rec.lock().unwrap().lines[0].1);
        assert_eq!(
            line,
            json!({"type": "control_request", "request_id": LIST_MODELS_REQUEST_ID,
                   "request": {"subtype": "initialize"}})
        );
        assert_eq!(
            models,
            vec![
                ModelInfo {
                    value: "opus".into(),
                    display_name: "Opus 5.5".into(),
                    description: "d".into(),
                    effort_levels: vec!["low".into(), "max".into()],
                },
                ModelInfo {
                    value: "haiku".into(),
                    display_name: "Haiku 4.5".into(),
                    description: "fast".into(),
                    effort_levels: vec![],
                },
            ]
        );
    }

    #[test]
    fn list_models_ignores_unrelated_events_and_other_request_ids() {
        let script: Script = Arc::new(|_, out| {
            out.json(json!({"type": "system", "subtype": "ui_invalidate"}));
            out.json(models_response(json!([haiku_raw()]), "other"));
            out.json(models_response(json!([opus_raw()]), LIST_MODELS_REQUEST_ID));
            true
        });
        let (client, _rec) = make(cfg(2000, 20), vec![script], vec![]);
        let models = client.list_models(Duration::from_secs(2)).unwrap();
        let values: Vec<_> = models.iter().map(|m| m.value.as_str()).collect();
        assert_eq!(values, ["opus"]);
    }

    #[test]
    fn list_models_second_call_reuses_result_without_resending() {
        let (client, rec) = make(cfg(2000, 20), vec![echo()], vec![]);
        let first = client.list_models(Duration::from_secs(2)).unwrap();
        let second = client.list_models(Duration::from_secs(2)).unwrap();
        assert_eq!(first, second);
        assert_eq!(rec.lock().unwrap().lines.len(), 1);
    }

    #[test]
    fn list_models_skips_malformed_items_keeps_valid() {
        let raw = json!([{"displayName": "no value"}, "not a dict", opus_raw()]);
        let script = responder(models_response(raw, LIST_MODELS_REQUEST_ID));
        let (client, _rec) = make(cfg(2000, 20), vec![script], vec![]);
        let models = client.list_models(Duration::from_secs(2)).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].value, "opus");
    }

    #[test]
    fn list_models_rejects_when_no_valid_models_and_tears_down() {
        let mut missing = models_response(json!([]), LIST_MODELS_REQUEST_ID);
        missing["response"]["response"]
            .as_object_mut()
            .unwrap()
            .remove("models");
        for line in [
            models_response(json!([]), LIST_MODELS_REQUEST_ID),
            models_response(json!("oops"), LIST_MODELS_REQUEST_ID),
            models_response(json!([{"displayName": "x"}]), LIST_MODELS_REQUEST_ID),
            missing,
        ] {
            let (client, rec) = make(cfg(2000, 20), vec![responder(line.clone())], vec![]);
            let r = client.list_models(Duration::from_secs(2));
            assert!(matches!(r, Err(CliError::Rejected(_))), "{line}: {r:?}");
            assert!(terminated(&rec, 0), "{line}");
        }
    }

    #[test]
    fn list_models_error_response_is_rejected_and_next_call_restarts() {
        let error = json!({"type": "control_response", "response": {
            "subtype": "error", "request_id": LIST_MODELS_REQUEST_ID, "error": "nope"}});
        let (client, rec) = make(cfg(2000, 20), vec![responder(error), echo()], vec![]);
        assert!(matches!(
            client.list_models(Duration::from_secs(2)),
            Err(CliError::Rejected(_))
        ));
        assert!(terminated(&rec, 0));
        assert_eq!(text(client.ask("hi")), "r:hi");
        assert_eq!(spawns(&rec), 2);
    }

    #[test]
    fn list_models_process_end_tears_down_so_next_ask_restarts() {
        let dying: Script = Arc::new(|_, _| false);
        let (client, rec) = make(cfg(2000, 20), vec![dying, echo()], vec![]);
        assert_eq!(
            client.list_models(Duration::from_secs(2)),
            Err(CliError::ProcessEnded)
        );
        assert!(terminated(&rec, 0));
        assert_eq!(text(client.ask("hi")), "r:hi");
        assert_eq!(spawns(&rec), 2);
    }

    #[test]
    fn list_models_spawn_failure_is_spawn_error() {
        let (client, _rec) = make(cfg(2000, 20), vec![echo()], vec![0]);
        assert!(matches!(
            client.list_models(Duration::from_secs(2)),
            Err(CliError::Spawn(_))
        ));
    }

    #[test]
    fn list_models_timeout_keeps_process_and_ask_skips_late_response() {
        // initialize is answered only after 400ms; list_models waits 100ms.
        let script: Script = Arc::new(|line, out| {
            let v = parse(line);
            if v["type"] == "control_request" {
                thread::sleep(Duration::from_millis(400));
                out.json(models_response(json!([opus_raw()]), LIST_MODELS_REQUEST_ID));
            } else if let Some(c) = user_content(&v) {
                out.json(result(&format!("r:{c}")));
            }
            true
        });
        let (client, rec) = make(cfg(5000, 20), vec![script], vec![]);
        assert_eq!(
            client.list_models(Duration::from_millis(100)),
            Err(CliError::Timeout)
        );
        assert!(!terminated(&rec, 0));
        assert_eq!(text(client.ask("hi")), "r:hi");
        assert_eq!(spawns(&rec), 1);
        assert!(!terminated(&rec, 0));
        // The late response was seen while skipping; list_models now answers from
        // it without sending a second initialize on this process.
        let models = client.list_models(Duration::from_secs(2)).unwrap();
        assert_eq!(models[0].value, "opus");
        assert_eq!(sent(&rec, 0), [r#""initialize""#, "hi"]);
    }
}
