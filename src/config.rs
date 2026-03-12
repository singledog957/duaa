use serde::Deserialize;
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
    pub course_ids: Vec<String>,
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
    course_ids: Option<Vec<String>>,
}

fn default_poll() -> u64 {
    10
}

fn default_window() -> u64 {
    15
}

impl Config {
    pub fn load<P: AsRef<Path>>(path: P) -> Self {
        let path_ref = path.as_ref();
        let raw_text = match std::fs::read_to_string(path_ref) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("config read error at {}: {e}", path_ref.display());
                std::process::exit(1);
            }
        };

        let raw_cfg: RawConfig = match serde_json::from_str(&raw_text) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("config parse error at {}: {e}", path_ref.display());
                std::process::exit(1);
            }
        };

        let mut students = Vec::new();
        for (idx, s) in raw_cfg.students.into_iter().enumerate() {
            let Some(student_id) = s.student_id else {
                warn!(index = idx, "skip student entry: missing student_id");
                continue;
            };
            let Some(course_ids) = s.course_ids else {
                warn!(student = %student_id, "skip student entry: missing course_ids");
                continue;
            };
            if course_ids.is_empty() {
                warn!(student = %student_id, "skip student entry: empty course_ids");
                continue;
            }

            let name = s.name.unwrap_or_else(|| student_id.clone());
            students.push(StudentEntry {
                student_id,
                name,
                course_ids,
            });
        }

        Config {
            poll_interval_minutes: raw_cfg.poll_interval_minutes,
            auto_window_minutes: raw_cfg.auto_window_minutes,
            log_file: raw_cfg.log_file,
            students,
        }
    }
}
