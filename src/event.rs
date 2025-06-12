#[derive(Debug, Clone)]
pub enum ProgressEvent {
    PackageCheckStart { name: String },
    PackageCheckEnd { name: String },
    PackageUpToDate { name: String, version: String },
    PackageNeedsUpdate { name: String, current: String, latest: String },
    PackageDownload { name: String, progress: f32 },
    PackageUpdated { name: String, version: String },
    NoMoreWork,
}
