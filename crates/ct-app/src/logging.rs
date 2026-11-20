//! Logging setup (v5 §5.1 step 3) and the panic hook that appends to `logs/crash.log`.
//!
//! Release builds write one file per day under `<data_dir>/logs/`; debug builds log to the
//! console. The level comes from `LOG_LEVEL` (default INFO). Nothing here ever records
//! captured or translated text; panic messages are redacted before they are written.

use std::io::Write;
use std::path::{Path, PathBuf};

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::Registry;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;

const LOG_FILE_PREFIX: &str = "claudetranslator";
const LOG_FILE_SUFFIX: &str = "log";
/// Daily files kept before the oldest is deleted.
const MAX_LOG_FILES: usize = 14;
const CRASH_LOG: &str = "crash.log";
const MAX_PANIC_MESSAGE_CHARS: usize = 300;
const REDACTED: &str = "<redacted>";

/// `LOG_LEVEL` value to a filter. Accepts the v5 (Python logging) names and the tracing
/// names, case-insensitively; anything else is INFO.
pub fn parse_level(value: Option<&str>) -> LevelFilter {
    match value.map(|v| v.trim().to_ascii_uppercase()).as_deref() {
        Some("TRACE") => LevelFilter::TRACE,
        Some("DEBUG") => LevelFilter::DEBUG,
        Some("WARN" | "WARNING") => LevelFilter::WARN,
        Some("ERROR" | "CRITICAL") => LevelFilter::ERROR,
        Some("OFF") => LevelFilter::OFF,
        _ => LevelFilter::INFO,
    }
}

/// Replaces every quoted span (backticks, single or double quotes) with `<redacted>`:
/// std panics such as string slicing embed the user's text in quotes. An unclosed quote
/// is kept as is. The result is capped at 300 characters.
pub fn redact_panic_message(message: &str) -> String {
    let chars: Vec<char> = message.chars().collect();
    let mut out = String::with_capacity(message.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '`' | '\'' | '"')
            && let Some(len) = chars[i + 1..].iter().position(|&x| x == c)
        {
            out.push_str(REDACTED);
            i += len + 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out.chars().take(MAX_PANIC_MESSAGE_CHARS).collect()
}

/// One `crash.log` line: the panic location and the redacted message, nothing else.
pub fn crash_entry(location: &str, message: &str) -> String {
    format!("panic at {location}: {}\n", redact_panic_message(message))
}

/// Applies a new level after start-up (e.g. once `.env` has been read).
pub struct LogHandle {
    reload: Option<reload::Handle<LevelFilter, Registry>>,
}

impl LogHandle {
    /// Re-reads `LOG_LEVEL` from the process environment.
    pub fn apply_env_level(&self) {
        let level = parse_level(std::env::var("LOG_LEVEL").ok().as_deref());
        if let Some(handle) = &self.reload
            && let Err(e) = handle.reload(level)
        {
            tracing::warn!(error = %e, "could not apply LOG_LEVEL");
        }
    }
}

fn file_writer(logs_dir: &Path) -> Option<BoxMakeWriter> {
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .filename_suffix(LOG_FILE_SUFFIX)
        .max_log_files(MAX_LOG_FILES)
        .build(logs_dir)
        .ok()?;
    Some(BoxMakeWriter::new(appender))
}

/// Installs the global subscriber and the panic hook. Release: daily files in
/// `<data_dir>/logs/` (console if the folder cannot be used); debug: console.
pub fn init(data_dir: &Path) -> LogHandle {
    let logs_dir = data_dir.join("logs");
    let _ = std::fs::create_dir_all(&logs_dir);
    install_panic_hook(logs_dir.join(CRASH_LOG));

    let level = parse_level(std::env::var("LOG_LEVEL").ok().as_deref());
    let (filter, handle) = reload::Layer::new(level);
    let file = if cfg!(debug_assertions) {
        None
    } else {
        file_writer(&logs_dir)
    };
    let to_file = file.is_some();
    let writer = file.unwrap_or_else(|| BoxMakeWriter::new(std::io::stderr));
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(!to_file);
    match tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .try_init()
    {
        Ok(()) => LogHandle {
            reload: Some(handle),
        },
        Err(_) => LogHandle { reload: None },
    }
}

fn install_panic_hook(crash_log: PathBuf) {
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_string());
        let entry = crash_entry(&location, &message);
        tracing::error!("{}", entry.trim_end());
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crash_log)
        {
            let _ = f.write_all(entry.as_bytes());
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_defaults_to_info() {
        assert_eq!(parse_level(None), LevelFilter::INFO);
        assert_eq!(parse_level(Some("")), LevelFilter::INFO);
        assert_eq!(parse_level(Some("verbose")), LevelFilter::INFO);
    }

    #[test]
    fn level_accepts_v5_names_case_insensitively() {
        assert_eq!(parse_level(Some("DEBUG")), LevelFilter::DEBUG);
        assert_eq!(parse_level(Some("debug")), LevelFilter::DEBUG);
        assert_eq!(parse_level(Some(" info ")), LevelFilter::INFO);
        assert_eq!(parse_level(Some("WARNING")), LevelFilter::WARN);
        assert_eq!(parse_level(Some("warn")), LevelFilter::WARN);
        assert_eq!(parse_level(Some("ERROR")), LevelFilter::ERROR);
        assert_eq!(parse_level(Some("CRITICAL")), LevelFilter::ERROR);
        assert_eq!(parse_level(Some("TRACE")), LevelFilter::TRACE);
        assert_eq!(parse_level(Some("OFF")), LevelFilter::OFF);
    }

    #[test]
    fn panic_message_hides_quoted_text() {
        // The std string-slicing panic embeds the string itself in backticks.
        let m =
            "byte index 4 is not a char boundary; it is inside '안' (bytes 3..6) of `비밀 문장`";
        let r = redact_panic_message(m);
        assert!(!r.contains("비밀"), "{r}");
        assert!(!r.contains('안'), "{r}");
        assert_eq!(
            r,
            "byte index 4 is not a char boundary; it is inside <redacted> (bytes 3..6) of <redacted>"
        );
        assert_eq!(
            redact_panic_message("called `Option::unwrap()` on a `None` value"),
            "called <redacted> on a <redacted> value"
        );
        assert_eq!(
            redact_panic_message("index out of bounds"),
            "index out of bounds"
        );
        assert_eq!(redact_panic_message("say \"hi\" now"), "say <redacted> now");
    }

    #[test]
    fn panic_message_keeps_unclosed_quote_and_is_capped() {
        assert_eq!(redact_panic_message("can't open"), "can't open");
        let long = "x".repeat(1000);
        assert_eq!(redact_panic_message(&long).chars().count(), 300);
    }

    #[test]
    fn crash_entry_is_location_and_message_only() {
        assert_eq!(
            crash_entry("src/flows.rs:10:5", "boom `secret`"),
            "panic at src/flows.rs:10:5: boom <redacted>\n"
        );
    }
}
