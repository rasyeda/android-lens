pub mod http;
pub mod mcp;
pub mod pins;
pub mod session;

use std::sync::{Arc, RwLock};
use std::time::Duration;

use logcat_stream::{
    CrashGroup, LogEntry, LogFilter, LogLevel, LogcatEvent, LogcatStream, DEFAULT_RING_CAPACITY,
};
use tokio::sync::{broadcast, Mutex};

use crate::pins::PinnedContextStore;
use crate::session::DebugSession;

pub const DEFAULT_MCP_PORT: u16 = 43821;

struct LogcatSlot {
    stream: Option<LogcatStream>,
    forward_task: Option<tokio::task::JoinHandle<()>>,
}

/// Shared application core owned jointly by the Tauri Desktop UI and the local MCP server.
pub struct SharedLensState {
    pub debug: DebugSession,
    pub pins: PinnedContextStore,
    project_root: RwLock<Option<String>>,
    logcat: Mutex<LogcatSlot>,
    logcat_tx: broadcast::Sender<LogcatEvent>,
}

impl Default for SharedLensState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedLensState {
    pub fn new() -> Self {
        let (logcat_tx, _) = broadcast::channel(512);
        Self {
            debug: DebugSession::new(),
            pins: PinnedContextStore::new(),
            project_root: RwLock::new(None),
            logcat: Mutex::new(LogcatSlot {
                stream: None,
                forward_task: None,
            }),
            logcat_tx,
        }
    }

    pub fn set_project_root(&self, root: Option<String>) {
        *self.project_root.write().unwrap() = root;
    }

    pub fn project_root(&self) -> Option<String> {
        self.project_root.read().unwrap().clone()
    }

    pub fn subscribe_logcat(&self) -> broadcast::Receiver<LogcatEvent> {
        self.logcat_tx.subscribe()
    }

    pub async fn start_logcat(
        &self,
        serial: Option<String>,
        package: String,
    ) -> Result<(), String> {
        let dev = adb_bridge::resolve_device(serial.as_deref())
            .await
            .map_err(|e| e.to_string())?;

        let mut lc = self.logcat.lock().await;
        if let Some(old_task) = lc.forward_task.take() {
            old_task.abort();
        }
        if let Some(old_stream) = lc.stream.take() {
            old_stream.stop();
        }

        let stream = LogcatStream::start(dev.serial, package, DEFAULT_RING_CAPACITY);
        let mut rx = stream.subscribe();
        let tx = self.logcat_tx.clone();
        let task = tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                let _ = tx.send(ev);
            }
        });

        lc.stream = Some(stream);
        lc.forward_task = Some(task);
        Ok(())
    }

    pub async fn stop_logcat(&self) {
        let mut lc = self.logcat.lock().await;
        if let Some(task) = lc.forward_task.take() {
            task.abort();
        }
        if let Some(stream) = lc.stream.take() {
            stream.stop();
        }
    }

    pub async fn clear_logcat(&self) {
        if let Some(ref stream) = self.logcat.lock().await.stream {
            stream.clear();
        }
    }

    pub async fn query_logcat(
        &self,
        min_level: Option<LogLevel>,
        tag: Option<String>,
        query: Option<String>,
        limit: Option<usize>,
    ) -> Vec<LogEntry> {
        let lc = self.logcat.lock().await;
        let Some(ref stream) = lc.stream else {
            return Vec::new();
        };
        stream.entries(&LogFilter {
            min_level,
            tag,
            query,
            limit,
        })
    }

    pub async fn logcat_crashes(&self) -> Vec<CrashGroup> {
        let lc = self.logcat.lock().await;
        let Some(ref stream) = lc.stream else {
            return Vec::new();
        };
        stream.crashes()
    }

    /// Ensures a logcat stream is running for `package` (or the currently attached package),
    /// waits briefly if just started so the initial ring buffer populates, and returns entries + crashes.
    pub async fn tail_logcat(
        &self,
        package: Option<String>,
        min_level: Option<LogLevel>,
        tag: Option<String>,
        query: Option<String>,
        limit: usize,
    ) -> Result<(Option<String>, Vec<LogEntry>, Vec<CrashGroup>), String> {
        let target_pkg = match package.filter(|s| !s.trim().is_empty()) {
            Some(p) => Some(p),
            None => {
                let current = self
                    .logcat
                    .lock()
                    .await
                    .stream
                    .as_ref()
                    .map(|s| s.package().to_string());
                match current {
                    Some(p) => Some(p),
                    None => self.debug.status().await.package,
                }
            }
        };

        let Some(pkg) = target_pkg else {
            return Err("no package specified and no active debug/logcat session".into());
        };

        let needs_start = {
            let lc = self.logcat.lock().await;
            lc.stream
                .as_ref()
                .map(|s| s.package() != pkg)
                .unwrap_or(true)
        };

        if needs_start {
            let serial = self.debug.status().await.serial;
            self.start_logcat(serial, pkg.clone()).await?;
            tokio::time::sleep(Duration::from_millis(350)).await;
        }

        let entries = self
            .query_logcat(min_level, tag, query, Some(limit.clamp(1, 500)))
            .await;
        let crashes = self.logcat_crashes().await;
        Ok((Some(pkg), entries, crashes))
    }

    /// Convenience constructor returning an `Arc` ready for sharing between Tauri and MCP.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }
}
