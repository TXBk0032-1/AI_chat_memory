use super::*;

#[test]
fn truncate_respects_char_boundary() {
    // 每个中文字符占 3 字节，取上限 4 字节应回退到 3 字节边界（1 个字符）。
    let s = "汉字漢字";
    let (out, truncated) = truncate_on_char_boundary(s, 4);
    assert!(truncated);
    assert_eq!(out, "汉");
    assert!(out.len() <= 4);
}

#[test]
fn truncate_keeps_short_text() {
    let (out, truncated) = truncate_on_char_boundary("hello", 130 * 1024);
    assert!(!truncated);
    assert_eq!(out, "hello");
}

fn line(json: &str) -> Value {
    serde_json::from_str(json).unwrap()
}

#[test]
fn parses_user_message_and_final_answer_into_session() {
    let lines = vec![
        line(r#"{"type":"session_meta","payload":{"id":"T-1","cwd":"/home/u/my-proj","timestamp":"2026-09-01T10:00:00Z"}}"#),
        line(r#"{"ordinal":1,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","text":"你好"}}}"#),
        line(r#"{"ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","phase":"final_answer","text":"你好，我能帮忙"}}}"#),
        line(r#"{"ordinal":3,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","phase":"commentary","text":"我先看看文件"}}}"#),
    ];
    let thread = parse_lines("T-1", &lines, None).unwrap();
    assert_eq!(thread.session.platform, "codex");
    assert_eq!(thread.session.platform_session_id, "T-1");
    assert_eq!(thread.session.project.as_deref(), Some("my-proj"));
    // 用户消息 + 最终回答 + commentary 都进 DB messages
    let roles: Vec<&str> = thread.session.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["user", "assistant", "assistant"]);
    // commentary 也生成一个 work item（供工作栏时间线展示）
    assert!(thread.work_items.iter().any(|w| w.kind == "commentary"));
}

#[test]
fn reasoning_and_command_go_to_work_items_only() {
    let lines = vec![
        line(r#"{"type":"session_meta","payload":{"id":"T-2","cwd":"/x/proj"}}"#),
        line(r#"{"ordinal":1,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","text":"跑测试"}}}"#),
        line(r#"{"ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning","summary_text":["计划"],"raw_content":["细节明文"]}}}"#),
        line(r#"{"ordinal":3,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":"cargo test","aggregated_output":"ok"}}}"#),
    ];
    let thread = parse_lines("T-2", &lines, None).unwrap();
    // 只有用户消息进 DB
    assert_eq!(thread.session.messages.len(), 1);
    assert!(thread.work_items.iter().any(|w| w.kind == "reasoning"));
    assert!(thread.work_items.iter().any(|w| w.kind == "command" && w.title.contains("cargo test")));
}

use std::io::Write;

fn write_rollout(dir: &std::path::Path, name: &str, lines: &[&str]) -> PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
    path
}

#[test]
fn merges_paginated_windows_by_thread_id() {
    let tmp = std::env::temp_dir().join(format!("codex-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    // 窗口 1：ordinal 1,2
    write_rollout(&tmp, "rollout-a_0.jsonl", &[
        r#"{"type":"session_meta","payload":{"id":"T-9","cwd":"/p/proj","history_mode":"paginated"}}"#,
        r#"{"ordinal":1,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","text":"问题一"}}}"#,
        r#"{"ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","phase":"final_answer","text":"答一"}}}"#,
    ]);
    // 窗口 2：ordinal 2（重叠，应去重）,3
    write_rollout(&tmp, "rollout-a_1.jsonl", &[
        r#"{"type":"session_meta","payload":{"id":"T-9","cwd":"/p/proj","history_mode":"paginated","history_base":{"thread_id":"T-9","end_ordinal_exclusive":2}}}"#,
        r#"{"ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","phase":"final_answer","text":"答一"}}}"#,
        r#"{"ordinal":3,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","text":"问题二"}}}"#,
    ]);
    let threads = scan_dir(&tmp).unwrap();
    std::fs::remove_dir_all(&tmp).ok();
    assert_eq!(threads.len(), 1);
    let t = &threads[0];
    assert_eq!(t.session.platform_session_id, "T-9");
    // 去重后：user 问题一 / assistant 答一 / user 问题二 —— 只有一个"答一"
    let contents: Vec<&str> = t.session.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["问题一", "答一", "问题二"]);
}

#[tokio::test]
async fn codex_watch_incremental_import_is_idempotent() {
    // 实时监听最终复用的导入链：scan_dir 解析目录 -> import_local_sessions 幂等入库。
    // 简报步骤 1 引用的 import_codex_file(single_file) 单文件导入函数在代码库中并不存在
    // （见任务简报「关键代码事实」），因此在真实可复用、构造成本合理的这一层验证幂等：
    // 首次解析出 1 个线程且入库 1 行；对同一数据二次导入后 sessions 表仍只有 1 行（upsert 幂等）。
    let tmp = std::env::temp_dir().join(format!("codex-watch-{}", uuid::Uuid::new_v4()));
    let sessions_dir = tmp.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let thread_id = uuid::Uuid::new_v4().to_string();
    let meta = format!(r#"{{"type":"session_meta","payload":{{"id":"{thread_id}","cwd":"/w/proj"}}}}"#);
    write_rollout(
        &sessions_dir,
        "rollout-2026-01-01T00-00-00.jsonl",
        &[
            meta.as_str(),
            r#"{"ordinal":1,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","text":"你好"}}}"#,
            r#"{"ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","phase":"final_answer","text":"回答"}}}"#,
        ],
    );

    let threads = scan_dir(&sessions_dir).unwrap();
    assert_eq!(threads.len(), 1, "应解析出恰好 1 个 codex 线程");
    let normalized: Vec<NormalizedSession> = threads.iter().map(|t| t.session.clone()).collect();

    // register_sqlite_vec() 由 database::connect 内部调用；不用 #[sqlx::test]（其注入的 pool 缺 sqlite-vec）。
    let db_path = tmp.join("chat_memory.db");
    let pool = crate::database::connect(&db_path).await.unwrap();

    let imported = crate::service::import_local_sessions(&pool, &normalized)
        .await
        .unwrap();
    assert_eq!(imported, 1, "首次导入应处理 1 个会话");
    let after_first: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after_first, 1, "首次导入后 sessions 应有 1 行");

    // 二次导入同一数据：upsert 幂等，不应新增行。
    crate::service::import_local_sessions(&pool, &normalized)
        .await
        .unwrap();
    let after_second: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after_second, 1, "二次导入应保持 1 行（幂等 upsert，不重复计入）");

    pool.close().await;
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn work_items_roundtrip_through_jsonl() {
    let tmp = std::env::temp_dir().join(format!("codex-work-{}", uuid::Uuid::new_v4()));
    let items = vec![WorkItem {
        work_seq: 0, seq: 0, kind: "command".into(), title: "$ ls".into(),
        body: Some("out".into()), expandable: true, truncated: false,
        agent_thread_id: None, agent_label: None,
    }];
    write_work_items(&tmp, "T-x", &items).unwrap();
    let back = read_work_items(&tmp, "T-x");
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].title, "$ ls");
    delete_work_items(&tmp, "T-x");
    assert!(read_work_items(&tmp, "T-x").is_empty());
    std::fs::remove_dir_all(&tmp).ok();
}
