use rand::Rng;
use tracing::debug;

use super::{Task, TaskQueue};

/// Parse an iclass datetime string "YYYY-MM-DD HH:MM[:SS]" → UNIX seconds (UTC+8→UTC).
pub fn parse_class_time(s: &str) -> Option<u64> {
    // Format: "2024-03-01 08:00:00" or "2024-03-01 08:00"
    let fmt_full = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
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

/// Compute a random run_at within `[class_start - 10 min, class_start]`.
pub fn randomized_run_at(class_start_secs: u64) -> u64 {
    let offset_secs: u64 = rand::rng().random_range(0..=600); // 0..=10 minutes
    class_start_secs.saturating_sub(offset_secs)
}

/// Enqueue tasks for a single student based on their schedules.
///
/// Only enqueues tasks whose `run_at` is in the future.
pub async fn plan_tasks(
    queue: &TaskQueue,
    student_id: &str,
    schedules: &[crate::client::Schedule],
    course_ids: &[String],
) {
    let now = super::now_secs();
    let mut registered_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut registered_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for entry in course_ids {
        if entry.chars().all(|c| c.is_ascii_digit()) {
            registered_ids.insert(entry.as_str());
        } else {
            registered_names.insert(entry.as_str());
        }
    }

    for sched in schedules {
        // Only schedule for courses the student has registered for auto-checkin.
        let id_match = registered_ids.contains(sched.course_id.as_str())
            || registered_ids.contains(sched.id.as_str());
        if !id_match && !registered_names.contains(sched.name.as_str()) {
            continue;
        }
        // Don't enqueue if already signed.
        if sched.status() == 1 {
            continue;
        }
        let Some(class_start) = parse_class_time(&sched.time) else {
            continue;
        };
        let run_at = randomized_run_at(class_start);
        // Skip if run_at is already in the past.
        if run_at < now {
            continue;
        }
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
                student_id: student_id.to_owned(),
                schedule_id: sched.id.clone(),
                course_id: sched.course_id.clone(),
            })
            .await;
    }
}
