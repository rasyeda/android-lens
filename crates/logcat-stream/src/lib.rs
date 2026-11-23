use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{broadcast, Notify};

pub const DEFAULT_RING_CAPACITY: usize = 5_000;
const MAX_CRASH_GROUPS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Verbose,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    Silent,
    Unknown,
}

impl LogLevel {
    pub fn from_char(c: char) -> Self {
        match c {
            'V' => Self::Verbose,
            'D' => Self::Debug,
            'I' => Self::Info,
            'W' => Self::Warn,
            'E' => Self::Error,
            'F' | 'A' => Self::Fatal,
            'S' => Self::Silent,
            _ => Self::Unknown,
        }
    }

    pub fn as_letter(&self) -> char {
        match self {
            Self::Verbose => 'V',
            Self::Debug => 'D',
            Self::Info => 'I',
            Self::Warn => 'W',
            Self::Error => 'E',
            Self::Fatal => 'F',
            Self::Silent => 'S',
            Self::Unknown => '?',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: u64,
    pub timestamp: String,
    pub pid: u32,
    pub tid: u32,
    pub level: LogLevel,
    pub tag: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashGroup {
    pub id: u64,
    pub timestamp: String,
    pub pid: u32,
    pub process: Option<String>,
    pub summary: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogFilter {
    pub min_level: Option<LogLevel>,
    pub tag: Option<String>,
    pub query: Option<String>,
    pub limit: Option<usize>,
}

impl LogFilter {
    pub fn matches(&self, entry: &LogEntry) -> bool {
        if let Some(min) = self.min_level {
            if entry.level < min {
                return false;
            }
        }
        if let Some(ref tag) = self.tag {
            if !tag.is_empty()
                && !entry
                    .tag
                    .to_ascii_lowercase()
                    .contains(&tag.to_ascii_lowercase())
            {
                return false;
            }
        }
        if let Some(ref q) = self.query {
            if !q.is_empty() {
                let needle = q.to_ascii_lowercase();
                if !entry.message.to_ascii_lowercase().contains(&needle)
                    && !entry.tag.to_ascii_lowercase().contains(&needle)
                {
                    return false;
                }
            }
        }
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LogcatEvent {
    Entry { entry: LogEntry },
    Crash { crash: CrashGroup },
    Attached { pid: u32 },
    WaitingForProcess { package: String },
    Error { message: String },
}

/// Parses a single `adb logcat -v threadtime` line:
/// `11-23 21:14:05.123  6083  6083 D MainActivity: hello world`
pub fn parse_threadtime_line(line: &str, seq: u64) -> Option<LogEntry> {
    let line = line.trim_end_matches(&['\r', '\n'][..]);
    if line.is_empty() || line.starts_with("---------") {
        return None;
    }

    let mut parts = line.split_whitespace();
    let date = parts.next()?;
    let time = parts.next()?;
    let pid_str = parts.next()?;
    let tid_str = parts.next()?;
    let level_str = parts.next()?;

    let pid: u32 = pid_str.parse().ok()?;
    let tid: u32 = tid_str.parse().ok()?;
    let level_ch = level_str.chars().next()?;
    if level_str.len() != 1 {
        return None;
    }

    // Find the remainder of the line starting after the level token so we preserve
    // exact spacing in both tag and message.
    let level_offset = (level_str.as_ptr() as usize) - (line.as_ptr() as usize);
    let after_level = line.get(level_offset + 1..)?.trim_start();

    let (tag, message) = if let Some((raw_tag, raw_msg)) = after_level.split_once(": ") {
        (raw_tag.trim().to_string(), raw_msg.to_string())
    } else if let Some((raw_tag, raw_msg)) = after_level.split_once(':') {
        (raw_tag.trim().to_string(), raw_msg.trim_start().to_string())
    } else {
        (String::new(), after_level.to_string())
    };

    Some(LogEntry {
        seq,
        timestamp: format!("{date} {time}"),
        pid,
        tid,
        level: LogLevel::from_char(level_ch),
        tag,
        message,
    })
}

/// Stateful crash stack detector that groups `AndroidRuntime` fatal exception blocks
/// and contiguous stack-trace lines into structured `CrashGroup`s.
#[derive(Default)]
pub struct CrashDetector {
    next_id: u64,
    active: Option<CrashGroup>,
    active_tag: String,
    active_tid: u32,
}

impl CrashDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a new `LogEntry`. Returns a completed `CrashGroup` when a previous crash block
    /// finishes (e.g. a non-crash log line arrives), or updates the in-progress crash block.
    pub fn push(&mut self, entry: &LogEntry) -> Option<CrashGroup> {
        let is_fatal_start = entry.tag == "AndroidRuntime"
            && entry.level >= LogLevel::Error
            && entry.message.starts_with("FATAL EXCEPTION:");

        if is_fatal_start {
            let finished = self.active.take();
            self.next_id += 1;
            self.active_tag = entry.tag.clone();
            self.active_tid = entry.tid;
            self.active = Some(CrashGroup {
                id: self.next_id,
                timestamp: entry.timestamp.clone(),
                pid: entry.pid,
                process: None,
                summary: entry.message.clone(),
                lines: vec![entry.message.clone()],
            });
            return finished;
        }

        if let Some(ref mut current) = self.active {
            let same_stream = entry.tag == self.active_tag
                && entry.pid == current.pid
                && entry.tid == self.active_tid
                && entry.level >= LogLevel::Error;
            if same_stream && is_crash_continuation(&entry.message) {
                if let Some(proc_info) = entry.message.strip_prefix("Process: ") {
                    let proc_name = proc_info
                        .split(',')
                        .next()
                        .unwrap_or(proc_info)
                        .trim()
                        .to_string();
                    current.process = Some(proc_name);
                } else if current.summary.starts_with("FATAL EXCEPTION:")
                    && is_exception_header(&entry.message)
                {
                    current.summary = entry.message.clone();
                }
                current.lines.push(entry.message.clone());
                return None;
            }
            // Different stream or non-continuation line -> finalize active crash
            return self.active.take();
        }

        // Also detect non-AndroidRuntime uncaught exception blocks (e.g. System.err / custom tag)
        if entry.level >= LogLevel::Error && is_exception_header(&entry.message) {
            self.next_id += 1;
            self.active_tag = entry.tag.clone();
            self.active_tid = entry.tid;
            self.active = Some(CrashGroup {
                id: self.next_id,
                timestamp: entry.timestamp.clone(),
                pid: entry.pid,
                process: None,
                summary: entry.message.clone(),
                lines: vec![entry.message.clone()],
            });
        }

        None
    }

    /// Flushes any in-progress crash group (only if it has >1 line or is a FATAL EXCEPTION).
    pub fn flush(&mut self) -> Option<CrashGroup> {
        let group = self.active.take()?;
        if group.lines.len() > 1
            || group
                .lines
                .first()
                .map(|l| l.starts_with("FATAL EXCEPTION:"))
                .unwrap_or(false)
        {
            Some(group)
        } else {
            None
        }
    }
}

fn is_crash_continuation(msg: &str) -> bool {
    let trimmed = msg.trim_start();
    trimmed.starts_with("at ")
        || trimmed.starts_with("Caused by:")
        || trimmed.starts_with("Suppressed:")
        || trimmed.starts_with("... ")
        || trimmed.starts_with("Process: ")
        || trimmed.starts_with("PID: ")
        || is_exception_header(trimmed)
}

fn is_exception_header(msg: &str) -> bool {
    let first_token = msg.split(&[':', ' '][..]).next().unwrap_or("");
    first_token.contains('.')
        && (first_token.ends_with("Exception")
            || first_token.ends_with("Error")
            || first_token.ends_with("Throwable"))
}

struct SharedBuffers {
    entries: VecDeque<LogEntry>,
    crashes: VecDeque<CrashGroup>,
    capacity: usize,
    current_pid: Option<u32>,
}

/// Package-scoped `adb logcat --pid` stream backed by an in-memory ring buffer
/// with automatic reattach when the target Android process restarts.
pub struct LogcatStream {
    serial: String,
    package: String,
    buffers: Arc<RwLock<SharedBuffers>>,
    events_tx: broadcast::Sender<LogcatEvent>,
    stopped: Arc<AtomicBool>,
    stop_notify: Arc<Notify>,
}

impl LogcatStream {
    pub fn start(serial: String, package: String, capacity: usize) -> Self {
        let capacity = capacity.max(100);
        let buffers = Arc::new(RwLock::new(SharedBuffers {
            entries: VecDeque::with_capacity(capacity.min(4096)),
            crashes: VecDeque::new(),
            capacity,
            current_pid: None,
        }));
        let (events_tx, _) = broadcast::channel(512);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop_notify = Arc::new(Notify::new());

        let bg_serial = serial.clone();
        let bg_package = package.clone();
        let bg_buffers = Arc::clone(&buffers);
        let bg_tx = events_tx.clone();
        let bg_stopped = Arc::clone(&stopped);
        let bg_notify = Arc::clone(&stop_notify);

        tokio::spawn(async move {
            run_logcat_loop(
                bg_serial, bg_package, bg_buffers, bg_tx, bg_stopped, bg_notify,
            )
            .await;
        });

        Self {
            serial,
            package,
            buffers,
            events_tx,
            stopped,
            stop_notify,
        }
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    pub fn package(&self) -> &str {
        &self.package
    }

    pub fn current_pid(&self) -> Option<u32> {
        self.buffers.read().unwrap().current_pid
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogcatEvent> {
        self.events_tx.subscribe()
    }

    pub fn entries(&self, filter: &LogFilter) -> Vec<LogEntry> {
        let guard = self.buffers.read().unwrap();
        let mut out: Vec<LogEntry> = guard
            .entries
            .iter()
            .filter(|e| filter.matches(e))
            .cloned()
            .collect();
        if let Some(limit) = filter.limit {
            if out.len() > limit {
                let drop_n = out.len() - limit;
                out.drain(0..drop_n);
            }
        }
        out
    }

    pub fn crashes(&self) -> Vec<CrashGroup> {
        self.buffers
            .read()
            .unwrap()
            .crashes
            .iter()
            .cloned()
            .collect()
    }

    pub fn clear(&self) {
        let mut guard = self.buffers.write().unwrap();
        guard.entries.clear();
        guard.crashes.clear();
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.stop_notify.notify_waiters();
    }
}

impl Drop for LogcatStream {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn run_logcat_loop(
    serial: String,
    package: String,
    buffers: Arc<RwLock<SharedBuffers>>,
    events_tx: broadcast::Sender<LogcatEvent>,
    stopped: Arc<AtomicBool>,
    stop_notify: Arc<Notify>,
) {
    let seq = AtomicU64::new(1);
    let mut detector = CrashDetector::new();
    let mut announced_waiting = false;

    while !stopped.load(Ordering::SeqCst) {
        let pid = match adb_bridge::find_package_pid(&serial, &package).await {
            Ok(pid) => {
                announced_waiting = false;
                pid
            }
            Err(_) => {
                buffers.write().unwrap().current_pid = None;
                if !announced_waiting {
                    announced_waiting = true;
                    let _ = events_tx.send(LogcatEvent::WaitingForProcess {
                        package: package.clone(),
                    });
                }
                tokio::select! {
                    _ = stop_notify.notified() => break,
                    _ = tokio::time::sleep(Duration::from_millis(600)) => continue,
                }
            }
        };

        buffers.write().unwrap().current_pid = Some(pid);
        let _ = events_tx.send(LogcatEvent::Attached { pid });

        let mut child = match Command::new("adb")
            .args([
                "-s",
                &serial,
                "logcat",
                "-v",
                "threadtime",
                &format!("--pid={pid}"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = events_tx.send(LogcatEvent::Error {
                    message: format!("failed to start adb logcat: {e}"),
                });
                tokio::select! {
                    _ = stop_notify.notified() => break,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => continue,
                }
            }
        };

        let Some(stdout) = child.stdout.take() else {
            continue;
        };
        let mut lines = BufReader::new(stdout).lines();
        let mut pid_check = tokio::time::interval(Duration::from_millis(1500));
        pid_check.tick().await; // consume initial tick

        loop {
            tokio::select! {
                _ = stop_notify.notified() => {
                    let _ = child.kill().await;
                    return;
                }
                _ = pid_check.tick() => {
                    // Detect app restart (PID changed) or crash exit so we reattach cleanly.
                    match adb_bridge::find_package_pid(&serial, &package).await {
                        Ok(current) if current == pid => {}
                        _ => {
                            let _ = child.kill().await;
                            break;
                        }
                    }
                }
                line_res = lines.next_line() => {
                    match line_res {
                        Ok(Some(raw)) => {
                            let id = seq.fetch_add(1, Ordering::Relaxed);
                            if let Some(entry) = parse_threadtime_line(&raw, id) {
                                if let Some(crash) = detector.push(&entry) {
                                    record_crash(&buffers, &events_tx, crash);
                                }
                                record_entry(&buffers, &events_tx, entry);
                            }
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
            }
        }

        if let Some(crash) = detector.flush() {
            record_crash(&buffers, &events_tx, crash);
        }
    }
}

fn record_entry(
    buffers: &Arc<RwLock<SharedBuffers>>,
    events_tx: &broadcast::Sender<LogcatEvent>,
    entry: LogEntry,
) {
    {
        let mut guard = buffers.write().unwrap();
        if guard.entries.len() >= guard.capacity {
            guard.entries.pop_front();
        }
        guard.entries.push_back(entry.clone());
    }
    let _ = events_tx.send(LogcatEvent::Entry { entry });
}

fn record_crash(
    buffers: &Arc<RwLock<SharedBuffers>>,
    events_tx: &broadcast::Sender<LogcatEvent>,
    crash: CrashGroup,
) {
    {
        let mut guard = buffers.write().unwrap();
        if guard.crashes.len() >= MAX_CRASH_GROUPS {
            guard.crashes.pop_front();
        }
        guard.crashes.push_back(crash.clone());
    }
    let _ = events_tx.send(LogcatEvent::Crash { crash });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_threadtime_line() {
        let line = "11-23 21:14:05.123  6083  6099 D MainActivity: onCreate: offset=0";
        let entry = parse_threadtime_line(line, 1).unwrap();
        assert_eq!(entry.seq, 1);
        assert_eq!(entry.timestamp, "11-23 21:14:05.123");
        assert_eq!(entry.pid, 6083);
        assert_eq!(entry.tid, 6099);
        assert_eq!(entry.level, LogLevel::Debug);
        assert_eq!(entry.tag, "MainActivity");
        assert_eq!(entry.message, "onCreate: offset=0");
    }

    #[test]
    fn skips_header_and_blank_lines() {
        assert!(parse_threadtime_line("--------- beginning of main", 1).is_none());
        assert!(parse_threadtime_line("", 2).is_none());
    }

    #[test]
    fn filter_matches_level_tag_and_query() {
        let entry = LogEntry {
            seq: 1,
            timestamp: "11-23 21:14:05.123".into(),
            pid: 100,
            tid: 100,
            level: LogLevel::Warn,
            tag: "BoardViewModel".into(),
            message: "Prayer time recalculated".into(),
        };

        let f1 = LogFilter {
            min_level: Some(LogLevel::Info),
            tag: Some("board".into()),
            query: Some("recalc".into()),
            limit: None,
        };
        assert!(f1.matches(&entry));

        let f2 = LogFilter {
            min_level: Some(LogLevel::Error),
            ..Default::default()
        };
        assert!(!f2.matches(&entry));
    }

    #[test]
    fn crash_detector_groups_fatal_android_runtime_stack() {
        let lines = [
            "11-23 21:14:05.100  6083  6083 E AndroidRuntime: FATAL EXCEPTION: main",
            "11-23 21:14:05.100  6083  6083 E AndroidRuntime: Process: com.everybit.sholattv.native.dev, PID: 6083",
            "11-23 21:14:05.100  6083  6083 E AndroidRuntime: java.lang.IllegalStateException: boom",
            "11-23 21:14:05.100  6083  6083 E AndroidRuntime: \tat com.everybit.sholattv.MainActivity.onCreate(MainActivity.kt:27)",
            "11-23 21:14:05.100  6083  6083 E AndroidRuntime: Caused by: java.io.IOException: disk full",
            "11-23 21:14:05.100  6083  6083 E AndroidRuntime: \t... 4 more",
            "11-23 21:14:05.200  6083  6083 I Process : Sending signal. PID: 6083 SIG: 9",
        ];

        let mut detector = CrashDetector::new();
        let mut crashes = Vec::new();
        for (i, raw) in lines.iter().enumerate() {
            let entry = parse_threadtime_line(raw, i as u64 + 1).unwrap();
            if let Some(c) = detector.push(&entry) {
                crashes.push(c);
            }
        }
        if let Some(c) = detector.flush() {
            crashes.push(c);
        }

        assert_eq!(crashes.len(), 1);
        let crash = &crashes[0];
        assert_eq!(
            crash.process.as_deref(),
            Some("com.everybit.sholattv.native.dev")
        );
        assert_eq!(crash.summary, "java.lang.IllegalStateException: boom");
        assert_eq!(crash.lines.len(), 6);
    }
}
