use crate::cli::UpdateArgs;

const REPO_OWNER: &str = "Alfredvc";
const REPO_NAME: &str = "cct";
const BIN_NAME: &str = "cct";

fn detect_target() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        ("macos", "x86_64") => Ok("darwin-x86_64"),
        ("macos", "aarch64") => Ok("darwin-aarch64"),
        (os, arch) => Err(format!("unsupported platform: {os}/{arch}")),
    }
}

pub fn run(args: UpdateArgs) {
    let target = match detect_target() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    let mut builder = self_update::backends::github::Update::configure();
    builder
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .target(target)
        .bin_path_in_archive("cct-{{ version }}-{{ target }}/{{ bin }}")
        .show_download_progress(true)
        .current_version(self_update::cargo_crate_version!())
        .no_confirm(args.yes);

    if let Some(v) = args.version.as_deref() {
        let tag = if v.starts_with('v') {
            v.to_string()
        } else {
            format!("v{v}")
        };
        builder.target_version_tag(&tag);
    }

    let updater = match builder.build() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("error: configure update: {e}");
            std::process::exit(1);
        }
    };

    match updater.update() {
        Ok(status) if status.updated() => {
            println!("Updated cct to {}", status.version());
        }
        Ok(status) => {
            println!("cct already at {}", status.version());
        }
        Err(e) => {
            eprintln!("error: update failed: {e}");
            std::process::exit(1);
        }
    }
}
