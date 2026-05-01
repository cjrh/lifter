use crate::archive::extract_targets_from_tar;
use crate::Config;

pub fn extract_target_from_tarfile(compressed: &mut [u8], conf: &Config) -> Vec<String> {
    let cbuf = std::io::Cursor::new(compressed);
    let gzip_archive = flate2::read::GzDecoder::new(cbuf);
    let mut archive = tar::Archive::new(gzip_archive);
    extract_targets_from_tar(&mut archive, conf)
}
