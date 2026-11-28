pub mod poller;
pub mod scheduler;
pub mod weekly;
pub mod worker;

/// Start trying the signed 8081 path this long before class begins.
pub const PRE_CLASS_WINDOW_SECS: u64 = 10 * 60;
/// Keep trying the signed 8081 path this long after class ends.
pub const POST_CLASS_WINDOW_SECS: u64 = 10 * 60;
/// Interval between signed 8081 attempts.
pub const CHECKIN_RETRY_INTERVAL_SECS: u64 = 30;

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap},
    time::{SystemTime, UNIX_EPOCH},
};

use dashmap::DashMap;
use tokio::sync::{Mutex, Notify};

// ── SchedulerCache ────────────────────────────────────────────────────────────

/// In-memory mapping student_id -> course filter loaded from config.
pub struct SchedulerCache {
    inner: DashMap<String, scheduler::CourseFilter>,
}

impl SchedulerCache {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    pub fn set(&self, student_id: String, filter: scheduler::CourseFilter) {
        self.inner.insert(student_id, filter);
    }

    pub fn remove(&self, student_id: &str) {
        self.inner.remove(student_id);
    }

    pub fn get(&self, student_id: &str) -> Option<scheduler::CourseFilter> {
        self.inner.get(student_id).map(|e| e.value().clone())
    }

    /// Snapshot of all (student_id, course filter) pairs.
    pub fn snapshot(&self) -> Vec<(String, scheduler::CourseFilter)> {
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
    pub class_start: u64,
    pub student_id: String,
    /// Representative schedule id (the lexicographically smallest member).
    pub schedule_id: String,
    pub course_id: String,
    /// All schedule ids of this parallel-section group (same start time slot).
    /// Signing any member flips the shared sign status, and only the teacher's
    /// own section has an open 8081 QR session, so the worker tries the official
    /// path across the whole group but falls back to the unsigned 8082 path at
    /// most once per group.
    pub group: Vec<String>,
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
    seen: Mutex<HashMap<(String, String), u64>>,
    notify: Notify,
}

impl TaskQueue {
    pub fn new() -> Self {
        Self {
            heap: Mutex::new(BinaryHeap::new()),
            seen: Mutex::new(HashMap::new()),
            notify: Notify::new(),
        }
    }

    /// Enqueue task once per class session, including tasks already in-flight or executed.
    pub async fn push(&self, task: Task) {
        let key = (task.student_id.clone(), task.schedule_id.clone());
        let mut seen = self.seen.lock().await;
        let now = now_secs();
        seen.retain(|_, expires| *expires > now);
        if seen.contains_key(&key) {
            return;
        }
        let expires = task
            .class_start
            .saturating_add(7200)
            .max(now.saturating_add(3600));
        seen.insert(key, expires);
        drop(seen);
        self.heap.lock().await.push(task);
        self.notify.notify_one();
    }

    /// Pop task whose run_at is <= now. Returns None if queue is empty or top isn't due.
    pub async fn pop_ready(&self) -> Option<Task> {
        let now = now_secs();
        let mut heap = self.heap.lock().await;
        if heap.peek().map(|t| t.run_at <= now).unwrap_or(false) {
            return heap.pop();
        }
        None
    }

    /// Test-only: clone every queued task regardless of readiness.
    #[cfg(test)]
    pub async fn snapshot_all(&self) -> Vec<Task> {
        self.heap.lock().await.iter().cloned().collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completed_task_is_not_queued_again() {
        let queue = TaskQueue::new();
        let now = now_secs();
        let task = Task {
            run_at: now,
            class_start: now + 120,
            student_id: "abc".into(),
            schedule_id: "s1".into(),
            course_id: "c1".into(),
            group: vec!["s1".into()],
        };
        queue.push(task.clone()).await;
        assert!(queue.pop_ready().await.is_some());
        queue.push(task).await;
        assert!(queue.pop_ready().await.is_none());
    }
}
