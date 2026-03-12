use std::sync::Arc;
use std::time::Duration;

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
    let interval_secs = state.cfg.poll_interval_minutes * 60;
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));

    loop {
        ticker.tick().await;
        if !in_polling_window() {
            debug!("outside polling window (07:30-22:30), skipping");
            continue;
        }
        poll_once(&state).await;
    }
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
        format!(
            "{:04}{:02}{:02}",
            now.year(),
            now.month() as u8,
            now.day()
        )
    };

    info!(date = %today, students = registrations.len(), "poller running");

    for (student_id, course_ids) in &registrations {
        match state.client.query_schedule(student_id, &today).await {
            Ok(schedules) => {
                scheduler::plan_tasks(&state.queue, student_id, &schedules, course_ids).await;
            }
            Err(e) => {
                warn!(student = %student_id, err = %e, "schedule fetch failed");
            }
        }
    }
}
