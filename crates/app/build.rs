//! Embeds the git short sha and build time (`EASYCAD_GIT_SHA`, `EASYCAD_BUILD_TIME`)
//! so the running binary can say which build it is (About, the reload toast).
use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=9", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=EASYCAD_GIT_SHA={sha}{}", if dirty { "+" } else { "" });
    println!("cargo:rustc-env=EASYCAD_BUILD_TIME={secs}");
    println!("cargo:rerun-if-env-changed=EASYCAD_BUILD_STAMP");
}
