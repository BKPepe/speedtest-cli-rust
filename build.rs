//! Records the build date so `--version` can report it, matching build.sh.

use std::process::Command;

fn main() {
    // Honour SOURCE_DATE_EPOCH so reproducible builds stay reproducible.
    let date = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|epoch| epoch.parse::<i64>().ok())
        .and_then(|epoch| {
            Command::new("date")
                .args(["-u", "-r", &epoch.to_string(), "+%Y-%m-%d %H:%M:%S %Z"])
                .output()
                .ok()
        })
        .or_else(|| {
            Command::new("date")
                .args(["-u", "+%Y-%m-%d %H:%M:%S %Z"])
                .output()
                .ok()
        })
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=LIBRESPEED_BUILD_DATE={date}");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}
