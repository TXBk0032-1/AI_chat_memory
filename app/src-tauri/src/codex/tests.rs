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
