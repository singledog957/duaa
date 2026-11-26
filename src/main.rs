mod client;
mod config;
mod error;
mod pipeline;

use std::sync::Arc;
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub struct AppState {
    pub cfg: Arc<config::Config>,
    pub config_path: String,
    pub client: Arc<client::ClassClient>,
    pub cache: Arc<pipeline::SchedulerCache>,
    pub queue: Arc<pipeline::TaskQueue>,
}

fn parse_args() -> (String, bool) {
    let mut config_path = "config.json".to_string();
    let mut status_mode = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                if let Some(path) = args.next() {
                    config_path = path;
                } else {
                    eprintln!("missing value for --config");
                    std::process::exit(1);
                }
            }
            "--status" => status_mode = true,
            _ => {}
        }
    }
    (config_path, status_mode)
}

/// One-shot mode: print today's real sign status for every configured student.
async fn run_status(state: Arc<AppState>) {
    let today = {
        use time::macros::offset;
        use time::OffsetDateTime;
        let now = OffsetDateTime::now_utc().to_offset(offset!(+8));
        format!("{:04}{:02}{:02}", now.year(), now.month() as u8, now.day())
    };

    for student in &state.cfg.students {
        match state
            .client
            .query_schedule_force(&student.student_id, &today)
            .await
        {
            Ok(schedules) if schedules.is_empty() => {
                println!("[{} {}] 今日无课", student.student_id, student.name);
            }
            Ok(schedules) => {
                for s in &schedules {
                    let mark = if s.status() == 1 {
                        "✅ 已签"
                    } else {
                        "⬜ 未签"
                    };
                    println!(
                        "[{} {}] {} {} {} (courseId={} code={} schedId={})",
                        student.student_id,
                        student.name,
                        mark,
                        s.time,
                        s.name,
                        s.course_id,
                        s.course_num,
                        s.id
                    );
                }
            }
            Err(e) => {
                println!("[{} {}] 查询失败: {e}", student.student_id, student.name);
            }
        }
    }
}

fn init_logging(
    log_file: Option<&str>,
    quiet: bool,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let env = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        if quiet {
            "duaa=warn".into()
        } else {
            "duaa=info".into()
        }
    });

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_writer(std::io::stdout);

    if let Some(path) = log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap_or_else(|e| {
                eprintln!("failed to open log file {path}: {e}");
                std::process::exit(1);
            });
        let (file_writer, guard) = tracing_appender::non_blocking(file);
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(file_writer);

        tracing_subscriber::registry()
            .with(env)
            .with(stdout_layer)
            .with(file_layer)
            .init();
        Some(guard)
    } else {
        tracing_subscriber::registry()
            .with(env)
            .with(stdout_layer)
            .init();
        None
    }
}

#[tokio::main]
async fn main() {
    let (config_path, status_mode) = parse_args();
    let cfg = Arc::new(config::Config::load(&config_path));
    let _log_guard = init_logging(cfg.log_file.as_deref(), status_mode);

    info!(
        config = %config_path,
        students = cfg.students.len(),
        poll_interval = cfg.poll_interval_minutes,
        auto_window = cfg.auto_window_minutes,
        "starting duaa"
    );

    let client = Arc::new(client::ClassClient::new(&cfg.students));
    let cache = Arc::new(pipeline::SchedulerCache::new());
    let queue = Arc::new(pipeline::TaskQueue::new());

    for student in &cfg.students {
        cache.set(
            student.student_id.clone(),
            pipeline::scheduler::CourseFilter::from(student),
        );
        info!(student = %student.student_id, name = %student.name, "loaded student config");
    }

    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        config_path: config_path.clone(),
        client,
        cache,
        queue,
    });

    if status_mode {
        run_status(state).await;
        return;
    }

    let poller_state = state.clone();
    tokio::spawn(async move {
        pipeline::poller::run(poller_state).await;
    });

    let worker_state = state.clone();
    tokio::spawn(async move {
        pipeline::worker::run(worker_state).await;
    });

    info!("duaa running, press Ctrl-C to stop");
    if let Err(e) = tokio::signal::ctrl_c().await {
        eprintln!("failed to listen for ctrl-c: {e}");
        std::process::exit(1);
    }
    info!("shutdown signal received, exiting");
}
