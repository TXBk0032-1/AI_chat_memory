//! Codex 数据目录的实时监听：用 `notify` v8 递归监听 `<codex_home>/sessions`
//! 与 `<codex_home>/archived_sessions`，涉及 `rollout-*.jsonl` 的文件事件经
//! `tokio::sync::mpsc` 转发，500ms 防抖聚合后只触发一次 `Service::import_codex()`
//! 整目录重扫。重扫由 upsert 幂等保证不会重复入库，是最稳妥的增量方案：
//! 无需自行实现单文件增量，`import_codex` 已自带工作 JSONL 落盘、语义索引与
//! `notify_local_sync()` 前端刷新。
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::service::AppService;

/// 一个静默窗口的防抖时长：收到首个事件后若 500ms 内无新事件即触发一次导入。
const DEBOUNCE: Duration = Duration::from_millis(500);

/// 判断路径是否为 Codex rollout 文件（`rollout-*.jsonl`），过滤掉无关变更。
fn is_rollout_path(path: &Path) -> bool {
    let is_jsonl = path.extension().and_then(|e| e.to_str()) == Some("jsonl");
    let is_rollout = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("rollout-"))
        .unwrap_or(false);
    is_jsonl && is_rollout
}

/// 默认 Codex 数据根：设置里显式配置优先，否则 %USERPROFILE%\.codex（回退 HOME）。
/// 与 `Service::resolve_codex_home` 同源，避免为暴露内部方法而扩大 service.rs 改动面。
fn resolve_codex_home(settings: &crate::models::AppSettings) -> Option<PathBuf> {
    if let Some(dir) = settings
        .codex
        .codex_home
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .map(|home| home.join(".codex"))
}

/// 拉起一个常驻 async 任务：任务持有 `notify` watcher 句柄（句柄必须保持存活，
/// 否则监听立即停止），循环接收防抖后的事件并调用整扫导入。启动失败仅告警。
pub fn spawn(service: AppService) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = run(service).await {
            tracing::warn!(%error, "codex 实时监听启动失败");
        }
    });
}

async fn run(service: AppService) -> crate::error::Result<()> {
    let settings = service.settings().await;
    let Some(codex_home) = resolve_codex_home(&settings) else {
        tracing::info!("无法定位 Codex 数据目录，跳过实时监听");
        return Ok(());
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    // notify 回调运行在其后台线程（同步上下文）：只把「有相关变更」的信号丢进通道，
    // 真正的导入放到本 async 任务里做，避免在回调里阻塞后台监听线程。
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && event.paths.iter().any(|p| is_rollout_path(p))
        {
            let _ = tx.send(());
        }
    })
    .map_err(|e| crate::error::AppError::Configuration(e.to_string()))?;

    let mut watched = 0usize;
    for sub in ["sessions", "archived_sessions"] {
        let dir = codex_home.join(sub);
        if !dir.is_dir() {
            continue; // 子目录缺失时跳过、不报错
        }
        match watcher.watch(&dir, RecursiveMode::Recursive) {
            Ok(()) => watched += 1,
            Err(error) => tracing::warn!(%error, dir = %dir.display(), "codex 目录监听注册失败"),
        }
    }
    if watched == 0 {
        tracing::info!("codex 数据目录不存在，跳过实时监听");
        return Ok(());
    }
    tracing::info!(watched, "codex 实时监听已启动");

    // 防抖循环：收到首个信号后进入静默窗口，窗口内的新信号重置计时，
    // 一旦静默满 DEBOUNCE 就触发一次整目录导入（幂等）。watcher 句柄由本
    // 函数作用域持有，只要循环不结束就保持存活，监听不会中断。
    while rx.recv().await.is_some() {
        loop {
            match tokio::time::timeout(DEBOUNCE, rx.recv()).await {
                Ok(Some(())) => continue,  // 窗口内又有变更：重置静默计时
                Ok(None) => return Ok(()), // 通道关闭（watcher 已释放）
                Err(_) => break,           // 静默满 DEBOUNCE：触发导入
            }
        }
        match service.import_codex().await {
            Ok(resp) => tracing::info!(
                imported = resp.imported,
                updated = resp.updated,
                "codex 实时增量导入完成"
            ),
            Err(error) => tracing::warn!(%error, "codex 实时增量导入失败"),
        }
    }
    Ok(())
}
