use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tracing::{debug, info, warn};

use crate::AppState;

use super::scheduler;

/// Returns true if the current Beijing time is within [07:30, 22:30].
fn in_polling_window() -> bool {
    use time::{macros::offset, OffsetDateTime};
    let now = OffsetDateTime::now_utc().to_offset(offset!(+8));
    let total_minutes = now.hour() as u16 * 60 + now.minute() as u16;
    // 07:30 = 450,  22:30 = 1350
    (450..=1350).contains(&total_minutes)
}

/// Long-running task: periodically fetches today's schedules for all registered students
/// and enqueues auto-checkin tasks.
pub async fn run(state: Arc<AppState>) {
    let interval_secs = state.cfg.poll_interval_minutes.max(1) * 60;
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    let mut last_mtime: Option<SystemTime> = None;

    loop {
        ticker.tick().await;
        reload_config(&state, &mut last_mtime);
        if !in_polling_window() {
            debug!("outside polling window (07:30-22:30), skipping");
            continue;
        }
        poll_once(&state).await;
    }
}

/// Hot-reload config.json when its mtime changes; keep the previous config on errors.
fn reload_config(state: &AppState, last_mtime: &mut Option<SystemTime>) {
    let mtime = match std::fs::metadata(&state.config_path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(e) => {
            warn!(path = %state.config_path, err = %e, "config stat failed, skipping reload");
            return;
        }
    };
    if *last_mtime == Some(mtime) {
        return;
    }

    match crate::config::Config::try_load(&state.config_path) {
        Ok(cfg) => {
            apply_students(state, &cfg.students);
            // Only remember the mtime of a config we actually applied; a broken
            // file keeps last_mtime stale so the next tick retries it.
            *last_mtime = Some(mtime);
            info!(path = %state.config_path, students = cfg.students.len(), "config reloaded");
        }
        Err(e) => {
            warn!(err = %e, "config reload failed, keeping previous config");
        }
    }
}

/// Sync the runtime student state (filters + accounts) with a freshly loaded config.
/// Students no longer present are removed together with their cached sessions.
fn apply_students(state: &AppState, students: &[crate::config::StudentEntry]) {
    let mut new_ids = HashSet::new();
    for entry in students {
        new_ids.insert(entry.student_id.clone());
        state.cache.set(
            entry.student_id.clone(),
            scheduler::CourseFilter::from(entry),
        );
    }

    let gone: Vec<String> = state
        .cache
        .snapshot()
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| !new_ids.contains(id))
        .collect();
    for id in gone {
        state.cache.remove(&id);
        state.client.remove_student(&id);
        info!(student = %id, "student removed from config, cleared runtime state");
    }

    state.client.sync_accounts(students);
}

pub async fn poll_once(state: &AppState) {
    let registrations = state.cache.snapshot();
    if registrations.is_empty() {
        return;
    }

    let today = {
        use time::macros::offset;
        use time::OffsetDateTime;
        let now = OffsetDateTime::now_utc().to_offset(offset!(+8));
        format!("{:04}{:02}{:02}", now.year(), now.month() as u8, now.day())
    };

    info!(date = %today, students = registrations.len(), "poller running");

    for (student_id, filter) in &registrations {
        match state.client.refresh_schedule(student_id, &today).await {
            Ok(schedules) => {
                if schedules.is_empty() {
                    debug!(student = %student_id, date = %today, "today's schedule is empty");
                }
                scheduler::plan_tasks(&state.queue, student_id, &schedules, filter).await;
            }
            Err(e) => {
                warn!(student = %student_id, err = %e, "schedule fetch failed");
            }
        }
    }
}
