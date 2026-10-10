use crate::archive::{
    all_fulfilled, first_unfulfilled_match, init_target_states, warn_unfulfilled,
};
use crate::Config;
use log::debug;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Walk the zip once, fulfilling each `ExtractionTarget` in `conf` on
/// the first archive entry whose basename matches that target's
/// pattern. Files are written into `output_dir`. Returns the on-disk
/// paths actually written. Targets that never match are warned about
/// but do not error — keeps a typo'd plural entry from killing the run.
pub fn extract_target_from_zipfile(
    compressed: &mut [u8],
    conf: &Config,
    output_dir: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut archive = zip::ZipArchive::new(&mut cbuf)?;

    let mut state = init_target_states(conf);
    let mut written: Vec<PathBuf> = Vec::new();
    // Reuse capacity across matches, but fully validate each entry before
    // creating its destination so corrupt data cannot overwrite an existing file.
    let mut payload = Vec::new();

    // Borrow names from the central directory without copying the full list.
    // Open only matching entries, so unreadable unrelated entries are skipped.
    for index in 0..archive.len() {
        if all_fulfilled(&state) {
            break;
        }
        let Some(fname) = archive.name_for_index(index) else {
            continue;
        };
        let entry_path = Path::new(fname);
        let Some(basename) = entry_path.file_name().and_then(|p| p.to_str()) else {
            continue;
        };
        debug!("zip, got filename: {}", basename);

        let Some(slot) = first_unfulfilled_match(&mut state, basename) else {
            continue;
        };
        let out_name = slot.target.rename_to.as_deref().unwrap_or(basename);
        let out_path = output_dir.join(out_name);
        debug!("zip, Got a match: {} -> {}", fname, out_path.display());
        let mut entry = archive.by_index(index)?;
        payload.clear();
        entry.read_to_end(&mut payload)?;
        std::fs::File::create(&out_path)?.write_all(&payload)?;
        slot.fulfilled = true;
        written.push(out_path);
    }

    warn_unfulfilled(&state);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::extract_target_from_zipfile;
    use crate::testutil::{build_test_zip, make_conf_from_ini};

    #[test]
    fn extracts_first_matching_entry_and_renames_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut zip_bytes = build_test_zip(&[
            ("first/tool", b"first binary"),
            ("second/tool", b"second binary"),
        ]);
        let conf = make_conf_from_ini(
            "tool",
            &[
                ("target_filename_to_extract_from_archive", "tool"),
                ("desired_filename", "renamed-tool"),
            ],
        );

        let written = extract_target_from_zipfile(&mut zip_bytes, &conf, dir.path()).unwrap();

        assert_eq!(written, vec![dir.path().join("renamed-tool")]);
        assert_eq!(
            std::fs::read(dir.path().join("renamed-tool")).unwrap(),
            b"first binary"
        );
        assert!(!dir.path().join("tool").exists());
    }

    #[test]
    fn skips_unmatched_entry_with_invalid_local_header() {
        let dir = tempfile::tempdir().unwrap();
        let mut zip_bytes = build_test_zip(&[("README.md", b"unwanted"), ("tool", b"binary")]);
        // Keep the central directory intact, but make opening README.md fail.
        assert_eq!(&zip_bytes[..4], b"PK\x03\x04");
        zip_bytes[..4].copy_from_slice(b"BAD!");
        let conf = make_conf_from_ini(
            "tool",
            &[("target_filenames_to_extract_from_archive", r#"["tool"]"#)],
        );

        let written = extract_target_from_zipfile(&mut zip_bytes, &conf, dir.path()).unwrap();

        assert_eq!(written, vec![dir.path().join("tool")]);
        assert_eq!(std::fs::read(dir.path().join("tool")).unwrap(), b"binary");
        assert!(!dir.path().join("README.md").exists());
    }

    #[test]
    fn extracts_multiple_entries_without_carrying_over_previous_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let large = vec![0x5a; 128 * 1024];
        let larger = vec![0xa5; 256 * 1024];
        let entries: &[(&str, &[u8])] = &[
            ("large", &large),
            ("small", b"short"),
            ("empty", b""),
            ("larger", &larger),
            ("last", b"last binary"),
        ];
        let mut zip_bytes = build_test_zip(entries);
        let conf = make_conf_from_ini(
            "tool",
            &[(
                "target_filenames_to_extract_from_archive",
                r#"["large", "small", "empty", "larger", "last"]"#,
            )],
        );

        let written = extract_target_from_zipfile(&mut zip_bytes, &conf, dir.path()).unwrap();

        let expected: Vec<_> = entries
            .iter()
            .map(|(name, _)| dir.path().join(name))
            .collect();
        assert_eq!(written, expected);
        for (name, contents) in entries {
            assert_eq!(std::fs::read(dir.path().join(name)).unwrap(), *contents);
        }
    }

    #[test]
    fn corrupt_later_entry_does_not_create_or_overwrite_its_destination() {
        for existing in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let dest = dir.path().join("second");
            if existing {
                std::fs::write(&dest, b"installed binary").unwrap();
            }
            let mut zip_bytes =
                build_test_zip(&[("first", b"first binary"), ("second", b"corrupt binary")]);
            // Change the last central-directory entry's expected CRC, leaving
            // its compressed bytes readable so validation fails during reading.
            let central_header = zip_bytes
                .windows(4)
                .rposition(|bytes| bytes == b"PK\x01\x02")
                .unwrap();
            zip_bytes[central_header + 16] ^= 0xff;
            let conf = make_conf_from_ini(
                "tool",
                &[(
                    "target_filenames_to_extract_from_archive",
                    r#"["first", "second"]"#,
                )],
            );

            let result = extract_target_from_zipfile(&mut zip_bytes, &conf, dir.path());

            assert!(result.is_err());
            assert_eq!(
                std::fs::read(dir.path().join("first")).unwrap(),
                b"first binary"
            );
            if existing {
                assert_eq!(std::fs::read(&dest).unwrap(), b"installed binary");
            } else {
                assert!(!dest.exists());
            }
        }
    }

    #[test]
    fn returns_written_files_when_a_target_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let mut zip_bytes = build_test_zip(&[("tool", b"binary")]);
        let conf = make_conf_from_ini(
            "tool",
            &[(
                "target_filenames_to_extract_from_archive",
                r#"["tool", "missing"]"#,
            )],
        );

        let written = extract_target_from_zipfile(&mut zip_bytes, &conf, dir.path()).unwrap();

        assert_eq!(written, vec![dir.path().join("tool")]);
        assert_eq!(std::fs::read(dir.path().join("tool")).unwrap(), b"binary");
        assert!(!dir.path().join("missing").exists());
    }
}
