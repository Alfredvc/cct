//! Pipeline: overwrite DB, walk → parallel parse → single-transaction write.
//!
//! The writer keeps one transaction and one appender per table open for the
//! whole ingest, then commits once at the end. Per-batch commits caused
//! DuckDB to re-run column compression/FSST analysis as the tables grew,
//! which made throughput collapse over time.
//!
//! Fail fast: any error prints and exits with code 1. No retries, no
//! rollback, no re-ingest — the output DB is recreated from scratch on
//! every run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use crossbeam_channel::bounded;
use duckdb::types::{TimeUnit, ValueRef};
use duckdb::{appender_params_from_iter, params, Connection, ToSql};
use rayon::prelude::*;
use serde_json::Value;
use walkdir::WalkDir;

use crate::cli::IngestArgs;
use crate::parse::{parse_file, ParsedFile};
use crate::pricing::{self, build_lookup, merge, seed_rows, PriceRow};
use crate::schema::{
    COMMENTS_DDL, DEDUPED_TABLE_DDL, INDEXES_DDL, PK_DDL, SCHEMA_DDL, TOOL_USES_VIEW_DDL,
};

/// Summary returned by [`run_ingest`]. `unknown_variants` is read by the
/// integration test for the `last-prompt:no-fields` counter; the other three
/// fields are populated for future test/observability use and currently
/// unread, hence the field-level `allow(dead_code)`.
#[derive(Debug, Default, Clone)]
pub struct RunSummary {
    #[allow(dead_code)]
    pub files_processed: usize,
    #[allow(dead_code)]
    pub entry_counts: HashMap<String, u64>,
    pub unknown_variants: HashMap<String, u64>,
    #[allow(dead_code)]
    pub unknown_models: HashMap<String, u64>,
}

pub fn run(cli: IngestArgs) -> ! {
    match run_ingest(cli) {
        Ok(_) => std::process::exit(0),
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
    }
}

/// Library entry point used by `pub fn run` (the binary path) and by integration
/// tests. Returns `Err` for top-level setup/IO/DB failures so callers can avoid
/// `process::exit`.
///
/// Limitation: the parser worker thread (spawned at line ~110 of this file)
/// still calls `die` (process-exit) on a JSONL parse failure — threading that
/// error back through the channel is out of scope. Tests that drive
/// `run_ingest` must use well-formed fixtures. The originally-reported parse
/// failure is covered by the manual real-corpus smoke (acceptance criterion 1).
pub fn run_ingest(cli: IngestArgs) -> Result<RunSummary, String> {
    let started = Instant::now();
    let jobs = if cli.jobs == 0 {
        num_cpus_or(4)
    } else {
        cli.jobs
    };

    rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build_global()
        .map_err(|e| format!("rayon init: {e}"))?;

    remove_db_files(&cli.output);

    if let Some(parent) = cli.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
    }

    let files = discover(&cli.input_dir);
    let total_files = files.len();
    eprintln!(
        "Found {total_files} .jsonl files under {}",
        cli.input_dir.display()
    );

    let mut conn =
        Connection::open(&cli.output).map_err(|e| format!("open {}: {e}", cli.output.display()))?;
    conn.execute_batch(SCHEMA_DDL)
        .map_err(|e| format!("schema init: {e}"))?;

    let mut rows = seed_rows();
    if let Some(p) = &cli.pricing {
        let overrides = pricing::load_overrides(p).unwrap_or_else(|e| die(e));
        rows = merge(rows, overrides);
    }
    seed_pricing(&conn, &rows);
    let pricing_arc = Arc::new(build_lookup(&rows));

    let mut next_id: i64 = 1;

    let (tx, rx) = bounded::<ParsedFile>(jobs * 2);

    let processed = Arc::new(AtomicUsize::new(0));
    let unknown_models_global: Arc<std::sync::Mutex<HashMap<String, u64>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));
    let unknown_variants_global: Arc<std::sync::Mutex<HashMap<String, u64>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));

    let stop_progress = Arc::new(AtomicBool::new(false));
    let progress_handle = if !cli.no_progress {
        let processed = processed.clone();
        let stop = stop_progress.clone();
        let total = total_files;
        Some(std::thread::spawn(move || {
            let start = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1000));
                let done = processed.load(Ordering::Relaxed);
                let secs = start.elapsed().as_secs_f64().max(0.001);
                let rate = (done as f64) / secs;
                eprintln!("  progress: {done}/{total} files ({rate:.1}/s)");
                if done >= total {
                    break;
                }
            }
        }))
    } else {
        None
    };

    let pricing_for_parsers = pricing_arc.clone();
    let files_arc = Arc::new(files);
    let files_for_parsers = files_arc.clone();
    let parser_handle = std::thread::spawn(move || {
        files_for_parsers.par_iter().for_each_with(tx, |tx, path| {
            let parsed = parse_file(path, pricing_for_parsers.as_ref());
            if !parsed.failures.is_empty() {
                for (line, msg) in &parsed.failures {
                    eprintln!("  {}:{} parse: {}", parsed.transcript.file_path, line, msg);
                }
                die(format!("parse failure in {}", parsed.transcript.file_path));
            }
            if parsed.entries.is_empty() {
                return;
            }
            let _ = tx.send(parsed);
        });
    });

    // Single transaction for the whole ingest: keep appenders open across
    // every file so DuckDB only scans/compresses/flushes once at commit.
    let tx = conn.transaction().map_err(|e| format!("begin tx: {e}"))?;
    {
        let mut transcripts_app = tx
            .appender("transcripts")
            .map_err(|e| format!("appender transcripts: {e}"))?;
        let transcripts_ts = ts_cols("transcripts");

        let mut entries_app = tx
            .appender("entries")
            .map_err(|e| format!("appender entries: {e}"))?;
        let entries_ts = ts_cols("entries");
        let mut variant_apps: HashMap<&'static str, duckdb::Appender<'_>> = HashMap::new();

        for parsed in rx {
            if !parsed.unknown_models.is_empty() {
                let mut g = unknown_models_global.lock().unwrap();
                for m in &parsed.unknown_models {
                    *g.entry(m.clone()).or_insert(0) += 1;
                }
            }
            if !parsed.unknown_variants.is_empty() {
                let mut g = unknown_variants_global.lock().unwrap();
                for v in &parsed.unknown_variants {
                    *g.entry(v.clone()).or_insert(0) += 1;
                }
            }

            let t = &parsed.transcript;
            let opt_str = |o: &Option<String>| match o {
                Some(s) => Value::String(s.clone()),
                None => Value::Null,
            };
            let transcript_row: [Value; 10] = [
                Value::String(t.file_path.clone()),
                opt_str(&t.session_id),
                Value::Bool(t.is_subagent),
                opt_str(&t.agent_id),
                opt_str(&t.parent_session_id),
                Value::Number(serde_json::Number::from(t.entry_count as i64)),
                opt_str(&t.first_timestamp),
                opt_str(&t.last_timestamp),
                opt_str(&t.mtime),
                Value::String(Utc::now().to_rfc3339()),
            ];
            append_row(&mut transcripts_app, &transcript_row, transcripts_ts)
                .map_err(|m| format!("insert transcript {}: {m}", t.file_path))?;

            let n_entries = parsed.entries.len() as i64;
            let start_id = next_id;
            next_id += n_entries;
            let fp = parsed.transcript.file_path.clone();

            for (idx, mut e) in parsed.entries.into_iter().enumerate() {
                let id = start_id + idx as i64;
                e.entry[0] = Value::Number(serde_json::Number::from(id));
                append_row(&mut entries_app, &e.entry, entries_ts)
                    .map_err(|m| format!("write {fp}: insert entry: {m}"))?;

                if let Some((table, mut vrow)) = e.variant {
                    vrow[0] = Value::Number(serde_json::Number::from(id));
                    let app = get_or_open(&mut variant_apps, &tx, table);
                    append_row(app, &vrow, ts_cols(table))
                        .map_err(|m| format!("write {fp}: insert {table}: {m}"))?;
                }
                for (table, rows) in e.children {
                    let ts = ts_cols(table);
                    let app = get_or_open(&mut variant_apps, &tx, table);
                    for mut r in rows {
                        r[0] = Value::Number(serde_json::Number::from(id));
                        append_row(app, &r, ts)
                            .map_err(|m| format!("write {fp}: insert {table}: {m}"))?;
                    }
                }
            }
            processed.fetch_add(1, Ordering::Relaxed);
        }
        // Appenders + prepared statement drop here, flushing before commit.
    }
    tx.commit().map_err(|e| format!("commit: {e}"))?;

    parser_handle
        .join()
        .map_err(|_| "parser thread panicked".to_string())?;

    stop_progress.store(true, Ordering::Relaxed);
    if let Some(h) = progress_handle {
        let _ = h.join();
    }

    conn.execute_batch(TOOL_USES_VIEW_DDL)
        .map_err(|e| format!("create view: {e}"))?;
    conn.execute_batch(PK_DDL)
        .map_err(|e| format!("add primary keys: {e}"))?;
    conn.execute_batch(INDEXES_DDL)
        .map_err(|e| format!("create indexes: {e}"))?;
    conn.execute_batch(DEDUPED_TABLE_DDL)
        .map_err(|e| format!("create deduped table: {e}"))?;
    conn.execute_batch(COMMENTS_DDL)
        .map_err(|e| format!("add column comments: {e}"))?;

    let processed_n = processed.load(Ordering::Relaxed);
    let counts = entry_counts(&conn);
    let total_entries: u64 = counts.values().sum();

    eprintln!("\n── Ingest report ──");
    eprintln!("Files:        {processed_n} processed");
    eprintln!("Entries:      total={total_entries}");
    if !counts.is_empty() {
        let mut keys: Vec<&String> = counts.keys().collect();
        keys.sort();
        let summary: Vec<String> = keys.iter().map(|k| format!("{k}={}", counts[*k])).collect();
        eprintln!("              {}", summary.join("  "));
    }
    let unknown = unknown_models_global.lock().unwrap();
    if !unknown.is_empty() {
        eprintln!("Unknown models (cost_usd = NULL):");
        let mut keys: Vec<&String> = unknown.keys().collect();
        keys.sort();
        for k in keys {
            eprintln!("  - {k}  (count={})", unknown[k]);
        }
    }
    let unknown_vars = unknown_variants_global.lock().unwrap();
    if !unknown_vars.is_empty() {
        eprintln!("Unknown variants dropped (schema needs update):");
        let mut keys: Vec<&String> = unknown_vars.keys().collect();
        keys.sort();
        for k in keys {
            eprintln!("  - {k}  (count={})", unknown_vars[k]);
        }
    }
    let elapsed = started.elapsed();
    let h = elapsed.as_secs() / 3600;
    let m = (elapsed.as_secs() % 3600) / 60;
    let s = elapsed.as_secs() % 60;
    eprintln!("Elapsed:      {h:02}:{m:02}:{s:02}");

    let summary = RunSummary {
        files_processed: processed_n,
        entry_counts: counts,
        unknown_variants: unknown_vars.clone(),
        unknown_models: unknown.clone(),
    };
    drop(unknown_vars);
    drop(unknown);
    drop(conn);
    Ok(summary)
}

fn die(msg: String) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn remove_db_files(path: &Path) {
    if path.exists() {
        std::fs::remove_file(path)
            .unwrap_or_else(|e| die(format!("remove {}: {e}", path.display())));
    }
    let wal = PathBuf::from(format!("{}.wal", path.display()));
    if wal.exists() {
        std::fs::remove_file(&wal)
            .unwrap_or_else(|e| die(format!("remove {}: {e}", wal.display())));
    }
}

fn discover(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension().and_then(|s| s.to_str()) == Some("jsonl")
                && p.file_name().and_then(|s| s.to_str()) != Some("permissions_log.jsonl")
        })
        .collect();
    out.sort();
    out
}

fn num_cpus_or(default: usize) -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(default)
}

fn seed_pricing(conn: &Connection, rows: &[PriceRow]) {
    conn.execute("DELETE FROM model_pricing", [])
        .unwrap_or_else(|e| die(format!("clear pricing: {e}")));
    let mut stmt = conn
        .prepare(
            "INSERT INTO model_pricing
             (model, input_per_mtok, output_per_mtok,
              cache_creation_5m_per_mtok, cache_creation_1h_per_mtok,
              cache_read_per_mtok, effective_date)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .unwrap_or_else(|e| die(format!("prep pricing: {e}")));
    for r in rows {
        stmt.execute(params![
            r.model,
            r.input_per_mtok,
            r.output_per_mtok,
            r.cache_creation_5m_per_mtok,
            r.cache_creation_1h_per_mtok,
            r.cache_read_per_mtok,
            r.effective_date
        ])
        .unwrap_or_else(|e| die(format!("insert pricing: {e}")));
    }
}

fn entry_counts(conn: &Connection) -> HashMap<String, u64> {
    let mut stmt = conn
        .prepare("SELECT type, COUNT(*) FROM entries GROUP BY type")
        .unwrap_or_else(|e| die(format!("prep counts: {e}")));
    let qrows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .unwrap_or_else(|e| die(format!("counts: {e}")));
    let mut out = HashMap::new();
    for (k, v) in qrows.flatten() {
        out.insert(k, v as u64);
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────
// Writer
// ─────────────────────────────────────────────────────────────────────────

enum Cell<'a> {
    Auto(&'a Value),
    Ts(&'a Value),
}

impl<'a> ToSql for Cell<'a> {
    fn to_sql(&self) -> duckdb::Result<duckdb::types::ToSqlOutput<'_>> {
        use duckdb::types::{ToSqlOutput, Value as DV};
        let out = match self {
            Cell::Ts(v) => match v {
                Value::Null => ToSqlOutput::Borrowed(ValueRef::Null),
                Value::String(s) => match parse_ts_micros(s) {
                    Some(us) => {
                        ToSqlOutput::Borrowed(ValueRef::Timestamp(TimeUnit::Microsecond, us))
                    }
                    None => ToSqlOutput::Borrowed(ValueRef::Null),
                },
                _ => ToSqlOutput::Borrowed(ValueRef::Null),
            },
            Cell::Auto(v) => match v {
                Value::Null => ToSqlOutput::Borrowed(ValueRef::Null),
                Value::Bool(b) => ToSqlOutput::Owned(DV::Boolean(*b)),
                Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        ToSqlOutput::Owned(DV::BigInt(i))
                    } else if let Some(u) = n.as_u64() {
                        if u <= i64::MAX as u64 {
                            ToSqlOutput::Owned(DV::BigInt(u as i64))
                        } else {
                            ToSqlOutput::Owned(DV::Text(u.to_string()))
                        }
                    } else if let Some(f) = n.as_f64() {
                        ToSqlOutput::Owned(DV::Double(f))
                    } else {
                        ToSqlOutput::Borrowed(ValueRef::Null)
                    }
                }
                Value::String(s) => ToSqlOutput::Borrowed(ValueRef::Text(s.as_bytes())),
                other => ToSqlOutput::Owned(DV::Text(other.to_string())),
            },
        };
        Ok(out)
    }
}

fn parse_ts_micros(s: &str) -> Option<i64> {
    use chrono::{DateTime, NaiveDateTime};
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_micros());
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Some(ndt.and_utc().timestamp_micros());
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f") {
        return Some(ndt.and_utc().timestamp_micros());
    }
    None
}

fn get_or_open<'tx, 'm>(
    apps: &'m mut HashMap<&'static str, duckdb::Appender<'tx>>,
    tx: &'tx duckdb::Transaction<'_>,
    table: &'static str,
) -> &'m mut duckdb::Appender<'tx> {
    if !apps.contains_key(table) {
        let app = tx
            .appender(table)
            .unwrap_or_else(|e| die(format!("appender {table}: {e}")));
        apps.insert(table, app);
    }
    apps.get_mut(table).unwrap()
}

fn ts_cols(table: &str) -> &'static [usize] {
    match table {
        "transcripts" => &[6, 7, 8, 9],
        "entries" => &[10],
        "task_summary_entries" => &[3],
        "pr_link_entries" => &[5],
        "queue_operation_entries" => &[2],
        "speculation_accept_entries" => &[1],
        "attachment_entries" => &[63],
        "file_history_delta_entries" => &[6, 8],
        "frame_link_entries" => &[6],
        _ => &[],
    }
}

fn append_row(app: &mut duckdb::Appender<'_>, row: &[Value], ts: &[usize]) -> Result<(), String> {
    app.append_row(appender_params_from_iter(row.iter().enumerate().map(
        |(i, v)| {
            if ts.contains(&i) {
                Cell::Ts(v)
            } else {
                Cell::Auto(v)
            }
        },
    )))
    .map_err(|e| e.to_string())
}
