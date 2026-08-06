//! Machine-readable JSON and CSV reports.

use serde::{Serialize, Serializer};

use crate::defs::IPInfoResponse;

/// Serializes a float the way Go's `encoding/json` does, so whole numbers are
/// written as `555` rather than `555.0`.
fn go_float<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        s.serialize_i64(*v as i64)
    } else {
        s.serialize_f64(*v)
    }
}

/// Formats a timestamp the way Go's `time.Time` marshals to JSON (RFC 3339 with
/// fractional seconds, trailing zeros removed).
pub fn timestamp_now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false)
}

/// The speed test server's information in a JSON report.
#[derive(Debug, Default, Serialize)]
pub struct ReportServer {
    pub name: String,
    pub url: String,
}

/// The speed test client's information in a JSON report.
#[derive(Debug, Default, Serialize)]
pub struct Client {
    #[serde(flatten)]
    pub ip_info: IPInfoResponse,
}

/// The output data fields of a JSON report.
#[derive(Debug, Default, Serialize)]
pub struct JSONReport {
    pub timestamp: String,
    pub server: ReportServer,
    pub client: Client,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    #[serde(serialize_with = "go_float")]
    pub ping: f64,
    #[serde(serialize_with = "go_float")]
    pub jitter: f64,
    #[serde(serialize_with = "go_float")]
    pub upload: f64,
    #[serde(serialize_with = "go_float")]
    pub download: f64,
    pub share: String,
}

/// The output data fields of a CSV report.
#[derive(Debug, Default, Serialize)]
pub struct CSVReport {
    #[serde(rename = "Timestamp")]
    pub timestamp: String,
    #[serde(rename = "Server Name")]
    pub name: String,
    #[serde(rename = "Address")]
    pub address: String,
    #[serde(rename = "Ping", serialize_with = "go_float")]
    pub ping: f64,
    #[serde(rename = "Jitter", serialize_with = "go_float")]
    pub jitter: f64,
    #[serde(rename = "Download", serialize_with = "go_float")]
    pub download: f64,
    #[serde(rename = "Upload", serialize_with = "go_float")]
    pub upload: f64,
    #[serde(rename = "Share")]
    pub share: String,
    #[serde(rename = "IP")]
    pub ip: String,
}

/// The CSV column names, in order.
pub const CSV_HEADERS: [&str; 9] = [
    "Timestamp",
    "Server Name",
    "Address",
    "Ping",
    "Jitter",
    "Download",
    "Upload",
    "Share",
    "IP",
];

fn writer(delimiter: u8) -> csv::Writer<Vec<u8>> {
    csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(Vec::new())
}

/// Renders just the CSV header row, for `--csv-header`.
pub fn csv_header(delimiter: u8) -> anyhow::Result<String> {
    let mut w = writer(delimiter);
    w.write_record(CSV_HEADERS)?;
    Ok(String::from_utf8(w.into_inner()?)?)
}

/// Renders CSV rows without a header, with no trailing newline.
pub fn csv_rows(reports: &[CSVReport], delimiter: u8) -> anyhow::Result<String> {
    let mut w = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .from_writer(Vec::new());

    for rep in reports {
        w.serialize(rep)?;
    }

    let out = String::from_utf8(w.into_inner()?)?;
    Ok(out.trim_end_matches(['\n', '\r']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_uses_the_configured_delimiter() {
        assert_eq!(
            csv_header(b',').unwrap().trim_end(),
            "Timestamp,Server Name,Address,Ping,Jitter,Download,Upload,Share,IP"
        );
        assert_eq!(
            csv_header(b';').unwrap().trim_end(),
            "Timestamp;Server Name;Address;Ping;Jitter;Download;Upload;Share;IP"
        );
    }

    #[test]
    fn rows_quote_embedded_delimiters_and_omit_trailing_newline() {
        let rep = CSVReport {
            timestamp: "2026-08-06T15:41:36.067293+02:00".into(),
            name: "Prague, Czech Republic (CESNET)".into(),
            address: "https://speedtest.cesnet.cz".into(),
            ping: 6.36,
            jitter: 0.82,
            download: 228.39,
            upload: 553.97,
            ..Default::default()
        };
        let out = csv_rows(&[rep], b',').unwrap();
        assert_eq!(
            out,
            "2026-08-06T15:41:36.067293+02:00,\"Prague, Czech Republic (CESNET)\",https://speedtest.cesnet.cz,6.36,0.82,228.39,553.97,,"
        );
    }
}
