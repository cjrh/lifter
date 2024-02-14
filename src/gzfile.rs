use crate::Config;
use std::io::{Read, Seek, Write};

pub fn extract_target_from_gzfile(compressed: &mut [u8], conf: &Config) {
    let mut cbuf = std::io::Cursor::new(compressed);
    let mut archive = flate2::read::GzDecoder::new(&mut cbuf);

    let target_filename = conf.desired_filename.as_ref().expect(
        "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
    );

    // If it's only `.gz` (and not `.tar.gz`) then it's a single file, so we don't
    // worry about trying to match a regex, just save whatever is there into the
    // `desired_filename`.

    let mut buf = vec![];
    archive.read_to_end(&mut buf).unwrap();
    match std::fs::File::create(target_filename) {
        Ok(mut file) => {
            file.seek(std::io::SeekFrom::Start(0)).unwrap();
            file.write_all(&buf).unwrap();
        }
        Err(e) => {
            eprintln!("Failed to create file {}: {}", &target_filename, e);
        }
    }
    // let mut file = std::fs::File::create(target_filename).unwrap();
    // file.seek(std::io::SeekFrom::Start(0)).unwrap();
    // file.write_all(&buf).unwrap();
}
