use std::collections::HashMap;
use lifter::event::ProgressEvent;

/// Holds the application state for the TUI
pub struct App {
    /// Active worker tasks, by section_name
    pub active_jobs: Vec<String>,
    /// List of packages that have been updated, section_name
    pub updated: Vec<String>,
    /// Any errors encountered during processing
    pub errors: Vec<(String, String)>, // (section_name, error_message)
    pub downloads: HashMap<String, f32>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            active_jobs: Vec::new(),
            updated: Vec::new(),
            errors: Vec::new(),
            downloads: HashMap::default(),
        }
    }
}

impl App {
    /// Optional: Called on regular intervals to update animations
    pub fn on_tick(&mut self) {
        // Update any animations, progress bars, timers here
    }
    
    pub fn handle_event(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::PackageCheckStart { name } => {
                self.active_jobs.push(format!("Checking {}", name));
            }
            ProgressEvent::PackageCheckEnd { name } => {
                self.active_jobs.retain(|desc| !desc.contains(&name));
            }
            ProgressEvent::PackageUpToDate { name, version } => {
                self.active_jobs.retain(|desc| !desc.contains(&name));
            }
            ProgressEvent::PackageNeedsUpdate { name, current, latest } => {
                // self.active_jobs.retain(|_, desc| !desc.contains(&name));
            }
            ProgressEvent::PackageDownload { name, progress } => {
                let value = self.downloads.entry(name).or_insert(0.0);
                *value = progress.max(progress);
            }
            ProgressEvent::PackageUpdated { name, version } => {
                self.active_jobs.retain(|desc| !desc.contains(&name));
                self.downloads.remove(&name);
                self.updated.push(format!("Updated {} to version {}", name, version));
            }
            ProgressEvent::NoMoreWork => {
                self.active_jobs.clear();
                self.downloads.clear();
            }
        }
    }
}
