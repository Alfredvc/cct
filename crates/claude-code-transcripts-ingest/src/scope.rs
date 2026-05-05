//! Shared scope (cwd + date) filter parsing for `report` / `extract` subcommands.
//!
//! Both subcommands accept the same `--project / --all / --no-subdirs / --from / --to`
//! flags and translate them into the same predicate against `entries.cwd` and
//! `entries.timestamp`. Centralized here so the two paths can't drift.

use std::path::{Path, PathBuf};

/// Resolved scope ready to be folded into a SQL `WHERE` clause.
///
/// `cwd_exact` is the canonical project directory (or `None` for `--all`);
/// `match_subdirs` is true when worktree / subdir cwds (`<exact>/...`) should
/// also match.
#[derive(Debug, Clone)]
pub struct Scope {
    pub cwd_exact: Option<String>,
    pub match_subdirs: bool,
    pub from_iso: Option<String>,
    /// Inclusive — the CLI accepts `--to YYYY-MM-DD` and we expand to end-of-day.
    pub to_iso: Option<String>,
}

impl Scope {
    pub fn resolve(
        all: bool,
        project: Option<&Path>,
        no_subdirs: bool,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Self, String> {
        let cwd_exact = if all {
            None
        } else {
            let raw = project
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .ok_or_else(|| "could not determine project directory".to_string())?;
            // Canonicalize so symlinked / `~` / relative paths still match the
            // value Claude Code wrote into `entries.cwd`. Fall back to the raw
            // path if canonicalize fails (e.g. directory deleted) — comparison
            // will then just miss those rows.
            let canon = std::fs::canonicalize(&raw).unwrap_or(raw);
            Some(canon.to_string_lossy().into_owned())
        };

        let from_iso = match from {
            None => None,
            Some(s) => Some(parse_date(s, false)?),
        };
        let to_iso = match to {
            None => None,
            Some(s) => Some(parse_date(s, true)?),
        };

        Ok(Self {
            cwd_exact,
            match_subdirs: !no_subdirs && !all,
            from_iso,
            to_iso,
        })
    }

    /// Append `AND` predicates onto an existing WHERE clause (caller already
    /// emitted `WHERE 1=1` or similar). Pushes literal-string parameters into
    /// `params` for binding.
    pub fn append_where(&self, sql: &mut String, params: &mut Vec<String>) {
        if let Some(cwd) = &self.cwd_exact {
            if self.match_subdirs {
                sql.push_str(" AND (e.cwd = ? OR e.cwd LIKE ? || '/%')");
                params.push(cwd.clone());
                params.push(cwd.clone());
            } else {
                sql.push_str(" AND e.cwd = ?");
                params.push(cwd.clone());
            }
        }
        if let Some(ts) = &self.from_iso {
            sql.push_str(" AND e.timestamp >= CAST(? AS TIMESTAMP)");
            params.push(ts.clone());
        }
        if let Some(ts) = &self.to_iso {
            sql.push_str(" AND e.timestamp <= CAST(? AS TIMESTAMP)");
            params.push(ts.clone());
        }
    }
}

/// Parse `YYYY-MM-DD`. `end_of_day` expands to `23:59:59` for inclusive `--to`.
fn parse_date(s: &str, end_of_day: bool) -> Result<String, String> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err(format!("invalid date '{s}', expected YYYY-MM-DD"));
    }
    let y: i32 = parts[0]
        .parse()
        .map_err(|_| format!("invalid year in '{s}'"))?;
    let m: u32 = parts[1]
        .parse()
        .map_err(|_| format!("invalid month in '{s}'"))?;
    let d: u32 = parts[2]
        .parse()
        .map_err(|_| format!("invalid day in '{s}'"))?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(format!("invalid month/day in '{s}'"));
    }
    let suffix = if end_of_day { "23:59:59" } else { "00:00:00" };
    Ok(format!("{y:04}-{m:02}-{d:02} {suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_parse_start_of_day() {
        assert_eq!(
            parse_date("2026-05-01", false).unwrap(),
            "2026-05-01 00:00:00"
        );
    }

    #[test]
    fn date_parse_end_of_day() {
        assert_eq!(
            parse_date("2026-05-01", true).unwrap(),
            "2026-05-01 23:59:59"
        );
    }

    #[test]
    fn date_parse_rejects_bad_input() {
        assert!(parse_date("2026/05/01", false).is_err());
        assert!(parse_date("2026-13-01", false).is_err());
        assert!(parse_date("not-a-date", false).is_err());
    }

    #[test]
    fn append_where_all_includes_no_cwd() {
        let s = Scope {
            cwd_exact: None,
            match_subdirs: false,
            from_iso: None,
            to_iso: None,
        };
        let mut sql = String::new();
        let mut params = Vec::new();
        s.append_where(&mut sql, &mut params);
        assert_eq!(sql, "");
        assert!(params.is_empty());
    }

    #[test]
    fn append_where_subdirs() {
        let s = Scope {
            cwd_exact: Some("/x".into()),
            match_subdirs: true,
            from_iso: Some("2026-05-01 00:00:00".into()),
            to_iso: None,
        };
        let mut sql = String::new();
        let mut params = Vec::new();
        s.append_where(&mut sql, &mut params);
        assert!(sql.contains("e.cwd = ?"));
        assert!(sql.contains("LIKE ? || '/%'"));
        assert!(sql.contains("e.timestamp >= CAST(? AS TIMESTAMP)"));
        assert_eq!(
            params,
            vec!["/x".to_string(), "/x".into(), "2026-05-01 00:00:00".into()]
        );
    }

    #[test]
    fn append_where_exact_only() {
        let s = Scope {
            cwd_exact: Some("/x".into()),
            match_subdirs: false,
            from_iso: None,
            to_iso: None,
        };
        let mut sql = String::new();
        let mut params = Vec::new();
        s.append_where(&mut sql, &mut params);
        assert_eq!(sql, " AND e.cwd = ?");
        assert_eq!(params, vec!["/x".to_string()]);
    }
}
