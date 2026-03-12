use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use tracing::{info, warn};

use crate::AppState;


/// Long-running task: consume queued tasks when they become due.
pub async fn run(state: Arc<AppState>) {
    loop {
        // Drain all tasks that are ready right now.
        while let Some(task) = state.queue.pop_ready().await {
            let s = state.clone();
            tokio::spawn(async move {
                execute_task(&s, &task.student_id, &task.schedule_id, &task.course_id).await;
            });
        }

        // Sleep until the next task is due (or 60 s max).
        let sleep_secs = state.queue.secs_until_next().await.unwrap_or(60).min(60);
        if sleep_secs > 0 {
            // Wait either for sleep duration or a new task notification.
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(sleep_secs)) => {}
                _ = state.queue.wait() => {}
            }
        }
    }
}

/// Execute one check-in task with dynamic validation and one retry on transient failure.
async fn execute_task(state: &AppState, student_id: &str, schedule_id: &str, course_id: &str) {
    // 2. Dynamic validation: fetch today's schedule and confirm sched is unsigned.
    let today = today_str();
    let schedules = match state.client.query_schedule(student_id, &today).await {
        Ok(s) => s,
        Err(e) => {
            warn!(student = student_id, err = %e, "pre-checkin schedule fetch failed; skipping");
            return;
        }
    };

    let target = schedules.iter().find(|s| s.course_id == course_id);
    match target {
        None => {
            info!(
                student = student_id,
                sched = schedule_id,
                course_id = course_id,
                "task skipped: not_found in today's schedule"
            );
            return;
        }
        Some(s) if s.status() == 1 => {
            info!(
                student = student_id,
                sched = schedule_id,
                course_id = course_id,
                "task skipped: already_signed"
            );
            return;
        }
        _ => {}
    }

    // 3. Execute check-in.
    match do_checkin(state, student_id, schedule_id).await {
        Ok(_) => {
            info!(student = student_id, sched = schedule_id, course_id = course_id, "checkin ok");
        }
        Err(e) => {
            // One retry after random jitter (1-5 s).
            let jitter: u64 = rand::rng().random_range(1..=5);
            warn!(
                student = student_id,
                sched = schedule_id,
                course_id = course_id,
                err = %e,
                retry_secs = jitter,
                "checkin failed; retrying"
            );
            tokio::time::sleep(Duration::from_secs(jitter)).await;

            match do_checkin(state, student_id, schedule_id).await {
                Ok(_) => {
                    info!(student = student_id, sched = schedule_id, course_id = course_id, "checkin ok (retry)");
                }
                Err(e2) => {
                    warn!(student = student_id, sched = schedule_id, course_id = course_id, err = %e2, "checkin failed after retry");
                }
            }
        }
    }
}

async fn do_checkin(
    state: &AppState,
    student_id: &str,
    course_sched_id: &str,
) -> crate::error::AppResult<()> {
    state.client.checkin(student_id, course_sched_id).await?;
    Ok(())
}

fn today_str() -> String {
    use time::macros::offset;
    use time::OffsetDateTime;
    let now = OffsetDateTime::now_utc().to_offset(offset!(+8));
    format!(
        "{:04}{:02}{:02}",
        now.year(),
        now.month() as u8,
        now.day()
    )
}
