//! Shared archive-walking helpers.
//!
//! Each of `zipfile`, `tarfile`, and `tarxzfile` runs the same
//! algorithm: walk archive entries once, and for each entry, find the
//! first unfulfilled `ExtractionTarget` whose pattern matches the
//! entry's basename. Write the entry to the target's `rename_to` (if
//! set) or to its original basename, mark the target fulfilled,
//! continue. This module owns the shared state machine and the tar
//! walker that the two tar variants both use.
//!
//! The zip case lives in `zipfile.rs` because `ZipArchive` requires
//! random access (`by_index`) rather than the streaming iteration the
//! tar reader provides.

use crate::{Config, ExtractionTarget};
use log::{debug, warn};
use std::path::{Path, PathBuf};

pub(crate) struct TargetState<'a> {
    pub target: &'a ExtractionTarget,
    pub fulfilled: bool,
}

pub(crate) fn init_target_states(conf: &Config) -> Vec<TargetState<'_>> {
    conf.extraction_targets
        .iter()
        .map(|t| TargetState {
            target: t,
            fulfilled: false,
        })
        .collect()
}

/// Find the first unfulfilled target whose pattern matches `basename`.
pub(crate) fn first_unfulfilled_match<'a, 'b>(
    state: &'a mut [TargetState<'b>],
    basename: &str,
) -> Option<&'a mut TargetState<'b>> {
    state
        .iter_mut()
        .find(|s| !s.fulfilled && s.target.pattern.is_match(basename))
}

pub(crate) fn all_fulfilled(state: &[TargetState]) -> bool {
    state.iter().all(|s| s.fulfilled)
}

pub(crate) fn warn_unfulfilled(state: &[TargetState]) {
    for s in state.iter().filter(|s| !s.fulfilled) {
        warn!(
            "Failed to find file inside archive matching: \"{}\"",
            s.target.pattern_str
        );
    }
}

/// Walk a tar archive (already wrapped in whatever decompressor the
/// caller needs), extracting one file per `ExtractionTarget` in
/// `conf` into `output_dir`. Returns the on-disk paths actually
/// written. Unfulfilled targets are warned about; an unreadable
/// entry is logged and skipped rather than failing the whole archive.
pub(crate) fn extract_targets_from_tar<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
    conf: &Config,
    output_dir: &Path,
) -> Vec<PathBuf> {
    let mut state = init_target_states(conf);
    let mut written: Vec<PathBuf> = Vec::new();

    let entries = match archive.entries() {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to iterate tar entries: {}", e);
            return written;
        }
    };

    for file in entries {
        if all_fulfilled(&state) {
            break;
        }
        let mut file = match file {
            Ok(f) => f,
            Err(e) => {
                debug!("Skipping unreadable tar entry: {}", e);
                continue;
            }
        };
        // Resolve GNU/PAX paths and borrow the basename instead of copying it.
        let path = match file.path() {
            Ok(p) => p,
            Err(e) => {
                debug!("Skipping tar entry with bad path: {}", e);
                continue;
            }
        };
        let basename = match path.file_name().and_then(|p| p.to_str()) {
            Some(b) => b,
            None => continue,
        };
        debug!("tar, got filename: {}", basename);

        let Some(slot) = first_unfulfilled_match(&mut state, basename) else {
            continue;
        };
        let out_name = slot.target.rename_to.as_deref().unwrap_or(basename);
        let out_path = output_dir.join(out_name);
        debug!("tar, Got a match: {} -> {}", basename, out_path.display());
        if let Err(e) = file.unpack(&out_path) {
            warn!("Failed to unpack {}: {}", out_path.display(), e);
            continue;
        }
        slot.fulfilled = true;
        written.push(out_path);
    }

    warn_unfulfilled(&state);
    written
}

#[cfg(test)]
mod tests {
    use super::extract_targets_from_tar;
    use crate::testutil::make_conf_from_ini;
    use std::io::Cursor;

    #[test]
    fn extracts_gnu_long_path_and_renames_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = format!("{}tool", "nested/".repeat(20));
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(6);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, &path, &b"binary"[..])
            .unwrap();
        let tar_bytes = builder.into_inner().unwrap();
        let conf = make_conf_from_ini(
            "tool",
            &[
                ("target_filename_to_extract_from_archive", "tool"),
                ("desired_filename", "renamed-tool"),
            ],
        );

        let mut archive = tar::Archive::new(Cursor::new(tar_bytes));
        let written = extract_targets_from_tar(&mut archive, &conf, dir.path());

        assert_eq!(written, vec![dir.path().join("renamed-tool")]);
        assert_eq!(std::fs::read(&written[0]).unwrap(), b"binary");
        assert!(!dir.path().join("tool").exists());
    }

    #[test]
    fn matches_pax_path_instead_of_raw_header_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_pax_extensions([("path", &b"nested/resolved-tool"[..])])
            .unwrap();
        let mut header = tar::Header::new_ustar();
        header.set_size(6);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, "header-tool", &b"binary"[..])
            .unwrap();
        let tar_bytes = builder.into_inner().unwrap();
        let conf = make_conf_from_ini(
            "tool",
            &[(
                "target_filenames_to_extract_from_archive",
                r#"["resolved-tool", "header-tool"]"#,
            )],
        );

        let mut archive = tar::Archive::new(Cursor::new(tar_bytes));
        let written = extract_targets_from_tar(&mut archive, &conf, dir.path());

        assert_eq!(written, vec![dir.path().join("resolved-tool")]);
        assert_eq!(std::fs::read(&written[0]).unwrap(), b"binary");
        assert!(!dir.path().join("header-tool").exists());
    }
}
