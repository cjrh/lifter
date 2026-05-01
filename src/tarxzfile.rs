use crate::archive::extract_targets_from_tar;
use crate::Config;

pub fn extract_target_from_tarxz(compressed: &mut [u8], conf: &Config) -> Vec<String> {
    let cbuf = std::io::Cursor::new(compressed);
    let decompressor = xz2::read::XzDecoder::new(cbuf);
    let mut archive = tar::Archive::new(decompressor);
    extract_targets_from_tar(&mut archive, conf)
}
