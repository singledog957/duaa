use rand::Rng;
use serde::Deserialize;
use tracing::debug;

use super::{Task, TaskQueue};

#[derive(Deserialize)]
struct PersistedCourseTarget {
    course_id: Option<String>,
    name: Option<String>,
}

/// Parse an iclass datetime string "YYYY-MM-DD HH:MM[:SS]" → UNIX seconds (UTC+8→UTC).
pub fn parse_class_time(s: &str) -> Option<u64> {
    // Format: "2024-03-01 08:00:00" or "2024-03-01 08:00"
    let fmt_full =
        time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    let fmt_short = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");

    let pdt = time::PrimitiveDateTime::parse(s, fmt_full)
        .or_else(|_| time::PrimitiveDateTime::parse(s, fmt_short))
        .ok()?;

    // Treat as Beijing time (UTC+8); convert to UTC seconds.
    let offset = time::UtcOffset::from_hms(8, 0, 0).ok()?;
    let odt = pdt.assume_offset(offset);
    let unix = odt.unix_timestamp();
    if unix < 0 {
        return None;
    }
    Some(unix as u64)
}

/// Plan from ten to one minute before class; schedule immediately if discovered late.
pub fn run_at_from(class_start_secs: u64, now_secs: u64) -> Option<u64> {
    if now_secs >= class_start_secs {
        return None;
    }
    let earliest = class_start_secs.saturating_sub(600).max(now_secs);
    let latest = class_start_secs.saturating_sub(60);
    if earliest >= latest {
        return Some(now_secs);
    }
    Some(rand::rng().random_range(earliest..=latest))
}

fn targets(
    course_ids: &[String],
    schedules: &[crate::client::Schedule],
) -> (
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
) {
    let mut ids = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for entry in course_ids {
        if let Ok(saved) = serde_json::from_str::<PersistedCourseTarget>(entry) {
            if let Some(id) = saved.course_id.filter(|id| !id.is_empty()) {
                ids.insert(id);
            }
            if let Some(name) = saved.name.filter(|name| !name.is_empty()) {
                names.insert(name);
            }
        } else if entry.chars().all(|c| c.is_ascii_digit()) {
            ids.insert(entry.clone());
        } else {
            names.insert(entry.clone());
        }
    }
    for sched in schedules {
        if ids.contains(&sched.course_id) || ids.contains(&sched.id) {
            names.insert(sched.name.clone());
        }
    }
    (ids, names)
}

pub fn matches_manual_target(
    target: &crate::client::Schedule,
    schedules: &[crate::client::Schedule],
    course_ids: &[String],
) -> bool {
    let (ids, names) = targets(course_ids, schedules);
    ids.contains(&target.course_id) || ids.contains(&target.id) || names.contains(&target.name)
}

/// Enqueue tasks for a single student based on their schedules.
///
/// Only enqueues tasks whose `run_at` is in the future.
pub async fn plan_tasks(
    queue: &TaskQueue,
    student_id: &str,
    schedules: &[crate::client::Schedule],
    course_ids: &[String],
    auto_window_minutes: u64,
    all_courses: bool,
) {
    let now = super::now_secs();
    let window_end = now.saturating_add(auto_window_minutes.saturating_mul(60));
    let (registered_ids, registered_names) = targets(course_ids, schedules);

    for sched in schedules {
        // Only schedule for courses the student has registered for auto-checkin.
        let id_match =
            registered_ids.contains(&sched.course_id) || registered_ids.contains(&sched.id);
        if !all_courses && !id_match && !registered_names.contains(&sched.name) {
            continue;
        }
        // Don't enqueue if already signed.
        if sched.status() == 1 {
            continue;
        }
        let Some(class_start) = parse_class_time(&sched.time) else {
            continue;
        };
        if !all_courses && auto_window_minutes > 0 && class_start > window_end {
            continue;
        }
        let Some(run_at) = run_at_from(class_start, now) else {
            continue;
        };
        debug!(
            student = student_id,
            sched_id = %sched.id,
            course_id = %sched.course_id,
            run_at,
            class_start,
            "scheduling task"
        );
        queue
            .push(Task {
                run_at,
                class_start,
                student_id: student_id.to_owned(),
                schedule_id: sched.id.clone(),
                course_id: sched.course_id.clone(),
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Schedule;

    fn schedule(id: &str, course_id: &str, name: &str, teacher: &str) -> Schedule {
        Schedule {
            id: id.into(),
            course_id: course_id.into(),
            name: name.into(),
            teacher: teacher.into(),
            time: "2026-09-28 08:00:00".into(),
            end_time: None,
            status_raw: "0".into(),
        }
    }

    #[test]
    fn selected_course_includes_same_name_other_teacher() {
        let schedules = [
            schedule("s1", "11", "高数", "甲"),
            schedule("s2", "12", "高数", "乙"),
            schedule("s3", "13", "英语", "乙"),
        ];
        let targets = vec![r#"{"course_id":"11","name":"高数"}"#.to_owned()];
        assert!(matches_manual_target(&schedules[1], &schedules, &targets));
        assert!(!matches_manual_target(&schedules[2], &schedules, &targets));
        assert!(matches_manual_target(
            &schedules[1],
            &schedules,
            &["11".into()]
        ));
    }

    #[test]
    fn late_discovery_runs_now_before_start() {
        assert_eq!(run_at_from(1000, 980), Some(980));
        assert_eq!(run_at_from(1000, 1000), None);
    }
}
