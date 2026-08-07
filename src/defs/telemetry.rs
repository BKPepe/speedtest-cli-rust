//! Telemetry configuration and the log shipped in the `log` telemetry field.

use std::sync::Mutex;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::util::path_join;

pub const LEVEL_DISABLED: &str = "disabled";
pub const LEVEL_BASIC: &str = "basic";
pub const LEVEL_FULL: &str = "full";
pub const LEVEL_DEBUG: &str = "debug";

pub const LEVELS: [&str; 4] = [LEVEL_DISABLED, LEVEL_BASIC, LEVEL_FULL, LEVEL_DEBUG];

/// Collects the log lines sent as the `log` telemetry field.
#[derive(Debug, Default)]
pub struct TelemetryLog {
    level: Mutex<i32>,
    content: Mutex<Vec<String>>,
}

impl TelemetryLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_level(&self, level: i32) {
        *self.level.lock().unwrap() = level;
    }

    fn level(&self) -> i32 {
        *self.level.lock().unwrap()
    }

    fn push(&self, prefix: &str, msg: &str) {
        let now = chrono::Local::now();
        // Close to Go's `time.Time.String()` layout. The zone abbreviation Go
        // prints last is omitted: it would need a tz database to resolve.
        let stamp = now.format("%Y-%m-%d %H:%M:%S%.f %z");
        self.content
            .lock()
            .unwrap()
            .push(format!("{stamp}: {prefix}{msg}"));
    }

    /// Logs when the level is at least "full".
    pub fn logf(&self, msg: impl std::fmt::Display) {
        if self.level() >= 2 {
            self.push("", &msg.to_string());
        }
    }

    /// Logs with a WARN prefix when the level is at least "full".
    ///
    /// Part of the telemetry log API for parity with the Go implementation,
    /// which likewise has no current call site.
    #[allow(dead_code)]
    pub fn warnf(&self, msg: impl std::fmt::Display) {
        if self.level() >= 2 {
            self.push("WARN: ", &msg.to_string());
        }
    }

    /// Logs when the level is at least "debug".
    ///
    /// Part of the telemetry log API for parity with the Go implementation,
    /// which likewise has no current call site.
    #[allow(dead_code)]
    pub fn verbosef(&self, msg: impl std::fmt::Display) {
        if self.level() >= 3 {
            self.push("", &msg.to_string());
        }
    }

    /// The accumulated log as a single string.
    pub fn contents(&self) -> String {
        self.content.lock().unwrap().join("\n")
    }
}

/// The `extra` field of the telemetry payload.
#[derive(Debug, Default, Serialize)]
pub struct TelemetryExtra {
    #[serde(rename = "server")]
    pub server_name: String,
    #[serde(rename = "extra", skip_serializing_if = "String::is_empty")]
    pub extra: String,
}

/// Telemetry server configuration, also the shape of `--telemetry-json`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TelemetryServer {
    #[serde(rename = "telemetryLevel", default)]
    pub level: String,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub path: String,
    #[serde(rename = "shareURL", default)]
    pub share: String,
}

impl TelemetryServer {
    /// Translates the level string to its numeric value.
    pub fn get_level(&self) -> i32 {
        match self.level.as_str() {
            LEVEL_BASIC => 1,
            LEVEL_FULL => 2,
            LEVEL_DEBUG => 3,
            _ => 0,
        }
    }

    /// The telemetry upload URL.
    pub fn get_path(&self) -> anyhow::Result<Url> {
        let base = Url::parse(&self.server)
            .with_context(|| format!("invalid telemetry server URL: {}", self.server))?;
        Ok(crate::util::url_join_path(&base, &self.path))
    }

    /// The share link URL, which always carries a trailing slash.
    pub fn get_share(&self) -> anyhow::Result<Url> {
        let base = Url::parse(&self.server)
            .with_context(|| format!("invalid telemetry server URL: {}", self.server))?;
        let joined = path_join(base.path(), &self.share);
        let joined = if joined.starts_with('/') {
            format!("{joined}/")
        } else {
            format!("/{joined}/")
        };
        let mut u = base;
        u.set_path(&joined);
        Ok(u)
    }
}
