use std::collections::{HashMap, HashSet};

use tracing::debug;

use super::{now_secs, Task, TaskQueue, POST_CLASS_WINDOW_SECS, PRE_CLASS_WINDOW_SECS};

/// Include/exclude course matching policy for one student.
///
/// `include` entries match by course id, schedule id, or exact course name;
/// the wildcard `"*"` matches everything. `exclude` uses the same matching
/// rules and always wins over `include`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CourseFilter {
    all: bool,
    include_ids: HashSet<String>,
    include_names: HashSet<String>,
    exclude_ids: HashSet<String>,
    exclude_names: HashSet<String>,
}

fn split_entries(entries: &[String]) -> (HashSet<String>, HashSet<String>) {
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for entry in entries {
        // "*" is only meaningful as an include wildcard; ignore it elsewhere.
        if entry == "*" {
            continue;
        }
        if entry.chars().all(|c| c.is_ascii_digit()) {
            ids.insert(entry.clone());
        } else {
            names.insert(entry.clone());
        }
    }
    (ids, names)
}

impl CourseFilter {
    pub fn new(include: &[String], exclude: &[String]) -> Self {
        let (include_ids, include_names) = split_entries(include);
        let (exclude_ids, exclude_names) = split_entries(exclude);
        Self {
            all: include.iter().any(|e| e == "*"),
            include_ids,
            include_names,
            exclude_ids,
            exclude_names,
        }
    }

    fn hits(
        ids: &HashSet<String>,
        names: &HashSet<String>,
        course_id: &str,
        sched_id: &str,
        name: &str,
    ) -> bool {
        ids.contains(course_id) || ids.contains(sched_id) || names.contains(name)
    }

    pub fn matches(&self, course_id: &str, sched_id: &str, name: &str) -> bool {
        if Self::hits(
            &self.exclude_ids,
            &self.exclude_names,
            course_id,
            sched_id,
            name,
        ) {
            return false;
        }
        self.all
            || Self::hits(
                &self.include_ids,
                &self.include_names,
                course_id,
                sched_id,
                name,
            )
    }
}

impl From<&crate::config::StudentEntry> for CourseFilter {
    fn from(entry: &crate::config::StudentEntry) -> Self {
        Self::new(&entry.course_ids, &entry.exclude_courses)
    }
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

/// First attempt is at `class_start - 10 min`, or immediately if that
/// moment has already passed but the class window is still open.
pub fn first_run_at(class_start_secs: u64, now: u64) -> u64 {
    class_start_secs
        .saturating_sub(PRE_CLASS_WINDOW_SECS)
        .max(now)
}

/// Enqueue tasks for a single student based on their schedules.
///
/// Enqueues tasks that are still inside the pre-class/post-class window.
pub async fn plan_tasks(
    queue: &TaskQueue,
    student_id: &str,
    schedules: &[crate::client::Schedule],
    filter: &CourseFilter,
) {
    let now = now_secs();

    // A student can only be in one classroom at a time, and the schedule API
    // returns every parallel section sharing that slot (same course with a
    // rotating teacher, or alternative sections of different courses). Plan a
    // single task per start-time slot: the worker tries each member via the
    // official 8081 path — only the section whose teacher opened sign-in can
    // succeed — and the 8082 fallback runs at most once per slot.
    let mut groups: HashMap<String, Vec<&crate::client::Schedule>> = HashMap::new();
    for sched in schedules {
        groups.entry(sched.time.clone()).or_default().push(sched);
    }

    for members in groups.into_values() {
        // Shared sign status: if any member is signed the whole group is done.
        if members.iter().any(|s| s.status() == 1) {
            continue;
        }
        let passing: Vec<&&crate::client::Schedule> = members
            .iter()
            .filter(|s| filter.matches(&s.course_id, &s.id, &s.name))
            .collect();
        if passing.is_empty() {
            continue;
        }
        let representative = passing.iter().map(|s| s.id.as_str()).min().unwrap();
        let representative = *passing.iter().find(|s| s.id == representative).unwrap();
        let Some(class_start) = parse_class_time(&representative.time) else {
            continue;
        };
        let end_unix = passing
            .iter()
            .filter_map(|s| s.end_time.as_deref().and_then(parse_class_time))
            .max();
        let deadline = end_unix
            .map(|e| e + POST_CLASS_WINDOW_SECS)
            .unwrap_or(class_start + POST_CLASS_WINDOW_SECS);
        if now > deadline {
            continue;
        }
        let run_at = first_run_at(class_start, now);
        debug!(
            student = student_id,
            sched_id = %representative.id,
            course_id = %representative.course_id,
            group_len = passing.len(),
            run_at,
            class_start,
            "scheduling task"
        );
        queue
            .push(Task {
                run_at,
                student_id: student_id.to_owned(),
                schedule_id: representative.id.clone(),
                course_id: representative.course_id.clone(),
                group: passing.iter().map(|s| s.id.clone()).collect(),
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn filter(include: &[&str], exclude: &[&str]) -> CourseFilter {
        CourseFilter::new(&strs(include), &strs(exclude))
    }

    fn sched(
        id: &str,
        course_id: &str,
        course_num: &str,
        time: &str,
        status: &str,
    ) -> crate::client::Schedule {
        serde_json::from_value(serde_json::json!({
            "id": id, "courseId": course_id, "courseNum": course_num,
            "courseName": "平行课测试", "teacherName": "T",
            "classBeginTime": time, "signStatus": status
        }))
        .unwrap()
    }

    async fn drain_ready(q: &TaskQueue) -> Vec<Task> {
        q.snapshot_all().await
    }

    #[tokio::test]
    async fn parallel_sections_collapse_into_one_group_task() {
        let q = TaskQueue::new();
        let scheds = vec![
            sched(
                "2531631",
                "100122",
                "D391063001",
                "2100-01-01 08:00:00",
                "0",
            ),
            sched(
                "2531630",
                "100121",
                "D391063001",
                "2100-01-01 08:00:00",
                "0",
            ),
            sched(
                "2557706",
                "102698",
                "D391063001",
                "2100-01-01 08:00:00",
                "0",
            ),
        ];
        plan_tasks(&q, "s1", &scheds, &filter(&["*"], &[])).await;
        let tasks = drain_ready(&q).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].schedule_id, "2531630"); // 最小 id 作代表
        let mut want = vec!["2531630", "2531631", "2557706"];
        let mut got: Vec<&str> = tasks[0].group.iter().map(|s| s.as_str()).collect();
        want.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, want);
    }

    #[tokio::test]
    async fn different_times_stay_separate() {
        let q = TaskQueue::new();
        let scheds = vec![
            sched("111", "1", "AAA", "2100-01-01 08:00:00", "0"),
            sched("222", "2", "BBB", "2100-01-01 10:00:00", "0"),
        ];
        plan_tasks(&q, "s1", &scheds, &filter(&["*"], &[])).await;
        assert_eq!(drain_ready(&q).await.len(), 2);
    }

    #[tokio::test]
    async fn different_courses_same_slot_share_one_task() {
        let q = TaskQueue::new();
        let scheds = vec![
            sched("111", "1", "AAA", "2100-01-01 08:00:00", "0"),
            sched("222", "2", "BBB", "2100-01-01 08:00:00", "0"),
        ];
        plan_tasks(&q, "s1", &scheds, &filter(&["*"], &[])).await;
        let tasks = drain_ready(&q).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].group.len(), 2);
    }

    #[tokio::test]
    async fn signed_member_marks_whole_group_done() {
        let q = TaskQueue::new();
        let scheds = vec![
            sched("111", "1", "AAA", "2100-01-01 08:00:00", "1"),
            sched("112", "2", "AAA", "2100-01-01 08:00:00", "0"),
        ];
        plan_tasks(&q, "s1", &scheds, &filter(&["*"], &[])).await;
        assert!(drain_ready(&q).await.is_empty());
    }

    #[test]
    fn wildcard_matches_everything() {
        let f = filter(&["*"], &[]);
        assert!(f.matches("91999", "555", "任意课程"));
    }

    #[test]
    fn explicit_include_matches_id_and_name() {
        let f = filter(&["91999", "高等数学"], &[]);
        assert!(f.matches("91999", "555", "其他名字"));
        assert!(f.matches("88888", "556", "高等数学"));
        assert!(!f.matches("88888", "556", "其他名字"));
    }

    #[test]
    fn schedule_id_also_matches() {
        let f = filter(&["555"], &[]);
        assert!(f.matches("91999", "555", "任意课程"));
    }

    #[test]
    fn exclude_overrides_wildcard() {
        let f = filter(&["*"], &["体育"]);
        assert!(f.matches("88888", "556", "高等数学"));
        assert!(!f.matches("88888", "556", "体育"));
    }

    #[test]
    fn exclude_by_course_id() {
        let f = filter(&["*"], &["91999"]);
        assert!(!f.matches("91999", "555", "高等数学"));
        assert!(f.matches("88888", "556", "高等数学"));
    }

    #[test]
    fn exclude_overrides_explicit_include() {
        let f = filter(&["91999"], &["91999"]);
        assert!(!f.matches("91999", "555", "高等数学"));
    }

    #[test]
    fn empty_filter_matches_nothing() {
        let f = CourseFilter::default();
        assert!(!f.matches("91999", "555", "高等数学"));
    }

    #[test]
    fn first_run_at_uses_preclass_window_or_now() {
        assert_eq!(first_run_at(1_000, 0), 400);
        assert_eq!(first_run_at(1_000, 500), 500);
        assert_eq!(first_run_at(1_000, 1_500), 1_500);
    }
}
