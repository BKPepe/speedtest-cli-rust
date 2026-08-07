//! Core data types shared by the speed test.

pub mod bytes_counter;
pub mod server;
pub mod telemetry;

pub use server::Server;
pub use telemetry::{TelemetryExtra, TelemetryLog, TelemetryServer};

use serde::{Deserialize, Serialize};

/// The program name, as reported by `--version`.
pub const PROG_NAME: &str = "librespeed-cli";

/// The name reported in the User-Agent header.
///
/// The Go client sends `librespeed-cli`, and this binary installs under that
/// same name, so sharing its User-Agent would leave a server's telemetry unable
/// to tell the two implementations apart. This matches the repository and
/// package name instead.
pub const UA_NAME: &str = "librespeed-cli-rust";

/// The program version.
pub const PROG_VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// The build date, filled in by `build.rs`.
pub const BUILD_DATE: &str = env!("LIBRESPEED_BUILD_DATE");

/// The source revision, filled in by `build.rs`.
pub const REVISION: &str = env!("LIBRESPEED_REVISION");

/// The User-Agent sent with every request.
pub fn user_agent() -> String {
    format!("{UA_NAME}/{PROG_VERSION}")
}

/// The JSON returned by a backend server's `getIP.php` endpoint.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GetIPResult {
    #[serde(rename = "processedString", default)]
    pub processed_string: String,
    #[serde(rename = "rawIspInfo", default)]
    pub raw_isp_info: IPInfoResponse,
}

/// The JSON returned by IPInfo.io's API.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct IPInfoResponse {
    #[serde(default)]
    pub ip: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub country: String,
    #[serde(rename = "loc", default)]
    pub location: String,
    #[serde(rename = "org", default)]
    pub organization: String,
    #[serde(default)]
    pub postal: String,
    #[serde(default)]
    pub timezone: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub readme: String,
}
