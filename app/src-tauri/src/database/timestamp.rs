const TIMESTAMP_SQL: &str = "CASE WHEN trim({column}) <> '' AND trim({column}) NOT GLOB '*[^0-9.]*' THEN ROUND(CASE WHEN CAST({column} AS REAL) > 100000000000 THEN CAST({column} AS REAL) / 1000.0 ELSE CAST({column} AS REAL) END, 3) ELSE ROUND((julianday({column}) - 2440587.5) * 86400.0, 3) END";

pub fn expression(column: &str) -> String {
    TIMESTAMP_SQL.replace("{column}", column)
}
