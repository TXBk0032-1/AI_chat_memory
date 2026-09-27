//! OpenAI Codex rollout 解析：把磁盘 rollout JSONL 转成可入库的
//! NormalizedSession（用户消息 + 最终回答 + 过程说明），其余过程性内容
//! （思考、工具调用、diff、子代理、计划）产出 WorkItem 写入按会话的
//! 工作 JSONL。分页窗口按 thread_id 合并、按 ordinal 去重，子代理继承前缀
//! （ordinal < subagent_history_start_ordinal）跳过。
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::models::{NormalizedMessage, NormalizedSession, WorkItem};

/// 单条工作项正文的截断上限（UTF-8 字符边界安全）。
pub(crate) const WORK_ITEM_MAX_BYTES: usize = 130 * 1024;

/// 解析一个线程（可能由多个窗口文件合并）后的产物。
#[derive(Debug, Clone)]
pub struct CodexThread {
    pub session: NormalizedSession,
    pub work_items: Vec<WorkItem>,
    pub parent_platform_session_id: Option<String>,
}

/// rollout 单行。
#[derive(Debug, Clone)]
struct RolloutLine {
    ordinal: Option<i64>,
    kind: String,
    timestamp: Option<String>,
    payload: Value,
}

/// 按 UTF-8 字符边界把正文截断到 WORK_ITEM_MAX_BYTES，返回 (截断后文本, 是否截断)。
pub(crate) fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

#[cfg(test)]
#[path = "codex/tests.rs"]
mod tests;
