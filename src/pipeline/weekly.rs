use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use time::{macros::offset, Date, OffsetDateTime, Weekday};
use tracing::{info, warn};

use super::scheduler;
use crate::AppState;

fn dates_to_plan(now: OffsetDateTime) -> Vec<Date> {
    let today = now.date();
    let until_sunday = 6 - today.weekday().number_days_from_monday() as i64;
    let mut dates: Vec<Date> = (0..=until_sunday)
        .map(|days| today + time::Duration::days(days))
        .collect();
    if today.weekday() == Weekday::Sunday && now.hour() >= 20 {
        dates.extend((1..=7).map(|days| today + time::Duration::days(days)));
    }
    dates
}

fn date_string(date: Date) -> String {
    format!(
        "{:04}{:02}{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
}

/// Refresh each day of the current week once. Sunday evening also plans next week.
/// Failed days are retried on the next tick; a restart rebuilds the in-memory plan.
pub async fn run(state: Arc<AppState>) {
    let mut planned = HashSet::<(String, String)>::new();
    let mut failed_at = HashMap::<(String, String), Instant>::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(10 * 60));
    loop {
        ticker.tick().await;
        let now = OffsetDateTime::now_utc().to_offset(offset!(+8));
        let dates = dates_to_plan(now);
        for student in state
            .cfg
            .students
            .iter()
            .filter(|s| s.auto_include_new_courses)
        {
            for date in &dates {
                let day = date_string(*date);
                let key = (student.student_id.clone(), day.clone());
                if planned.contains(&key) {
                    continue;
                }
                if failed_at
                    .get(&key)
                    .is_some_and(|at| at.elapsed() < Duration::from_secs(30 * 60))
                {
                    continue;
                }
                match state
                    .client
                    .refresh_schedule(&student.student_id, &day)
                    .await
                {
                    Ok(schedules) => {
                        scheduler::plan_tasks(
                            &state.queue,
                            &student.student_id,
                            &schedules,
                            &[],
                            0,
                            true,
                        )
                        .await;
                        failed_at.remove(&key);
                        planned.insert(key);
                        info!(student = %student.student_id, date = %day, "weekly day planned");
                    }
                    Err(err) => {
                        failed_at.insert(key, Instant::now());
                        warn!(student = %student.student_id, date = %day, err = %err, "weekly day failed; retry later");
                    }
                }
            }
        }
        let oldest = date_string(now.date() - time::Duration::days(7));
        planned.retain(|(_, day)| day >= &oldest);
        failed_at.retain(|(_, day), _| day >= &oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_week_is_added_sunday_evening() {
        let morning = time::macros::datetime!(2026-09-27 10:00 +08:00);
        let evening = time::macros::datetime!(2026-09-27 20:00 +08:00);
        assert_eq!(dates_to_plan(morning).len(), 1);
        assert_eq!(dates_to_plan(evening).len(), 8);
    }
}
