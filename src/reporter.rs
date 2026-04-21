//! Structured per-section output for shell pipelines.
//!
//! Each section of the config produces exactly one CSV row on stdout, no
//! header, regardless of outcome. Logging (on stderr) is for humans;
//! this channel is for tooling. The six columns are:
//!
//! ```text
//! timestamp,was_updated,tool_name,file_name,previous_version,current_version
//! ```
//!
//! `timestamp` is UTC RFC 3339, second precision (`YYYY-MM-DDTHH:MM:SSZ`).
//! Rows are serialized atomically under a mutex so parallel sections
//! never interleave. Flushed per row so a consumer piping to
//! `grep`/`awk` sees each section's result as soon as it finishes.

use std::io::{self, Stdout, Write};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// One outcome for one section of the config.
///
/// `file_name` and `previous_version` are `Option` because we may not
/// have computed them yet (e.g. a section missing required fields
/// bails before `desired_filename` is resolved). `current_version` is
/// `Option` because we may not have reached the remote at all.
pub struct OutputRecord {
    pub was_updated: bool,
    pub tool_name: String,
    pub file_name: Option<String>,
    pub previous_version: Option<String>,
    pub current_version: Option<String>,
}

/// Serializes `OutputRecord`s as CSV rows to stdout, one row per call,
/// safely across threads.
pub struct Reporter {
    out: Mutex<Stdout>,
}

impl Reporter {
    pub fn new() -> Self {
        Reporter {
            out: Mutex::new(io::stdout()),
        }
    }

    /// Write one CSV row. Locks stdout, writes the row + newline,
    /// flushes. Any IO error (e.g. broken pipe when a downstream
    /// consumer closed early) is intentionally swallowed — it's not a
    /// failure of the section's work.
    pub fn emit(&self, r: &OutputRecord) {
        let line = format_row(&now_rfc3339_utc(), r);
        if let Ok(mut handle) = self.out.lock() {
            let _ = handle.write_all(line.as_bytes());
            let _ = handle.flush();
        }
    }
}

impl Default for Reporter {
    fn default() -> Self {
        Self::new()
    }
}

/// Format a single CSV row, newline-terminated. Fields are quoted
/// (with doubled internal quotes) iff they contain `,`, `"`, `\n`, or
/// `\r`; otherwise emitted verbatim. This matches RFC 4180 for the
/// cases lifter actually produces.
fn format_row(timestamp: &str, r: &OutputRecord) -> String {
    let mut buf = String::with_capacity(128);
    append_field(&mut buf, timestamp);
    buf.push(',');
    append_field(&mut buf, if r.was_updated { "1" } else { "0" });
    buf.push(',');
    append_field(&mut buf, &r.tool_name);
    buf.push(',');
    append_field(&mut buf, r.file_name.as_deref().unwrap_or(""));
    buf.push(',');
    append_field(&mut buf, r.previous_version.as_deref().unwrap_or(""));
    buf.push(',');
    append_field(&mut buf, r.current_version.as_deref().unwrap_or(""));
    buf.push('\n');
    buf
}

/// Current wall-clock time as RFC 3339 UTC, second precision.
/// Falls back to the epoch if the system clock is somehow before
/// 1970-01-01 — not worth propagating an error for row output.
fn now_rfc3339_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_rfc3339_utc(secs)
}

/// Seconds-since-epoch → `YYYY-MM-DDTHH:MM:SSZ`.
///
/// The civil-date arithmetic is Howard Hinnant's `civil_from_days`
/// (public domain), which is branch-light, valid for any year in
/// `i32`, and doesn't need a table or an external crate.
fn format_rfc3339_utc(secs_since_epoch: i64) -> String {
    let days = secs_since_epoch.div_euclid(86_400);
    let tod = secs_since_epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let h = tod / 3600;
    let mi = (tod % 3600) / 60;
    let s = tod % 60;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, h, mi, s)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn append_field(buf: &mut String, field: &str) {
    if field
        .bytes()
        .any(|b| b == b',' || b == b'"' || b == b'\n' || b == b'\r')
    {
        buf.push('"');
        for c in field.chars() {
            if c == '"' {
                buf.push_str("\"\"");
            } else {
                buf.push(c);
            }
        }
        buf.push('"');
    } else {
        buf.push_str(field);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: &str = "2026-04-21T12:00:00Z";

    fn rec(
        was_updated: bool,
        tool: &str,
        file: Option<&str>,
        prev: Option<&str>,
        curr: Option<&str>,
    ) -> OutputRecord {
        OutputRecord {
            was_updated,
            tool_name: tool.to_string(),
            file_name: file.map(String::from),
            previous_version: prev.map(String::from),
            current_version: curr.map(String::from),
        }
    }

    #[test]
    fn updated_row() {
        let r = rec(true, "ripgrep", Some("rg"), Some("13.0.0"), Some("14.1.0"));
        assert_eq!(
            format_row(TS, &r),
            "2026-04-21T12:00:00Z,1,ripgrep,rg,13.0.0,14.1.0\n"
        );
    }

    #[test]
    fn not_updated_row() {
        let r = rec(false, "ripgrep", Some("rg"), Some("14.1.0"), Some("14.1.0"));
        assert_eq!(
            format_row(TS, &r),
            "2026-04-21T12:00:00Z,0,ripgrep,rg,14.1.0,14.1.0\n"
        );
    }

    #[test]
    fn blank_optionals_left_empty() {
        let r = rec(false, "broken", None, Some("1.0"), None);
        assert_eq!(format_row(TS, &r), "2026-04-21T12:00:00Z,0,broken,,1.0,\n");
    }

    #[test]
    fn tool_name_with_space_is_unquoted() {
        // Spaces alone don't require quoting under RFC 4180.
        let r = rec(
            true,
            "ripgrep Windows",
            Some("rg.exe"),
            Some("13.0.0"),
            Some("14.1.0"),
        );
        assert_eq!(
            format_row(TS, &r),
            "2026-04-21T12:00:00Z,1,ripgrep Windows,rg.exe,13.0.0,14.1.0\n"
        );
    }

    #[test]
    fn comma_in_field_triggers_quoting() {
        let r = rec(true, "weird,tool", Some("x"), Some("1"), Some("2"));
        assert_eq!(
            format_row(TS, &r),
            "2026-04-21T12:00:00Z,1,\"weird,tool\",x,1,2\n"
        );
    }

    #[test]
    fn quote_in_field_is_doubled() {
        let r = rec(true, r#"with"quote"#, Some("x"), Some("1"), Some("2"));
        assert_eq!(
            format_row(TS, &r),
            "2026-04-21T12:00:00Z,1,\"with\"\"quote\",x,1,2\n"
        );
    }

    #[test]
    fn newline_in_field_triggers_quoting() {
        let r = rec(true, "multi\nline", Some("x"), Some("1"), Some("2"));
        assert_eq!(
            format_row(TS, &r),
            "2026-04-21T12:00:00Z,1,\"multi\nline\",x,1,2\n"
        );
    }

    #[test]
    fn rfc3339_unix_epoch() {
        assert_eq!(format_rfc3339_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn rfc3339_known_point() {
        // 2024-02-29T13:45:30Z — leap day, to exercise civil_from_days.
        // `date -u -d '2024-02-29T13:45:30Z' +%s` = 1709214330
        assert_eq!(format_rfc3339_utc(1_709_214_330), "2024-02-29T13:45:30Z");
    }

    #[test]
    fn rfc3339_century_boundary() {
        // 2000-01-01T00:00:00Z, a century *not* skipped by the Gregorian rule.
        assert_eq!(format_rfc3339_utc(946_684_800), "2000-01-01T00:00:00Z");
    }

    #[test]
    fn now_is_shaped_correctly() {
        // Don't pin a value; just sanity-check the shape (len + terminal Z).
        let ts = now_rfc3339_utc();
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
    }
}
