use std::{fs, path::Path, time::Duration};
use tracing_subscriber::EnvFilter;

#[cfg(debug_assertions)]
use tracing_subscriber::fmt::writer::MakeWriterExt;

const LOG_RETENTION_DAYS: u64 = 14;

pub struct LogGuard {
    _guard: tracing_appender::non_blocking::WorkerGuard,
}

pub fn init(app_data_dir: &Path) -> std::io::Result<LogGuard> {
    let log_dir = app_data_dir.join("logs");
    fs::create_dir_all(&log_dir)?;
    cleanup_old_logs(&log_dir, LOG_RETENTION_DAYS)?;

    let appender = tracing_appender::rolling::daily(&log_dir, "ai-chat-memory.log");
    let (file_writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,hyper=warn"));

    #[cfg(debug_assertions)]
    let writer = file_writer.and(std::io::stderr);
    #[cfg(not(debug_assertions))]
    let writer = file_writer;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_writer(writer)
        .try_init()
        .map_err(std::io::Error::other)?;

    install_panic_hook();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %log_dir.display(),
        retention_days = LOG_RETENTION_DAYS,
        "logging initialized"
    );
    Ok(LogGuard { _guard: guard })
}

fn cleanup_old_logs(log_dir: &Path, retention_days: u64) -> std::io::Result<()> {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(retention_days * 24 * 60 * 60));
    let Some(cutoff) = cutoff else { return Ok(()) };

    for entry in fs::read_dir(log_dir)? {
        let entry = entry?;
        let path = entry.path();
        let is_app_log = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("ai-chat-memory.log"));
        if is_app_log
            && entry
                .metadata()?
                .modified()
                .is_ok_and(|modified| modified < cutoff)
            && let Err(error) = fs::remove_file(&path)
        {
            eprintln!("failed to remove expired log {}: {error}", path.display());
        }
    }
    Ok(())
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let message = panic_info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| {
                panic_info
                    .payload()
                    .downcast_ref::<String>()
                    .map(String::as_str)
            })
            .unwrap_or("non-string panic payload");
        if let Some(location) = panic_info.location() {
            tracing::error!(
                message,
                file = location.file(),
                line = location.line(),
                "application panic"
            );
        } else {
            tracing::error!(message, "application panic");
        }
        previous(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    use super::cleanup_old_logs;

    #[test]
    fn cleanup_ignores_non_application_files() {
        let root = std::env::temp_dir().join(format!("acm-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("unrelated.log"), "keep").unwrap();
        cleanup_old_logs(&root, 0).unwrap();
        assert!(root.join("unrelated.log").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cleanup_removes_expired_application_logs() {
        let root = std::env::temp_dir().join(format!("acm-log-expiry-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let expired = root.join("ai-chat-memory.log.2000-01-01");
        std::fs::write(&expired, "expired").unwrap();
        cleanup_old_logs(&root, 0).unwrap();
        assert!(!expired.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
