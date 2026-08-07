//! Records the build date so `--version` can report it.

fn main() {
    // SOURCE_DATE_EPOCH keeps reproducible builds reproducible; OpenWrt and
    // most distro packaging set it. Computing the date here rather than
    // shelling out to `date` avoids depending on GNU vs BSD flag differences.
    let date = match std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|epoch| epoch.parse::<i64>().ok())
        .and_then(|epoch| chrono::DateTime::from_timestamp(epoch, 0))
    {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        None => chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string(),
    };

    println!("cargo:rustc-env=LIBRESPEED_BUILD_DATE={date}");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}
