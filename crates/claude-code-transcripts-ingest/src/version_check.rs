//! Daily "update available" banner. Standard `update-notifier` pattern:
//! a detached background thread refreshes the cache file at most once per
//! day; the banner is read from cache only and printed to stderr when
//! interactive. Never blocks the foreground command.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cli::Command;

const CACHE_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct VersionCache {
    pub last_checked_unix: u64,
    pub latest_version: String,
}

pub(crate) fn cache_path() -> PathBuf {
    let xdg = std::env::var("XDG_CACHE_HOME").ok();
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok());
    cache_path_with_env(xdg.as_deref(), home.as_deref())
}

/// Pure helper: resolve cache path from explicit env values. Lets tests
/// drive the env-unset fallback without touching process-wide env (which
/// would race with parallel tests). When both `xdg` and `home` are
/// `None` (or blank), falls back to `.cache/cct/update_check.json`
/// relative to the current working directory.
pub(crate) fn cache_path_with_env(xdg: Option<&str>, home: Option<&str>) -> PathBuf {
    let cache_home = xdg
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = home.filter(|s| !s.is_empty()).unwrap_or(".");
            PathBuf::from(home).join(".cache")
        });
    cache_home.join("cct").join("update_check.json")
}

pub(crate) fn read_cache_at(path: &std::path::Path) -> Option<VersionCache> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn write_cache_atomic_at(
    path: &std::path::Path,
    cache: &VersionCache,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let json = serde_json::to_vec_pretty(cache)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub(crate) fn should_refresh(cache: Option<&VersionCache>, now_unix: u64) -> bool {
    match cache {
        None => true,
        Some(c) => now_unix.saturating_sub(c.last_checked_unix) > CACHE_TTL_SECS,
    }
}

pub(crate) fn format_banner_if_newer(current: &str, latest: &str) -> Option<String> {
    let latest_clean = latest.trim_start_matches('v');
    // self_update::version::bump_is_greater returns Err on unparseable input.
    if !self_update::version::bump_is_greater(current, latest_clean).unwrap_or(false) {
        return None;
    }
    // Compose the inner content lines first, then size the box around the
    // widest one. Versions vary in length, so a hard-coded border width
    // would either truncate or leave a ragged right edge. Width is
    // measured in `chars` (not bytes) so the `→` glyph is counted as one
    // column — that matches how monospace terminals render it.
    let line1 = format!("Update available: {current} → {latest_clean}");
    let line2 = "Run `cct update` to upgrade.".to_string();
    // Minimum width keeps the banner visually proportional even when both
    // version strings are very short (e.g. `0 → 1`). Matches the previous
    // hard-coded border size.
    const MIN_INNER: usize = 44;
    let inner_width = MIN_INNER
        .max(line1.chars().count())
        .max(line2.chars().count());
    let border = "─".repeat(inner_width + 2);
    let pad = |s: &str| {
        let n = s.chars().count();
        let fill = inner_width.saturating_sub(n);
        format!("│ {s}{} │", " ".repeat(fill))
    };
    Some(format!(
        "\n╭{border}╮\n{}\n{}\n╰{border}╯\n",
        pad(&line1),
        pad(&line2),
    ))
}

pub(crate) fn is_disabled_by_env(no_update_check: Option<&str>, ci: Option<&str>) -> bool {
    is_truthy(no_update_check) || is_truthy(ci)
}

fn is_truthy(v: Option<&str>) -> bool {
    matches!(
        v.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

pub(crate) fn skip_for_subcommand(cmd: &Command) -> bool {
    matches!(cmd, Command::Update(_))
}

pub(crate) fn deterministic_gates_open(cmd: &Command, env_disabled: bool) -> bool {
    !skip_for_subcommand(cmd) && !env_disabled
}

const REPO_OWNER: &str = "Alfredvc";
const REPO_NAME: &str = "cct";

pub(crate) fn fetch_latest_version() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // Hit GitHub's `/repos/{owner}/{name}/releases/latest` endpoint directly
    // via `Update::get_latest_release` so we agree, by construction, with what
    // `cct update` and `install.sh` will install. That endpoint returns "the
    // most recent non-prerelease, non-draft release, sorted by created_at",
    // so we don't have to reimplement that selection on the client. The
    // earlier implementation iterated `ReleaseList::fetch()` and skipped tags
    // containing `-`, which silently disagreed when a stable patch (e.g.
    // 0.2.1) was published after a newer minor (e.g. 0.3.0).
    //
    // We don't call `update()`; we only need the tag. The `bin_name` and
    // `bin_install_path` values below are required by the builder but are
    // never read by `get_latest_release`. We pass an explicit dummy install
    // path so the builder doesn't shell out to `env::current_exe()`, which
    // would convert a sandbox quirk into a misleading config error.
    let release = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("cct")
        .bin_install_path("/dev/null")
        .current_version(self_update::cargo_crate_version!())
        .build()?
        .get_latest_release()?;
    // `Release.version` already has the leading `v` stripped by self_update.
    Ok(release.version)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

fn gates_open(cmd: &Command) -> bool {
    let no_update = read_env("CCT_NO_UPDATE_CHECK");
    let ci = read_env("CI");
    let env_disabled = is_disabled_by_env(no_update.as_deref(), ci.as_deref());
    if !deterministic_gates_open(cmd, env_disabled) {
        return false;
    }
    std::io::stderr().is_terminal()
}

/// Sweep `update_check.tmp.*` files in `dir` whose mtime is older than
/// `max_age_secs`. Best-effort: every error (including a missing
/// directory, an unreadable entry, or a failed unlink) is swallowed.
/// Tmp files are produced by `write_cache_atomic_at` between the
/// `write` and the `rename`; if the process dies in that window the
/// tmp file lingers, and over years that adds up. Seven days is well
/// past any realistic `cct` run, so anything older is definitely orphaned.
pub(crate) fn sweep_old_tmp_files(dir: &std::path::Path, max_age_secs: u64, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with("update_check.tmp.") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        let age = now.duration_since(mtime).map(|d| d.as_secs()).unwrap_or(0);
        if age > max_age_secs {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

const TMP_SWEEP_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

/// Spawn a detached background thread that refreshes the cache if stale.
/// Never blocks. Errors and panics inside the thread are swallowed.
///
/// On fetch failure, we still write the timestamp so we don't retry on
/// every invocation when the user is offline or rate-limited. We
/// preserve the previously-known `latest_version` (or empty string on
/// first-ever attempt). `format_banner_if_newer` returns `None` for
/// empty / unparseable versions, so the empty case is safe.
///
/// The foreground process may exit before this thread finishes — that's
/// by design. The cache will be repopulated on the next slow-enough run.
pub fn maybe_spawn_check(cmd: &Command) {
    spawn_check_with(cache_path(), gates_open(cmd));
}

/// Inner spawn entry point with the gate decision and cache path lifted
/// out as parameters. Lets unit tests prove the gate-closed path is a
/// true no-op (no fs writes) without spinning up an `is_terminal()`
/// fake. Public callers should go through [`maybe_spawn_check`].
pub(crate) fn spawn_check_with(path: PathBuf, gates_open: bool) {
    if !gates_open {
        return;
    }
    let existing = read_cache_at(&path);
    if !should_refresh(existing.as_ref(), now_unix()) {
        return;
    }
    std::thread::spawn(move || {
        let _ = std::panic::catch_unwind(move || {
            // Best-effort orphan sweep before the fetch. Runs once per
            // refresh (i.e. at most daily), which is plenty — the failure
            // mode it guards against (process killed mid-rename) is rare.
            if let Some(parent) = path.parent() {
                sweep_old_tmp_files(parent, TMP_SWEEP_MAX_AGE_SECS, SystemTime::now());
            }
            let fetched = fetch_latest_version();
            let prev_version = read_cache_at(&path)
                .map(|c| c.latest_version)
                .unwrap_or_default();
            let cache = VersionCache {
                last_checked_unix: now_unix(),
                latest_version: fetched.unwrap_or(prev_version),
            };
            let _ = write_cache_atomic_at(&path, &cache);
        });
    });
}

/// Read the cache (no network) and return the banner string that should
/// be printed, or `None` if nothing should print.
///
/// This is strictly cache-only: it never sees the result of the fetch
/// kicked off by the sibling [`maybe_spawn_check`] call in the same
/// process — that fetch finishes (and writes the cache) after this
/// returns. So the banner reflects the PREVIOUS run's fetch.
pub fn maybe_print_banner(cmd: &Command) {
    if let Some(banner) = compute_banner_with(
        &cache_path(),
        gates_open(cmd),
        self_update::cargo_crate_version!(),
    ) {
        eprint!("{banner}");
    }
}

/// Inner banner computation with gate / cache path / current version
/// lifted out as parameters. Keeps the print path testable without
/// capturing stderr. Public callers should go through
/// [`maybe_print_banner`].
pub(crate) fn compute_banner_with(
    path: &std::path::Path,
    gates_open: bool,
    current: &str,
) -> Option<String> {
    if !gates_open {
        return None;
    }
    let cache = read_cache_at(path)?;
    format_banner_if_newer(current, &cache.latest_version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn read_returns_none_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("update_check.json");
        assert!(read_cache_at(&path).is_none());
    }

    #[test]
    fn read_returns_none_when_file_malformed() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("update_check.json");
        std::fs::write(&path, b"not json").unwrap();
        assert!(read_cache_at(&path).is_none());
    }

    #[test]
    fn write_then_read_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("update_check.json");
        let cache = VersionCache {
            last_checked_unix: 1_700_000_000,
            latest_version: "0.2.0".to_string(),
        };
        write_cache_atomic_at(&path, &cache).unwrap();
        let read = read_cache_at(&path).unwrap();
        assert_eq!(read, cache);
    }

    #[test]
    fn write_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("dirs").join("cache.json");
        let cache = VersionCache {
            last_checked_unix: 1,
            latest_version: "0.0.1".to_string(),
        };
        write_cache_atomic_at(&path, &cache).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn should_refresh_when_no_cache() {
        assert!(should_refresh(None, 1_000_000_000));
    }

    #[test]
    fn should_refresh_when_cache_is_stale() {
        let cache = VersionCache {
            last_checked_unix: 1_000_000_000,
            latest_version: "0.1.9".to_string(),
        };
        let now = 1_000_000_000 + CACHE_TTL_SECS + 1;
        assert!(should_refresh(Some(&cache), now));
    }

    #[test]
    fn should_not_refresh_when_cache_is_fresh() {
        let cache = VersionCache {
            last_checked_unix: 1_000_000_000,
            latest_version: "0.1.9".to_string(),
        };
        let now = 1_000_000_000 + CACHE_TTL_SECS - 1;
        assert!(!should_refresh(Some(&cache), now));
    }

    #[test]
    fn should_not_refresh_when_cache_is_at_exact_ttl_boundary() {
        let cache = VersionCache {
            last_checked_unix: 1_000_000_000,
            latest_version: "0.1.9".to_string(),
        };
        let now = 1_000_000_000 + CACHE_TTL_SECS;
        assert!(!should_refresh(Some(&cache), now));
    }

    #[test]
    fn should_refresh_when_clock_went_backwards() {
        // If the cache says "checked at T+1000" but `now` is T, treat as fresh
        // (don't double-fetch). Saturating subtraction handles this.
        let cache = VersionCache {
            last_checked_unix: 2_000_000_000,
            latest_version: "0.1.9".to_string(),
        };
        let now = 1_000_000_000;
        assert!(!should_refresh(Some(&cache), now));
    }

    #[test]
    fn banner_returns_none_when_versions_equal() {
        assert!(format_banner_if_newer("0.1.9", "0.1.9").is_none());
    }

    #[test]
    fn banner_returns_none_when_latest_is_older() {
        assert!(format_banner_if_newer("0.2.0", "0.1.9").is_none());
    }

    #[test]
    fn banner_returns_none_when_latest_is_unparseable() {
        assert!(format_banner_if_newer("0.1.9", "not-a-version").is_none());
    }

    #[test]
    fn banner_renders_when_latest_is_newer() {
        let out = format_banner_if_newer("0.1.9", "0.2.0").expect("expected banner");
        assert!(out.contains("0.1.9"), "missing current version: {out}");
        assert!(out.contains("0.2.0"), "missing latest version: {out}");
        assert!(out.contains("cct update"), "missing upgrade hint: {out}");
        assert_box_well_formed(&out);
    }

    #[test]
    fn banner_strips_leading_v_from_latest() {
        let out = format_banner_if_newer("0.1.9", "v0.2.0").expect("expected banner");
        assert!(out.contains("0.2.0"));
        assert!(
            !out.contains("v0.2.0"),
            "leading v should be stripped: {out}"
        );
        assert_box_well_formed(&out);
    }

    #[test]
    fn banner_grows_with_long_version_strings() {
        // Versions long enough to overflow the 44-char minimum width — the
        // box must grow to accommodate them, not truncate.
        let out = format_banner_if_newer("0.1.9", "999.999.999-rc.1+verylongbuildmeta")
            .expect("expected banner");
        assert!(out.contains("999.999.999-rc.1+verylongbuildmeta"));
        assert_box_well_formed(&out);
    }

    /// Validate that every content row starts with `│ `, ends with ` │`,
    /// is the same visible width as the border row, and that the corners
    /// match. Catches the regression where right-border padding was missing.
    fn assert_box_well_formed(out: &str) {
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert!(lines.len() >= 4, "expected >=4 non-empty lines: {out:?}");
        let top = lines[0];
        let bottom = lines[lines.len() - 1];
        assert!(top.starts_with('╭') && top.ends_with('╮'), "top: {top:?}");
        assert!(
            bottom.starts_with('╰') && bottom.ends_with('╯'),
            "bottom: {bottom:?}"
        );
        let top_width = top.chars().count();
        let bottom_width = bottom.chars().count();
        assert_eq!(top_width, bottom_width, "top/bottom width mismatch");
        for content in &lines[1..lines.len() - 1] {
            assert!(
                content.starts_with("│ "),
                "content line missing left border: {content:?}"
            );
            assert!(
                content.ends_with(" │"),
                "content line missing right border: {content:?}"
            );
            assert_eq!(
                content.chars().count(),
                top_width,
                "content line width != border width: {content:?}"
            );
        }
    }

    #[test]
    fn disabled_when_cct_no_update_check_is_truthy() {
        assert!(is_disabled_by_env(Some("1"), None));
        assert!(is_disabled_by_env(Some("true"), None));
        assert!(is_disabled_by_env(Some("yes"), None));
    }

    #[test]
    fn disabled_when_ci_is_truthy() {
        assert!(is_disabled_by_env(None, Some("true")));
        assert!(is_disabled_by_env(None, Some("1")));
    }

    #[test]
    fn not_disabled_when_envs_are_unset_or_empty() {
        assert!(!is_disabled_by_env(None, None));
        assert!(!is_disabled_by_env(Some(""), Some("")));
        assert!(!is_disabled_by_env(Some("0"), Some("0")));
        assert!(!is_disabled_by_env(Some("false"), Some("false")));
    }

    #[test]
    fn skip_for_update_subcommand_only() {
        use crate::cli::{Command, InfoArgs, UpdateArgs};
        let info = Command::Info(InfoArgs {
            db: std::path::PathBuf::from("/tmp/x.duckdb"),
        });
        let update = Command::Update(UpdateArgs {
            version: None,
            yes: false,
        });
        assert!(!skip_for_subcommand(&info));
        assert!(skip_for_subcommand(&update));
    }

    #[test]
    fn deterministic_gates_open_for_info_when_env_clean() {
        use crate::cli::{Command, InfoArgs};
        let info = Command::Info(InfoArgs {
            db: std::path::PathBuf::from("/tmp/x.duckdb"),
        });
        assert!(deterministic_gates_open(&info, /*env_disabled=*/ false));
    }

    #[test]
    fn gates_closed_for_update_command_regardless_of_env() {
        // Authoritative regression guard for the `Update` short-circuit:
        // `cct update` must never fire the banner gate, even when every
        // env opt-out is unset. Smoke-test step 8 in the plan references
        // this test by name.
        use crate::cli::{Command, UpdateArgs};
        let update = Command::Update(UpdateArgs {
            version: None,
            yes: false,
        });
        assert!(!deterministic_gates_open(&update, /*env_disabled=*/ false));
        assert!(!deterministic_gates_open(&update, /*env_disabled=*/ true));
    }

    #[test]
    fn deterministic_gates_closed_for_info_when_env_disabled() {
        use crate::cli::{Command, InfoArgs};
        let info = Command::Info(InfoArgs {
            db: std::path::PathBuf::from("/tmp/x.duckdb"),
        });
        assert!(!deterministic_gates_open(&info, /*env_disabled=*/ true));
    }

    #[test]
    fn cache_path_uses_xdg_when_set() {
        let p = cache_path_with_env(Some("/tmp/xdg"), Some("/home/u"));
        assert_eq!(p, PathBuf::from("/tmp/xdg/cct/update_check.json"));
    }

    #[test]
    fn cache_path_uses_home_when_xdg_blank_or_unset() {
        let p = cache_path_with_env(None, Some("/home/u"));
        assert_eq!(p, PathBuf::from("/home/u/.cache/cct/update_check.json"));
        let p = cache_path_with_env(Some(""), Some("/home/u"));
        assert_eq!(p, PathBuf::from("/home/u/.cache/cct/update_check.json"));
    }

    #[test]
    fn cache_path_falls_back_to_cwd_when_both_envs_unset() {
        // Both XDG_CACHE_HOME and HOME (and USERPROFILE on Windows) can be
        // missing in stripped sandboxes / minimal containers. We don't
        // panic — we land in `./.cache/cct/...` so the writer at least
        // gets a relative path it can attempt.
        let p = cache_path_with_env(None, None);
        assert_eq!(p, PathBuf::from("./.cache/cct/update_check.json"));
        let p = cache_path_with_env(Some(""), Some(""));
        assert_eq!(p, PathBuf::from("./.cache/cct/update_check.json"));
    }

    #[test]
    fn sweep_removes_only_old_tmp_files() {
        let dir = TempDir::new().unwrap();
        // Two tmp files plus one unrelated file. We can't easily set
        // mtime portably without an extra dep, so instead we sweep with
        // a max_age of 0 and a `now` strictly after each file's mtime —
        // every tmp file qualifies. The unrelated file must survive
        // because its name does not start with `update_check.tmp.`.
        let old_tmp = dir.path().join("update_check.tmp.1234");
        let other_tmp = dir.path().join("update_check.tmp.5678");
        let unrelated = dir.path().join("update_check.json");
        std::fs::write(&old_tmp, b"x").unwrap();
        std::fs::write(&other_tmp, b"y").unwrap();
        std::fs::write(&unrelated, b"z").unwrap();
        // `now` = SystemTime::now() + 1s ensures every file's age > 0.
        let later = SystemTime::now() + std::time::Duration::from_secs(1);
        sweep_old_tmp_files(dir.path(), 0, later);
        assert!(!old_tmp.exists(), "old tmp should be swept");
        assert!(!other_tmp.exists(), "old tmp should be swept");
        assert!(unrelated.exists(), "non-tmp file must be preserved");
    }

    #[test]
    fn sweep_keeps_recent_tmp_files() {
        let dir = TempDir::new().unwrap();
        let recent = dir.path().join("update_check.tmp.999");
        std::fs::write(&recent, b"x").unwrap();
        // max_age of 1 day vs. a file we just created — must survive.
        sweep_old_tmp_files(dir.path(), 24 * 60 * 60, SystemTime::now());
        assert!(recent.exists());
    }

    #[test]
    fn sweep_swallows_missing_dir() {
        // Should not panic.
        sweep_old_tmp_files(
            std::path::Path::new("/nonexistent/cct/sweep/dir"),
            0,
            SystemTime::now(),
        );
    }

    #[test]
    fn spawn_check_with_gates_closed_does_not_touch_cache() {
        // When gates are closed (env-disabled, Update subcommand, or
        // non-TTY) the spawn entry must be a true no-op: no fs writes,
        // no thread, no tmp files. We verify by pointing it at a fresh
        // temp dir and confirming nothing appears.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cct").join("update_check.json");
        spawn_check_with(path.clone(), /*gates_open=*/ false);
        // Nothing should exist — not the parent dir, not the cache file,
        // not any tmp file.
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn compute_banner_returns_none_when_gates_closed() {
        // Even with a cache file on disk that WOULD produce a banner,
        // the gate-closed path must return None.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("update_check.json");
        write_cache_atomic_at(
            &path,
            &VersionCache {
                last_checked_unix: 1,
                latest_version: "99.0.0".to_string(),
            },
        )
        .unwrap();
        assert!(compute_banner_with(&path, /*gates_open=*/ false, "0.1.9").is_none());
    }

    #[test]
    fn compute_banner_returns_none_when_cache_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("update_check.json");
        assert!(compute_banner_with(&path, /*gates_open=*/ true, "0.1.9").is_none());
    }

    #[test]
    fn compute_banner_returns_some_when_cache_has_newer_version() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("update_check.json");
        write_cache_atomic_at(
            &path,
            &VersionCache {
                last_checked_unix: 1,
                latest_version: "99.0.0".to_string(),
            },
        )
        .unwrap();
        let banner =
            compute_banner_with(&path, /*gates_open=*/ true, "0.1.9").expect("expected banner");
        assert!(banner.contains("0.1.9"));
        assert!(banner.contains("99.0.0"));
    }
}
