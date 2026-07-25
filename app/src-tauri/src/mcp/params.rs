/// Clamp MCP tool limit: None -> default, then bound to [1, max].
#[allow(dead_code)] // wired by later MCP tool handlers
pub fn clamp_limit(value: Option<i64>, default: i64, max: i64) -> i64 {
    value.unwrap_or(default).clamp(1, max)
}

/// Clamp MCP tool offset: None -> 0, never negative.
#[allow(dead_code)] // wired by later MCP tool handlers
pub fn clamp_offset(value: Option<i64>) -> i64 {
    value.unwrap_or(0).max(0)
}

/// Trim and require a non-empty query string.
#[allow(dead_code)] // wired by later MCP tool handlers
pub fn normalize_required_query(q: &str) -> Result<String, String> {
    let t = q.trim();
    if t.is_empty() {
        Err("查询词不能为空".into())
    } else {
        Ok(t.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_search_limit() {
        assert_eq!(clamp_limit(None, 20, 100), 20);
        assert_eq!(clamp_limit(Some(0), 20, 100), 1);
        assert_eq!(clamp_limit(Some(500), 20, 100), 100);
    }

    #[test]
    fn clamps_search_offset() {
        assert_eq!(clamp_offset(None), 0);
        assert_eq!(clamp_offset(Some(-3)), 0);
        assert_eq!(clamp_offset(Some(12)), 12);
    }

    #[test]
    fn rejects_empty_in_session_query() {
        assert!(normalize_required_query("  ").is_err());
        assert_eq!(normalize_required_query("foo").unwrap(), "foo");
        assert_eq!(normalize_required_query("  bar  ").unwrap(), "bar");
    }
}
