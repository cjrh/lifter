//! Shared fixtures for archive-layer tests.
//!
//! Lives behind `#[cfg(test)]` and is `pub(crate)` so any module's
//! `#[cfg(test)] mod tests` can use these helpers — saves the next
//! test author from grepping `~/.cargo/registry` to rediscover the
//! `tar::Builder` and `zip::ZipWriter` APIs.

use crate::{build_extraction_targets, Config};
use std::collections::HashMap;
use std::io::Write;

/// Build an in-memory uncompressed tar archive containing the given
/// `(name, contents)` entries. Use with `tar::Archive::new(Cursor::new(bytes))`.
pub(crate) fn build_test_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_path(name).unwrap();
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, *data).unwrap();
    }
    builder.into_inner().unwrap()
}

/// Build an in-memory zip archive containing the given `(name,
/// contents)` entries. Use with `zip::ZipArchive::new(Cursor::new(bytes))`.
pub(crate) fn build_test_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let buf = std::io::Cursor::new(Vec::new());
    let mut zw = zip::ZipWriter::new(buf);
    let opts = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(data).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

/// Build a `Config` whose `extraction_targets` are derived from the
/// given INI key/value pairs (as if they appeared under `[section]`).
/// All other fields default — useful for testing extractors in
/// isolation.
pub(crate) fn make_conf_from_ini(section: &str, pairs: &[(&str, &str)]) -> Config {
    let tmp: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut conf = Config::new();
    conf.extraction_targets = build_extraction_targets(section, &tmp).unwrap();
    conf
}
