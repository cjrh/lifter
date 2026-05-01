use crate::Config;
use anyhow::{anyhow, Result};
use std::io::{Read, Write};

/// A `.gz` file (as opposed to `.tar.gz`) is a single compressed
/// file: there's no archive member to match against, so the plural
/// `target_filenames_to_extract_from_archive` form makes no sense
/// here. We require the singular form (which always sets `rename_to`)
/// and write the decompressed bytes there.
pub fn extract_target_from_gzfile(
    section: &str,
    compressed: &[u8],
    conf: &Config,
) -> Result<Vec<String>> {
    let target = conf.singular_target().ok_or_else(|| {
        anyhow!(
            "[{}] target_filenames_to_extract_from_archive cannot be used \
             with a `.gz` download (single-file format, nothing to match)",
            section
        )
    })?;
    let dest = target
        .rename_to
        .as_deref()
        .expect("singular_target guarantees rename_to is Some");

    let cbuf = std::io::Cursor::new(compressed);
    let mut decoder = flate2::read::GzDecoder::new(cbuf);
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf)?;
    std::fs::File::create(dest)?.write_all(&buf)?;
    Ok(vec![dest.to_string()])
}
