use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct Config {
    pub poll_interval_minutes: u64,
    pub auto_window_minutes: u64,
    pub log_file: Option<String>,
    pub students: Vec<StudentEntry>,
}

#[derive(Debug, Clone)]
pub struct StudentEntry {
    pub student_id: String,
    pub name: String,
    pub sso_password: String,
    /// Optional per-account secret-provider URL (wwlocal container).
    /// When set, checkin signs via the official 8081 entry; on provider
    /// failure it falls back to the legacy 8082 path.
    pub secret_url: Option<String>,
    pub course_ids: Vec<String>,
    pub exclude_courses: Vec<String>,
    pub auto_include_new_courses: bool,
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default = "default_poll")]
    poll_interval_minutes: u64,
    #[serde(default = "default_window")]
    auto_window_minutes: u64,
    log_file: Option<String>,
    #[serde(default)]
    students: Vec<RawStudentEntry>,
}

#[derive(Debug, Deserialize)]
struct RawStudentEntry {
    student_id: Option<String>,
    name: Option<String>,
    sso_password: Option<String>,
    password: Option<String>,
    secret_url: Option<String>,
    course_ids: Option<Vec<String>>,
    #[serde(default)]
    exclude_courses: Option<Vec<String>>,
    #[serde(default)]
    auto_include_new_courses: bool,
}

fn default_poll() -> u64 {
    10
}

fn default_window() -> u64 {
    15
}

impl Config {
    pub fn load<P: AsRef<Path>>(path: P) -> Self {
        match Self::try_load(path) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }

    /// Non-fatal variant used by the hot-reload path: returns Err instead of exiting,
    /// so a broken config.json keeps the previous runtime state alive.
    pub fn try_load<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path_ref = path.as_ref();
        let raw_text = std::fs::read_to_string(path_ref)
            .map_err(|e| format!("config read error at {}: {e}", path_ref.display()))?;

        let raw_cfg: RawConfig = serde_json::from_str(&raw_text)
            .map_err(|e| format!("config parse error at {}: {e}", path_ref.display()))?;

        let mut students = Vec::new();
        let mut seen_ids = HashSet::new();
        for (idx, s) in raw_cfg.students.into_iter().enumerate() {
            let Some(student_id) = s.student_id else {
                warn!(index = idx, "skip student entry: missing student_id");
                continue;
            };
            let course_ids = s.course_ids.unwrap_or_default();
            if course_ids.is_empty() && !s.auto_include_new_courses {
                warn!(student = %student_id, "skip student entry: empty course_ids");
                continue;
            }
            let student_id = student_id.trim().to_ascii_lowercase();
            let Some(sso_password) = s.sso_password.or(s.password) else {
                warn!(student = %student_id, "skip student entry: missing sso_password");
                continue;
            };
            if sso_password.trim().is_empty() {
                warn!(student = %student_id, "skip student entry: empty sso_password");
                continue;
            }
            if student_id.is_empty() || !seen_ids.insert(student_id.clone()) {
                warn!(student = %student_id, "skip student entry: empty or duplicate student_id");
                continue;
            }

            let name = s.name.unwrap_or_else(|| student_id.clone());
            students.push(StudentEntry {
                student_id,
                name,
                sso_password,
                secret_url: s.secret_url,
                course_ids,
                exclude_courses: s.exclude_courses.unwrap_or_default(),
                auto_include_new_courses: s.auto_include_new_courses,
            });
        }

        Ok(Config {
            poll_interval_minutes: raw_cfg.poll_interval_minutes,
            auto_window_minutes: raw_cfg.auto_window_minutes,
            log_file: raw_cfg.log_file,
            students,
        })
    }
}
