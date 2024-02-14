use log::{debug, trace, warn};
use crate::Config;

pub fn extract_target_from_tarfile(compressed: &mut [u8], conf: &Config) {
    // std::fs::write("compressed.tar.gz", &compressed).unwrap();

    let mut cbuf = std::io::Cursor::new(compressed);
    let gzip_archive = flate2::read::GzDecoder::new(&mut cbuf);
    let mut archive = tar::Archive::new(gzip_archive);

    let target_filename = conf.desired_filename.as_ref().expect(
        "To extract from an archive, a target filename must be supplied using the \
        parameter \"target_filename_to_extract_from_archive\" in the config file.",
    );
    let re_pat =
        crate::make_re_target_filename(conf).expect("Failed to construct a regex for the target filename");

    for file in archive.entries().unwrap() {
        let mut file = file.unwrap();
        trace!("This is what I found in the tar.xz: {:?}", &file.header());
        let raw_path = &file.header().path().unwrap();
        debug!(
            "tar.gz, got filename: {}",
            &raw_path.file_name().unwrap().to_str().unwrap()
        );

        if let Some(p) = &raw_path.file_name() {
            if let Some(pm) = p.to_str() {
                if re_pat.is_match(pm) {
                    debug!("tar.gz, Got a match: {}", &pm);
                    file.unpack(&target_filename).unwrap();
                    return;
                }
            }
        }
    }

    warn!(
        "Failed to find file \"{}\" inside archive",
        &target_filename
    );
}
