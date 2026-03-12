pub mod poller;
pub mod scheduler;
pub mod worker;

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use dashmap::DashMap;
use tokio::sync::{Mutex, Notify};

// ── SchedulerCache ────────────────────────────────────────────────────────────

/// In-memory mapping student_id -> course_ids loaded from config.
pub struct SchedulerCache {
    inner: DashMap<String, Vec<String>>,
}

impl SchedulerCache {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    pub fn set(&self, student_id: String, ids: Vec<String>) {
        self.inner.insert(student_id, ids);
    }

    pub fn remove(&self, student_id: &str) {
        self.inner.remove(student_id);
    }

    /// Snapshot of all (student_id, course_ids) pairs.
    pub fn snapshot(&self) -> Vec<(String, Vec<String>)> {
        self.inner
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect()
    }
}

// ── Task ──────────────────────────────────────────────────────────────────────

/// A check-in task to be executed at `run_at` (seconds since UNIX_EPOCH).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Task {
    pub run_at: u64,
    pub student_id: String,
    pub schedule_id: String,
    pub course_id: String,
}

// BinaryHeap is a max-heap; wrap in Reverse for min-heap by run_at.
impl Ord for Task {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Reverse(self.run_at).cmp(&Reverse(other.run_at))
    }
}

impl PartialOrd for Task {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ── TaskQueue ─────────────────────────────────────────────────────────────────

pub struct TaskQueue {
    heap: Mutex<BinaryHeap<Task>>,
    seen: Mutex<HashSet<(String, String)>>,
    notify: Notify,
}

impl TaskQueue {
    pub fn new() -> Self {
        Self {
            heap: Mutex::new(BinaryHeap::new()),
            seen: Mutex::new(HashSet::new()),
            notify: Notify::new(),
        }
    }

    /// Enqueue task; silently skips if (student_id, schedule_id) already present.
    pub async fn push(&self, task: Task) {
        let key = (task.student_id.clone(), task.schedule_id.clone());
        let mut seen = self.seen.lock().await;
        if seen.contains(&key) {
            return;
        }
        seen.insert(key);
        drop(seen);
        self.heap.lock().await.push(task);
        self.notify.notify_one();
    }

    /// Pop task whose run_at is ≤ now. Returns None if queue is empty or top isn't due.
    pub async fn pop_ready(&self) -> Option<Task> {
        let now = now_secs();
        let mut heap = self.heap.lock().await;
        if heap.peek().map(|t| t.run_at <= now).unwrap_or(false) {
            let task = heap.pop().unwrap();
            let mut seen = self.seen.lock().await;
            seen.remove(&(task.student_id.clone(), task.schedule_id.clone()));
            return Some(task);
        }
        None
    }

    /// Seconds until the earliest task is due; None if queue is empty.
    pub async fn secs_until_next(&self) -> Option<u64> {
        let heap = self.heap.lock().await;
        let now = now_secs();
        heap.peek().map(|t| t.run_at.saturating_sub(now))
    }

    /// Notify the wait handle (used when new tasks are enqueued).
    pub fn notify(&self) {
        self.notify.notify_one();
    }

    /// Wait for a notification (task enqueue or timeout wakeup).
    pub async fn wait(&self) {
        self.notify.notified().await;
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
