use chrono::{Local, LocalResult, NaiveDate, NaiveDateTime, TimeDelta, TimeZone};

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

/// Normalize a MCP date filter to Unix seconds using the local timezone.
pub fn normalize_optional_search_date(
    value: Option<&str>,
    end_of_day: bool,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }

    if value.len() == 10 && value.as_bytes()[4] == b'-' && value.as_bytes()[7] == b'-' {
        let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| invalid_date_parameter(value))?;
        if date.format("%Y-%m-%d").to_string() != value {
            return Err(invalid_date_parameter(value));
        }
        let timestamp = resolve_local_date_boundary_with(date, end_of_day, |local| {
            Local.from_local_datetime(&local)
        })
        .ok_or_else(|| invalid_date_parameter(value))?
        .timestamp();
        return Ok(Some(timestamp.to_string()));
    }

    match value.parse::<f64>() {
        Ok(timestamp) if timestamp.is_finite() => Ok(Some(value.to_string())),
        _ => Err(invalid_date_parameter(value)),
    }
}

fn resolve_local_date_boundary_with<T, F>(
    date: NaiveDate,
    end_of_day: bool,
    mut resolve: F,
) -> Option<T>
where
    F: FnMut(NaiveDateTime) -> LocalResult<T>,
{
    const LAST_SECOND: i64 = 86_399;

    let midnight = date
        .and_hms_opt(0, 0, 0)
        .expect("valid date has a midnight");
    let boundary_second = if end_of_day { LAST_SECOND } else { 0 };
    let boundary = midnight + TimeDelta::seconds(boundary_second);
    if let Some(value) = select_local_boundary(resolve(boundary), end_of_day) {
        return Some(value);
    }

    if end_of_day {
        for second in (0..LAST_SECOND).rev() {
            if let Some(value) =
                select_local_boundary(resolve(midnight + TimeDelta::seconds(second)), true)
            {
                return Some(value);
            }
        }
    } else {
        for second in 1..=LAST_SECOND {
            if let Some(value) =
                select_local_boundary(resolve(midnight + TimeDelta::seconds(second)), false)
            {
                return Some(value);
            }
        }
    }

    None
}

fn select_local_boundary<T>(result: LocalResult<T>, end_of_day: bool) -> Option<T> {
    match result {
        LocalResult::Single(value) => Some(value),
        LocalResult::Ambiguous(earliest, latest) => {
            Some(if end_of_day { latest } else { earliest })
        }
        LocalResult::None => None,
    }
}

fn invalid_date_parameter(value: &str) -> String {
    format!("日期参数无效「{value}」，请使用 YYYY-MM-DD 或有限 Unix 秒")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, LocalResult, TimeZone};
    use std::cell::Cell;

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

    #[test]
    fn normalizes_date_from_to_local_start_of_day() {
        let expected = Local
            .with_ymd_and_hms(2024, 2, 29, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp()
            .to_string();

        assert_eq!(
            normalize_optional_search_date(Some("2024-02-29"), false).unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn normalizes_date_to_to_local_end_of_day() {
        let expected = Local
            .with_ymd_and_hms(2024, 2, 29, 23, 59, 59)
            .single()
            .unwrap()
            .timestamp()
            .to_string();

        assert_eq!(
            normalize_optional_search_date(Some("2024-02-29"), true).unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn preserves_trimmed_finite_unix_seconds_and_omits_whitespace() {
        assert_eq!(
            normalize_optional_search_date(Some("  1710000000.5  "), false).unwrap(),
            Some("1710000000.5".into())
        );
        assert_eq!(
            normalize_optional_search_date(Some(" \t "), false).unwrap(),
            None
        );
    }

    #[test]
    fn rejects_invalid_calendar_dates_and_non_finite_or_other_values() {
        for value in ["2024-02-30", "NaN", "inf", "2024/02/29"] {
            let err = normalize_optional_search_date(Some(value), false).unwrap_err();
            assert!(err.contains("日期"), "{err}");
        }
    }

    #[test]
    fn ambiguous_boundary_chooses_earliest_for_start_and_latest_for_end() {
        let date = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();

        assert_eq!(
            resolve_local_date_boundary_with(date, false, |_| LocalResult::Ambiguous(10, 20)),
            Some(10)
        );
        assert_eq!(
            resolve_local_date_boundary_with(date, true, |_| LocalResult::Ambiguous(10, 20)),
            Some(20)
        );
    }

    #[test]
    fn nonexistent_boundary_scans_inward_to_first_valid_second() {
        let date = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let midnight = date.and_hms_opt(0, 0, 0).unwrap();
        let start_calls = Cell::new(0);
        let start = resolve_local_date_boundary_with(date, false, |local| {
            start_calls.set(start_calls.get() + 1);
            let second = local.signed_duration_since(midnight).num_seconds();
            if second < 2 {
                LocalResult::None
            } else {
                LocalResult::Single(second)
            }
        });
        assert_eq!(start, Some(2));
        assert_eq!(start_calls.get(), 3);

        let end_calls = Cell::new(0);
        let end = resolve_local_date_boundary_with(date, true, |local| {
            end_calls.set(end_calls.get() + 1);
            let second = local.signed_duration_since(midnight).num_seconds();
            if second > 86_397 {
                LocalResult::None
            } else {
                LocalResult::Single(second)
            }
        });
        assert_eq!(end, Some(86_397));
        assert_eq!(end_calls.get(), 3);
    }
}
