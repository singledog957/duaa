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
    pub client: Arc<client::ClassClient>,
    pub cache: Arc<pipeline::SchedulerCache>,
    pub queue: Arc<pipeline::TaskQueue>,
}

fn parse_config_path() -> String {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" {
            if let Some(path) = args.next() {
                return path;
            }
            eprintln!("missing value for --config");
            std::process::exit(1);
        }
    }
    "config.json".to_string()
}

fn init_logging(log_file: Option<&str>) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let env = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "duaa=info".into());

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
    let config_path = parse_config_path();
    let cfg = Arc::new(config::Config::load(&config_path));
    let _log_guard = init_logging(cfg.log_file.as_deref());

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
        cache.set(student.student_id.clone(), student.course_ids.clone());
        info!(student = %student.student_id, name = %student.name, "loaded student config");
    }

    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        client,
        cache,
        queue,
    });

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
