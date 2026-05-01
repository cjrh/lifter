use crate::archive::{
    all_fulfilled, first_unfulfilled_match, init_target_states, warn_unfulfilled,
};
use crate::Config;
use log::debug;
use std::io::{Read, Write};
use std::path::Path;

/// Walk the zip once, fulfilling each `ExtractionTarget` in `conf` on
/// the first archive entry whose basename matches that target's
/// pattern. Returns the on-disk paths actually written. Targets that
/// never match are warned about but do not error — keeps a typo'd
/// plural entry from killing the run.
pub fn extract_target_from_zipfile(
    compressed: &mut [u8],
    conf: &Config,
) -> anyhow::Result<Vec<String>> {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut archive = zip::ZipArchive::new(&mut cbuf)?;

    let mut state = init_target_states(conf);
    let mut written: Vec<String> = Vec::new();

    // Collect filenames first to side-step the borrow conflict between
    // `archive.file_names()` (immutable) and `archive.by_name()` (mutable).
    let names: Vec<String> = archive.file_names().map(String::from).collect();
    for fname in names {
        if all_fulfilled(&state) {
            break;
        }
        let path = Path::new(&fname);
        let Some(basename) = path.file_name().and_then(|p| p.to_str()) else {
            continue;
        };
        debug!("zip, got filename: {}", basename);

        let Some(slot) = first_unfulfilled_match(&mut state, basename) else {
            continue;
        };
        let out_name = slot
            .target
            .rename_to
            .as_deref()
            .unwrap_or(basename)
            .to_string();
        debug!("zip, Got a match: {} -> {}", &fname, &out_name);
        let mut entry = archive.by_name(&fname)?;
        let mut payload = Vec::new();
        entry.read_to_end(&mut payload)?;
        std::fs::File::create(&out_name)?.write_all(&payload)?;
        slot.fulfilled = true;
        written.push(out_name);
    }

    warn_unfulfilled(&state);
    Ok(written)
}
