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
    // Kept verbatim rather than typed: a backend with ISP detection disabled
    // answers with an empty string here rather than an object, so a typed
    // field would reject the whole document and lose processedString with it.
    // Telemetry also sends this document back, and it should report what the
    // backend originally said, not this client's re-encoding of it. The typed
    // view is available through ip_info().
    #[serde(rename = "rawIspInfo", default)]
    pub raw_isp_info: serde_json::Value,
}

impl GetIPResult {
    /// The typed view of rawIspInfo. Anything that is not an object -- an
    /// empty string, null, or an absent field -- reads as no ISP information,
    /// which is also what the Go client settles on.
    pub fn ip_info(&self) -> IPInfoResponse {
        // as_name is an input alias for org, not a report field: it is read
        // here and never serialized back out.
        #[derive(Deserialize, Default)]
        struct Raw {
            #[serde(flatten)]
            info: IPInfoResponse,
            #[serde(rename = "as_name", default)]
            as_name: String,
        }

        let raw: Raw = serde_json::from_value(self.raw_isp_info.clone()).unwrap_or_default();
        let mut info = raw.info;

        // Current backends use as_name, while older ones use org.
        if info.organization.is_empty() {
            info.organization = raw.as_name;
        }

        info
    }

    /// The client address this measurement was made from.
    ///
    /// Taken from rawIspInfo when present, but a backend with ISP detection
    /// disabled puts the address only in processedString -- bare, or as the
    /// leading token of `<address> - <ISP>` -- so the first token stands in
    /// when it parses as an address. The report carries this field, and it is
    /// what lets a consumer tell an IPv4 measurement from an IPv6 one.
    pub fn ip(&self) -> String {
        let from_raw = self.ip_info().ip;
        if !from_raw.is_empty() {
            return from_raw;
        }

        match self.processed_string.split_whitespace().next() {
            Some(tok) if tok.parse::<std::net::IpAddr>().is_ok() => tok.to_string(),
            _ => String::new(),
        }
    }
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
        assert_eq!(parsed.ip_info().ip, "192.0.2.1");
        assert_eq!(parsed.ip_info().country, "CZ");
        assert_eq!(parsed.ip(), "192.0.2.1");
    }

    // The address lives only in processedString whenever rawIspInfo carries
    // none: live backends answer with an empty string, a JSON null, or -- with
    // the current ipinfo schema -- an object that has no ip field at all.
    #[test]
    fn client_address_recovered_from_processed_string() {
        for raw in [
            r#""""#,
            "null",
            r#"{"as_name":"O2 Czech Republic, a.s.","asn":"AS5610"}"#,
        ] {
            for (processed, want) in [
                (
                    "2a00:1028:8388:a84e:8cda:7223:57f8:67a6",
                    "2a00:1028:8388:a84e:8cda:7223:57f8:67a6",
                ),
                ("192.0.2.1 - Example ISP, CZ", "192.0.2.1"),
                ("no address here", ""),
                ("", ""),
            ] {
                let json = format!(r#"{{"processedString":"{processed}","rawIspInfo":{raw}}}"#);
                let parsed: GetIPResult = serde_json::from_str(&json).expect("should parse");
                assert_eq!(parsed.ip(), want, "for {processed:?} with raw {raw}");
            }
        }
    }

    // as_name is an input alias for org, not a report field: accepting it
    // must not start emitting it.
    #[test]
    fn ip_info_does_not_leak_as_name() {
        let json = r#"{"processedString":"x","rawIspInfo":{"as_name":"O2 Czech Republic, a.s."}}"#;
        let parsed: GetIPResult = serde_json::from_str(json).expect("should parse");
        let out = serde_json::to_string(&parsed.ip_info()).expect("serialize");
        assert!(!out.contains("as_name"), "leaked: {out}");
        assert!(
            out.contains(r#""org":"O2 Czech Republic, a.s.""#),
            "org missing: {out}"
        );
    }

    // org takes precedence over as_name; a backend sending only the current
    // ipinfo schema still yields a filled organisation.
    #[test]
    fn organisation_recovered_from_as_name() {
        for (raw, want) in [
            (
                r#"{"ip":"192.0.2.1","org":"AS64496 Example"}"#,
                "AS64496 Example",
            ),
            (
                r#"{"as_name":"O2 Czech Republic, a.s.","asn":"AS5610"}"#,
                "O2 Czech Republic, a.s.",
            ),
            (r#"{"org":"Old Name","as_name":"New Name"}"#, "Old Name"),
            (r#"{"ip":"192.0.2.1"}"#, ""),
        ] {
            let json = format!(r#"{{"processedString":"x","rawIspInfo":{raw}}}"#);
            let parsed: GetIPResult = serde_json::from_str(&json).expect("should parse");
            assert_eq!(parsed.ip_info().organization, want, "for {raw}");
        }
    }

    // rawIspInfo.ip wins over whatever processedString starts with: the typed
    // field is the backend's own answer, the token is only a stand-in.
    #[test]
    fn raw_address_takes_precedence_over_processed_string() {
        let json = r#"{"processedString":"198.51.100.7 - X","rawIspInfo":{"ip":"192.0.2.1"}}"#;
        let parsed: GetIPResult = serde_json::from_str(json).expect("should parse");
        assert_eq!(parsed.ip(), "192.0.2.1");
    }

    // Telemetry sends the getIP document back to the server; what the backend
    // said must survive the round trip unchanged, not come back re-typed.
    #[test]
    fn raw_isp_info_round_trips_verbatim() {
        for raw in [r#""""#, "null", r#"{"ip":"192.0.2.1"}"#, r#""unavailable""#] {
            let json = format!(r#"{{"processedString":"x","rawIspInfo":{raw}}}"#);
            let parsed: GetIPResult = serde_json::from_str(&json).expect("should parse");
            let back = serde_json::to_value(&parsed).expect("should serialize");
            let orig: serde_json::Value = serde_json::from_str(&json).expect("valid");
            assert_eq!(back["rawIspInfo"], orig["rawIspInfo"], "for {raw}");
        }
    }
}
