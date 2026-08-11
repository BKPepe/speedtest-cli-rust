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
///
/// The platform is named as well as the program, the way a browser does. A
/// server's telemetry stores this header already, so reporting the operating
/// system and architecture is what lets an operator tell which kinds of machine
/// are measuring against them without changing what the client sends.
///
/// It stays coarse on purpose: the kernel version or the hostname would
/// identify the machine rather than describe it.
pub fn user_agent() -> String {
    format!(
        "{UA_NAME}/{PROG_VERSION} ({}; {})",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// The JSON returned by a backend server's `getIP.php` endpoint.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GetIPResult {
    #[serde(rename = "processedString", default)]
    pub processed_string: String,
    // A backend with ISP detection disabled answers with an empty string here
    // rather than an object, so the field cannot be deserialised as the struct
    // alone -- doing so rejects the whole document and loses processedString
    // with it.
    #[serde(rename = "rawIspInfo", default, deserialize_with = "raw_isp_info")]
    pub raw_isp_info: IPInfoResponse,
}

/// Accepts unavailable or malformed ISP information without rejecting the rest
/// of an otherwise usable response.
///
/// A backend with ISP detection disabled sends an empty string here rather than
/// an object. Rejecting the document over it would throw away processedString
/// as well, which is the part actually shown to the user, so ISP information
/// that cannot be read is reported as absent -- which is also what the Go
/// client settles on after its own fallback.
///
/// This is deliberately broad, and it is worth being clear about the cost: a
/// backend that started sending something unexpected here would be tolerated
/// silently rather than reported. That trade is made because this field is
/// supplementary, while processedString is not.
fn raw_isp_info<'de, D>(d: D) -> Result<IPInfoResponse, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Info(Box<IPInfoResponse>),
        Unreadable(serde::de::IgnoredAny),
    }

    Ok(match Raw::deserialize(d)? {
        Raw::Info(info) => *info,
        Raw::Unreadable(_) => IPInfoResponse::default(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_names_the_program_and_the_platform() {
        let ua = user_agent();
        assert!(
            ua.starts_with(&format!("{UA_NAME}/{PROG_VERSION} (")),
            "unexpected prefix: {ua}"
        );
        assert!(ua.ends_with(')'), "unterminated platform: {ua}");
        assert!(
            ua.contains(std::env::consts::OS),
            "no operating system: {ua}"
        );
        assert!(ua.contains(std::env::consts::ARCH), "no architecture: {ua}");
    }

    // The Go client sends `librespeed-cli/...`, and this binary installs under
    // that same name, so a server telling the two implementations apart in its
    // telemetry has only this prefix to go on. That makes it part of the wire
    // contract rather than a cosmetic choice.
    #[test]
    fn user_agent_stays_distinguishable_from_the_go_client() {
        assert!(
            user_agent().starts_with("librespeed-cli-rust/"),
            "got {}",
            user_agent()
        );
    }

    // A backend with ISP detection disabled answers with an empty string here
    // rather than an object, which is a different JSON type than the field
    // declares.
    #[test]
    fn get_ip_result_tolerates_empty_raw_isp_info() {
        let json = r#"{"processedString":"192.0.2.1","rawIspInfo":""}"#;
        let parsed: Result<GetIPResult, _> = serde_json::from_str(json);
        assert!(parsed.is_ok(), "rejected: {:?}", parsed.err());
        assert_eq!(parsed.unwrap().processed_string, "192.0.2.1");
    }

    #[test]
    fn get_ip_result_keeps_processed_string_whatever_raw_isp_info_holds() {
        for raw in ["\"\"", "123", "null", "[]", "\"unavailable\""] {
            let json = format!(r#"{{"processedString":"192.0.2.1","rawIspInfo":{raw}}}"#);
            let parsed: Result<GetIPResult, _> = serde_json::from_str(&json);
            assert!(parsed.is_ok(), "rejected {raw}: {:?}", parsed.err());
            assert_eq!(parsed.unwrap().processed_string, "192.0.2.1", "for {raw}");
        }
    }

    #[test]
    fn get_ip_result_still_reads_a_real_object() {
        let json = r#"{"processedString":"x","rawIspInfo":{"ip":"192.0.2.1","country":"CZ"}}"#;
        let parsed: GetIPResult = serde_json::from_str(json).expect("should parse");
        assert_eq!(parsed.raw_isp_info.ip, "192.0.2.1");
        assert_eq!(parsed.raw_isp_info.country, "CZ");
    }
}
