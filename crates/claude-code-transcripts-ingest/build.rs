use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let web_src = manifest_dir.join("web");
    // All npm operations run from OUT_DIR/web so writes stay inside OUT_DIR.
    let web_out = out_dir.join("web");
    let web_dist_out = web_out.join("dist");

    for f in [
        "web/src",
        "web/package.json",
        "web/package-lock.json",
        "web/index.html",
        "web/vite.config.ts",
        "web/tsconfig.json",
    ] {
        println!("cargo:rerun-if-changed={f}");
    }
    println!("cargo:rerun-if-env-changed=SKIP_WEB_BUILD");

    if std::env::var_os("SKIP_WEB_BUILD").is_some() {
        if !web_dist_out.join("index.html").exists() {
            copy_dist(&web_src.join("dist"), &web_dist_out);
        }
        return;
    }

    let npm = which("npm");
    if npm.is_none() {
        println!("cargo:warning=npm not found; using prebuilt web/dist (set SKIP_WEB_BUILD=1 to silence)");
        if !web_dist_out.join("index.html").exists() {
            copy_dist(&web_src.join("dist"), &web_dist_out);
        }
        return;
    }
    let npm = npm.unwrap();

    sync_web_src(&web_src, &web_out);

    // Re-run `npm ci` when node_modules is missing OR the package-lock has
    // changed since the last install. We track the latter via a stamp file
    // that holds the lockfile's bytes from the previous successful ci run —
    // checking node_modules existence alone misses dep additions.
    let lockfile = web_out.join("package-lock.json");
    let stamp = web_out.join("node_modules/.cct-ci-stamp");
    let cur_lock = std::fs::read(&lockfile).ok();
    let stamp_content = std::fs::read(&stamp).ok();
    let needs_install =
        !web_out.join("node_modules").exists() || cur_lock.as_ref() != stamp_content.as_ref();
    if needs_install {
        run(&npm, &["ci", "--silent"], &web_out, &[]);
        if let Some(bytes) = cur_lock {
            // Best-effort: stamp survives across cargo builds; failure here
            // just means we re-run `npm ci` next time.
            std::fs::write(&stamp, bytes).ok();
        }
    }

    run(
        &npm,
        &["run", "build", "--silent"],
        &web_out,
        &[("VITE_OUT_DIR", web_dist_out.to_str().unwrap())],
    );
    ensure_dist_exists(&web_dist_out);
}

// Copy web source files into dst, skipping build artifacts so all npm
// operations run inside OUT_DIR and never touch the source tree.
fn sync_web_src(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap_or_else(|e| panic!("create {dst:?}: {e}"));
    for entry in std::fs::read_dir(src).unwrap_or_else(|e| panic!("readdir {src:?}: {e}")) {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if matches!(
            name.to_str(),
            Some("node_modules") | Some("dist") | Some(".vite")
        ) {
            continue;
        }
        let dst_path = dst.join(&name);
        // Mirror deletes from src by removing dst_path before copying; without
        // this, files renamed or deleted in src linger in OUT_DIR and break
        // tsc / vite (e.g. an old import that points at a now-removed module).
        if dst_path.exists() {
            if dst_path.is_dir() {
                std::fs::remove_dir_all(&dst_path)
                    .unwrap_or_else(|e| panic!("remove {dst_path:?}: {e}"));
            } else {
                std::fs::remove_file(&dst_path)
                    .unwrap_or_else(|e| panic!("remove {dst_path:?}: {e}"));
            }
        }
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&entry.path(), &dst_path)
                .unwrap_or_else(|e| panic!("copy dir {:?}: {e}", entry.path()));
        } else {
            std::fs::copy(entry.path(), &dst_path)
                .unwrap_or_else(|e| panic!("copy {:?}: {e}", entry.path()));
        }
    }
}

fn copy_dist(src: &Path, dst: &Path) {
    if !src.join("index.html").exists() {
        panic!(
            "web/dist/index.html missing at {}. Run `npm --prefix web run build` first.",
            src.display()
        );
    }
    copy_dir_all(src, dst).unwrap_or_else(|e| panic!("copy {src:?} -> {dst:?}: {e}"));
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

fn ensure_dist_exists(dist: &Path) {
    let idx = dist.join("index.html");
    if !idx.exists() {
        panic!("web/dist/index.html missing at {}", idx.display());
    }
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let full = dir.join(cmd);
        if full.is_file() {
            return Some(full);
        }
    }
    None
}

fn run(cmd: &Path, args: &[&str], cwd: &Path, env: &[(&str, &str)]) {
    let mut command = Command::new(cmd);
    command.args(args).current_dir(cwd);
    for (k, v) in env {
        command.env(k, v);
    }
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("run {} {:?}: {e}", cmd.display(), args));
    if !status.success() {
        panic!("{} {:?} failed: {status}", cmd.display(), args);
    }
}
