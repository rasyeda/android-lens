use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PinKind {
    StackFrame,
    Variable,
    LogcatEntry,
    Crash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedItem {
    pub id: String,
    pub kind: PinKind,
    pub title: String,
    pub summary: String,
    pub note: Option<String>,
    pub payload: Value,
    pub pinned_at_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewPinRequest {
    pub kind: PinKind,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Default)]
pub struct PinnedContextStore {
    next_id: AtomicU64,
    items: RwLock<Vec<PinnedItem>>,
}

impl PinnedContextStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, req: NewPinRequest) -> PinnedItem {
        let seq = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let item = PinnedItem {
            id: format!("pin-{seq}"),
            kind: req.kind,
            title: req.title,
            summary: req.summary,
            note: req.note,
            payload: req.payload,
            pinned_at_ms: now,
        };
        let mut guard = self.items.write().unwrap();
        if guard.len() >= 100 {
            guard.remove(0);
        }
        guard.push(item.clone());
        item
    }

    pub fn list(&self) -> Vec<PinnedItem> {
        self.items.read().unwrap().clone()
    }

    pub fn remove(&self, id: &str) -> bool {
        let mut guard = self.items.write().unwrap();
        let before = guard.len();
        guard.retain(|i| i.id != id);
        guard.len() < before
    }

    pub fn clear(&self) {
        self.items.write().unwrap().clear();
    }
}
