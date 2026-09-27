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
