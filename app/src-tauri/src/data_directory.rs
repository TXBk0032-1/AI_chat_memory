use std::path::{Path, PathBuf};

use crate::database;

pub async fn prepare_database_directory(
    configured: Option<&Path>,
    executable_dir: Option<&Path>,
    working_dir: Option<&Path>,
    app_data_dir: &Path,
) -> PathBuf {
    let runtime_database = [executable_dir, working_dir]
        .into_iter()
        .flatten()
        .map(|path| path.join("chat_memory.db"))
        .find(|path| path.is_file());

    if let Some(target_dir) = configured {
        let destination = target_dir.join("chat_memory.db");
        if destination.is_file() {
            return target_dir.to_path_buf();
        }
        if let Some(source) = runtime_database.as_deref() {
            if let Err(error) = tokio::fs::create_dir_all(target_dir).await {
                tracing::error!(%error, configured=%target_dir.display(), "failed to create configured directory");
                return source.parent().unwrap_or(app_data_dir).to_path_buf();
            }
            match database::copy_database(source, &destination).await {
                Ok(()) => {
                    tracing::info!(source=%source.display(), destination=%destination.display(), "migrated fallback database to configured directory");
                    return target_dir.to_path_buf();
                }
                Err(error) => {
                    tracing::error!(%error, source=%source.display(), configured=%target_dir.display(), "failed to migrate fallback database to configured directory; using source temporarily");
                    return source.parent().unwrap_or(app_data_dir).to_path_buf();
                }
            }
        }
        match tokio::fs::create_dir_all(target_dir).await {
            Ok(()) => return target_dir.to_path_buf(),
            Err(error) => {
                tracing::error!(%error, configured=%target_dir.display(), "configured data directory is unavailable; using application data directory temporarily")
            }
        }
    } else if let Some(source) = runtime_database {
        return source.parent().unwrap_or(app_data_dir).to_path_buf();
    }
    app_data_dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::prepare_database_directory;

    #[tokio::test]
    async fn migrates_runtime_database_to_missing_configured_directory() {
        let root = std::env::temp_dir().join(format!("acm-path-test-{}", std::process::id()));
        let runtime = root.join("runtime");
        let configured = root.join("configured");
        let app_data = root.join("app-data");
        std::fs::create_dir_all(&runtime).unwrap();
        let source_pool = crate::database::connect(&runtime.join("chat_memory.db"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title) VALUES ('1', 'deepseek', 'source', 'migrated')").execute(&source_pool).await.unwrap();
        source_pool.close().await;
        let resolved =
            prepare_database_directory(Some(&configured), Some(&runtime), None, &app_data).await;
        assert_eq!(resolved, configured);
        let migrated_pool = crate::database::connect(&resolved.join("chat_memory.db"))
            .await
            .unwrap();
        let title: String = sqlx::query_scalar("SELECT title FROM sessions WHERE id = '1'")
            .fetch_one(&migrated_pool)
            .await
            .unwrap();
        assert_eq!(title, "migrated");
        migrated_pool.close().await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn uses_app_data_when_no_existing_runtime_database_is_found() {
        let root = std::env::temp_dir().join(format!("acm-path-empty-{}", std::process::id()));
        let runtime = root.join("runtime");
        let app_data = root.join("app-data");
        std::fs::create_dir_all(&runtime).unwrap();
        let resolved = prepare_database_directory(
            Some(&root.join("missing")),
            Some(&runtime),
            None,
            &app_data,
        )
        .await;
        assert_eq!(resolved, root.join("missing"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn migrates_runtime_database_to_existing_configured_directory_without_db() {
        let root = std::env::temp_dir().join(format!("acm-path-existing-{}", std::process::id()));
        let runtime = root.join("runtime");
        let configured = root.join("existing_configured");
        let app_data = root.join("app-data");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::create_dir_all(&configured).unwrap(); // configured dir exists
        let source_pool = crate::database::connect(&runtime.join("chat_memory.db"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions (id, platform, platform_session_id, title) VALUES ('2', 'deepseek', 'source2', 'migrated2')").execute(&source_pool).await.unwrap();
        source_pool.close().await;
        let resolved =
            prepare_database_directory(Some(&configured), Some(&runtime), None, &app_data).await;
        assert_eq!(resolved, configured);
        assert!(configured.join("chat_memory.db").exists());
        let migrated_pool = crate::database::connect(&resolved.join("chat_memory.db"))
            .await
            .unwrap();
        let title: String = sqlx::query_scalar("SELECT title FROM sessions WHERE id = '2'")
            .fetch_one(&migrated_pool)
            .await
            .unwrap();
        assert_eq!(title, "migrated2");
        migrated_pool.close().await;
        let _ = std::fs::remove_dir_all(root);
    }
}
