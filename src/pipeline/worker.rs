use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use tracing::{debug, info, warn};

use super::{scheduler::parse_class_time, CHECKIN_RETRY_INTERVAL_SECS, POST_CLASS_WINDOW_SECS};
use crate::AppState;

/// Fallback cap when the schedule carries no end time.
const MAX_ATTEMPTS_WITHOUT_END: u32 = 20;
/// Errors that no amount of retrying can fix — bail out immediately.
const FATAL_CHECKIN_ERRORS: &[&str] = &["非本节课学生", "用户不存在"];

/// Long-running task: consume queued tasks when they become due.
pub async fn run(state: Arc<AppState>) {
    loop {
        // Drain all tasks that are ready right now.
        while let Some(task) = state.queue.pop_ready().await {
            let s = state.clone();
            tokio::spawn(async move {
                execute_task(
                    &s,
                    &task.student_id,
                    &task.schedule_id,
                    &task.course_id,
                    &task.group,
                )
                .await;
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

/// Execute one check-in task with dynamic validation, 8081 retries, and 8082 fallback.
async fn execute_task(
    state: &AppState,
    student_id: &str,
    schedule_id: &str,
    course_id: &str,
    group: &[String],
) {
    let today = today_str();
    let schedules = match state.client.refresh_schedule(student_id, &today).await {
        Ok(s) => s,
        Err(e) => {
            warn!(student = student_id, err = %e, "pre-checkin schedule fetch failed; skipping");
            return;
        }
    };
    let queued_group = if group.is_empty() {
        vec![schedule_id.to_owned()]
    } else {
        group.to_vec()
    };
    let group = match current_group(state, student_id, &queued_group, &schedules) {
        Ok(group) => group,
        Err(e) => {
            info!(student = student_id, sched = schedule_id, reason = %e, "task skipped");
            return;
        }
    };
    if schedules
        .iter()
        .any(|s| group.contains(&s.id) && s.status() == 1)
    {
        info!(
            student = student_id,
            sched = schedule_id,
            "task skipped: already_signed"
        );
        return;
    }

    // 3. Keep the signed 8081 path alive from the pre-class window through
    //    10 minutes after class; only then use the 8082 fallback.
    match run_checkin_with_retries(
        state,
        student_id,
        schedule_id,
        course_id,
        &group,
        &schedules,
    )
    .await
    {
        Ok(attempts) => {
            info!(
                student = student_id,
                sched = schedule_id,
                course_id = course_id,
                attempts,
                "checkin ok"
            );
        }
        Err(e) => {
            warn!(
                student = student_id,
                sched = schedule_id,
                course_id = course_id,
                err = %e,
                "checkin failed; giving up"
            );
        }
    }
}

/// Keep attempting the signed 8081 path every
/// [`CHECKIN_RETRY_INTERVAL_SECS`] until it succeeds, the class ends
/// (+[`POST_CLASS_WINDOW_SECS`]), or a permanently fatal error surfaces.
/// Once that window is over (or if no secret provider is configured), run the
/// unsigned 8082 fallback with one jittered retry.
async fn run_checkin_with_retries(
    state: &AppState,
    student_id: &str,
    schedule_id: &str,
    course_id: &str,
    group: &[String],
    schedules: &[crate::client::Schedule],
) -> crate::error::AppResult<u32> {
    let end_unix = schedules
        .iter()
        .filter(|s| group.contains(&s.id))
        .filter_map(|s| s.end_time.as_deref().and_then(parse_class_time))
        .max();
    let hard_end = end_unix.map(|e| e + POST_CLASS_WINDOW_SECS);
    let mut attempt: u32 = 1;
    if state.client.has_secret_provider(student_id) {
        loop {
            let active = current_group(state, student_id, group, schedules)?;
            if !state.client.has_secret_provider(student_id) {
                break;
            }
            match do_signed_checkin(state, student_id, &active).await {
                Ok(()) => return Ok(attempt),
                Err(e) => {
                    if group_signed(state, student_id, &active).await {
                        info!(
                            student = student_id,
                            sched = schedule_id,
                            attempts = attempt,
                            "checkin ok (group already signed)"
                        );
                        return Ok(attempt);
                    }
                    if FATAL_CHECKIN_ERRORS.iter().any(|s| e.message.contains(s)) {
                        warn!(student = student_id, sched = schedule_id, course_id = course_id, err = %e, "checkin failed permanently; giving up");
                        return Err(e);
                    }
                    let now = super::now_secs();
                    let over = match hard_end {
                        Some(h) => now + CHECKIN_RETRY_INTERVAL_SECS >= h,
                        None => attempt >= MAX_ATTEMPTS_WITHOUT_END,
                    };
                    if over {
                        warn!(student = student_id, sched = schedule_id, course_id = course_id, err = %e, attempts = attempt, "8081 window over; falling back to 8082");
                        break;
                    }
                    if attempt == 1 {
                        warn!(student = student_id, sched = schedule_id, course_id = course_id, err = %e, retry_secs = CHECKIN_RETRY_INTERVAL_SECS, "8081 checkin failed; retrying during class");
                    } else {
                        debug!(student = student_id, sched = schedule_id, course_id = course_id, err = %e, attempt, "8081 checkin retry failed");
                    }
                    attempt += 1;
                    tokio::time::sleep(Duration::from_secs(CHECKIN_RETRY_INTERVAL_SECS)).await;
                }
            }
        }
    } else {
        info!(
            student = student_id,
            sched = schedule_id,
            course_id = course_id,
            "no 8081 secret provider; using 8082 fallback"
        );
    }

    let active = current_group(state, student_id, group, schedules)?;
    match do_fallback_checkin(state, student_id, &active).await {
        Ok(()) => Ok(attempt),
        Err(e) => {
            let jitter: u64 = rand::rng().random_range(1..=5);
            warn!(
                student = student_id,
                sched = schedule_id,
                course_id = course_id,
                err = %e,
                retry_secs = jitter,
                "8082 fallback failed; retrying once"
            );
            tokio::time::sleep(Duration::from_secs(jitter)).await;
            let active = current_group(state, student_id, group, schedules)?;
            if group_signed(state, student_id, &active).await {
                info!(
                    student = student_id,
                    sched = schedule_id,
                    "checkin already confirmed after error"
                );
                return Ok(attempt);
            }
            do_fallback_checkin(state, student_id, &active).await?;
            Ok(attempt + 1)
        }
    }
}

/// Re-evaluate every queued member, including before retries and fallback.
/// Removing the old representative must not discard other selected members.
fn current_group(
    state: &AppState,
    student_id: &str,
    group: &[String],
    schedules: &[crate::client::Schedule],
) -> crate::error::AppResult<Vec<String>> {
    let filter = state.cache.get(student_id).ok_or_else(|| {
        crate::error::AppError::new("task_cancelled", "student no longer configured")
    })?;
    let active: Vec<String> = group
        .iter()
        .filter(|id| {
            schedules
                .iter()
                .any(|s| s.id == **id && filter.matches_in_schedules(s, schedules))
        })
        .cloned()
        .collect();
    if active.is_empty() {
        return Err(crate::error::AppError::new(
            "task_cancelled",
            "no selected group members remain in today's schedule",
        ));
    }
    Ok(active)
}

async fn do_signed_checkin(
    state: &AppState,
    student_id: &str,
    group: &[String],
) -> crate::error::AppResult<()> {
    state.client.checkin_signed(student_id, group).await?;
    Ok(())
}

async fn do_fallback_checkin(
    state: &AppState,
    student_id: &str,
    group: &[String],
) -> crate::error::AppResult<()> {
    state.client.checkin_fallback(student_id, group).await?;
    Ok(())
}

/// Force-refresh today's schedule and report whether any group member is signed.
async fn group_signed(state: &AppState, student_id: &str, group: &[String]) -> bool {
    if group.is_empty() {
        return false;
    }
    let today = today_str();
    match state.client.refresh_schedule(student_id, &today).await {
        Ok(scheds) => scheds
            .iter()
            .any(|s| group.contains(&s.id) && s.status() == 1),
        Err(_) => false,
    }
}

fn today_str() -> String {
    use time::macros::offset;
    use time::OffsetDateTime;
    let now = OffsetDateTime::now_utc().to_offset(offset!(+8));
    format!("{:04}{:02}{:02}", now.year(), now.month() as u8, now.day())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::scheduler::CourseFilter;

    fn state() -> AppState {
        AppState {
            cfg: Arc::new(crate::config::Config {
                poll_interval_minutes: 5,
                auto_window_minutes: 15,
                log_file: None,
                students: vec![],
            }),
            config_path: String::new(),
            client: Arc::new(crate::client::ClassClient::new(&[])),
            cache: Arc::new(crate::pipeline::SchedulerCache::new()),
            queue: Arc::new(crate::pipeline::TaskQueue::new()),
        }
    }

    fn schedules() -> Vec<crate::client::Schedule> {
        [("1", "11", "A"), ("2", "22", "B")]
            .into_iter()
            .map(|(id, course, name)| {
                serde_json::from_value(serde_json::json!({
                    "id": id, "courseId": course, "courseName": name,
                    "teacherName": "T", "classBeginTime": "2100-01-01 08:00:00"
                }))
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn queued_group_uses_reloaded_exclusions_for_every_member() {
        let state = state();
        let group = vec!["1".into(), "2".into()];
        let schedules = schedules();
        state
            .cache
            .set("s1".into(), CourseFilter::new(&["*".into()], &[]));
        assert_eq!(
            current_group(&state, "s1", &group, &schedules).unwrap(),
            group
        );
        // Excluding the non-representative after enqueue must remove it too.
        state
            .cache
            .set("s1".into(), CourseFilter::new(&["*".into()], &["B".into()]));
        assert_eq!(
            current_group(&state, "s1", &group, &schedules).unwrap(),
            vec!["1"]
        );
        // Excluding the representative must still allow the other course.
        state.cache.set(
            "s1".into(),
            CourseFilter::new(&["*".into()], &["11".into()]),
        );
        assert_eq!(
            current_group(&state, "s1", &group, &schedules).unwrap(),
            vec!["2"]
        );
    }

    #[tokio::test]
    async fn cancelled_groups_never_reach_fallback() {
        let state = state();
        let group = vec!["1".into(), "2".into()];
        let schedules = schedules();
        state.cache.set(
            "s1".into(),
            CourseFilter::new(&["*".into()], &["A".into(), "B".into()]),
        );
        let error = run_checkin_with_retries(&state, "s1", "1", "11", &group, &schedules)
            .await
            .unwrap_err();
        assert_eq!(error.code, "task_cancelled");
        state.cache.remove("s1");
        let error = run_checkin_with_retries(&state, "s1", "1", "11", &group, &schedules)
            .await
            .unwrap_err();
        assert_eq!(error.code, "task_cancelled");
    }

    #[test]
    fn vanished_group_member_is_not_sent_to_checkin() {
        let state = state();
        state
            .cache
            .set("s1".into(), CourseFilter::new(&["*".into()], &[]));
        let group = vec!["missing".into(), "2".into()];
        assert_eq!(
            current_group(&state, "s1", &group, &schedules()).unwrap(),
            vec!["2"]
        );
        assert_eq!(
            current_group(&state, "s1", &["missing".into()], &schedules())
                .unwrap_err()
                .code,
            "task_cancelled"
        );
    }
}
