use std::path::Path;
use log::{debug, warn};
use std::io::{Read, Write};
use crate::Config;

pub fn extract_target_from_zipfile(compressed: &mut [u8], conf: &Config) -> anyhow::Result<()> {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut archive = zip::ZipArchive::new(&mut cbuf)?;

    let target_filename = conf.desired_filename.as_ref().expect(
        "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
    );

    let re_pat =
        crate::make_re_target_filename(conf).expect("Failed to construct a regex for the target filename");

    for fname in archive
        .file_names()
        // What's dumb is that the borrow below `by_name` is a mutable
        // borrow, which means that an immutable borrow for
        // `archive.file_names` won't be allowed. To work around this,
        // for now just collect all the filenames into a long list.
        // Since we're looking for a specific name, it would be more
        // efficient to first find the name, leave the loop, and in the
        // next section do the extraction.
        .map(String::from)
        .collect::<Vec<String>>()
    {
        let mut file = archive.by_name(&fname)?;
        let path = Path::new(&fname);
        debug!(
            "zip, got filename: {}",
            &path.file_name().unwrap().to_str().unwrap()
        );
        if let Some(p) = &path.file_name() {
            if re_pat.is_match(p.to_str().unwrap()) {
                debug!("zip, Got a match: {}", &fname);
                let mut rawfile = std::fs::File::create(&target_filename)?;
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)?;
                rawfile.write_all(&buf)?;
                return Ok(());
            }
        }
    }

    warn!(
        "Failed to find file inside archive: \"{}\"",
        &target_filename
    );

    Ok(())
}
