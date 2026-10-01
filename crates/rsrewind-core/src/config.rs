//! `config.toml`: few knobs, strong defaults.
//!
//! Every field has a default so a partial or empty file is valid, and unknown keys are rejected so
//! a typo (`excluded_proceses`) fails loudly instead of silently disabling a privacy rule.

use crate::{CoreError, PrivacyPolicy, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub capture: CaptureConfig,
    pub storage: StorageConfig,
    pub ocr: OcrConfig,
    pub privacy: PrivacyPolicy,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    /// Candidate frames per second per monitor. Most candidates are discarded as unchanged.
    pub fps_candidate: f32,
    /// Fraction of the downscaled fingerprint that must differ (0.0–1.0) to store a new state.
    pub change_threshold: f32,
    /// WebP quality, 0–100. Screenshots are mostly text; 80 keeps it crisp.
    pub image_quality: u8,
    /// Stop storing new states after this many seconds without keyboard/mouse input.
    pub idle_after_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Soft cap on media + database size. Oldest history is pruned first. 0 disables.
    pub max_size_gb: f32,
    /// Delete history older than this many days. 0 keeps everything.
    pub retention_days: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OcrConfig {
    pub enabled: bool,
    /// BCP-47 tag (e.g. `en-US`). Empty = the user's profile languages.
    pub language: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// `tracing` filter directive, e.g. `info` or `rsrewind_capture=debug`.
    pub level: String,
    /// Log recognized text and window titles. Off by default: logs are not a second copy of
    /// your history.
    pub log_captured_content: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            capture: CaptureConfig::default(),
            storage: StorageConfig::default(),
            ocr: OcrConfig::default(),
            privacy: PrivacyPolicy::suggested_defaults(),
            logging: LoggingConfig::default(),
        }
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            fps_candidate: 1.0,
            change_threshold: 0.02,
            image_quality: 80,
            idle_after_secs: 300,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            max_size_gb: 50.0,
            retention_days: 90,
        }
    }
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            language: String::new(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            log_captured_content: false,
        }
    }
}

impl Config {
    /// Loads `path`, or returns defaults if it does not exist. Invalid files are an error, never
    /// silently replaced: the file may hold privacy rules the user is relying on.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|message| CoreError::Config {
                path: path.to_path_buf(),
                message,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(CoreError::io(path, error)),
        }
    }

    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }

    /// Writes the defaults with explanatory comments if no config exists yet.
    pub fn write_default_if_missing(path: &Path) -> Result<bool> {
        if path.exists() {
            return Ok(false);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::io(parent, e))?;
        }
        let body = toml::to_string_pretty(&Self::default()).map_err(|e| CoreError::Config {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let text = format!(
            "# rsRewind configuration. Delete this file to restore defaults.\n\
             # Everything rsRewind records stays in this folder; nothing is uploaded.\n\n{body}"
        );
        std::fs::write(path, text).map_err(|e| CoreError::io(path, e))?;
        Ok(true)
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        let c = &self.capture;
        if !(c.fps_candidate > 0.0 && c.fps_candidate <= 10.0) {
            return Err("capture.fps_candidate must be in (0, 10]".into());
        }
        if !(0.0..=1.0).contains(&c.change_threshold) {
            return Err("capture.change_threshold must be between 0.0 and 1.0".into());
        }
        if c.image_quality > 100 {
            return Err("capture.image_quality must be 0-100".into());
        }
        if !(self.storage.max_size_gb >= 0.0) {
            return Err("storage.max_size_gb must be >= 0".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_defaults() {
        assert_eq!(Config::parse("").ok(), Some(Config::default()));
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let config = Config::parse("[capture]\nimage_quality = 60\n").ok();
        let config = config.as_ref().map(|c| (c.capture.image_quality, c.capture.fps_candidate));
        assert_eq!(config, Some((60, 1.0)));
    }

    #[test]
    fn typos_are_rejected() {
        assert!(Config::parse("[privacy]\nexcluded_proceses = [\"x.exe\"]\n").is_err());
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        assert!(Config::parse("[capture]\nfps_candidate = 0\n").is_err());
        assert!(Config::parse("[capture]\nchange_threshold = 1.5\n").is_err());
        assert!(Config::parse("[capture]\nimage_quality = 101\n").is_err());
        assert!(Config::parse("[storage]\nmax_size_gb = nan\n").is_err());
    }

    #[test]
    fn default_file_round_trips() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.toml");
        assert!(Config::write_default_if_missing(&path)?);
        assert!(!Config::write_default_if_missing(&path)?);
        assert_eq!(Config::load_or_default(&path)?, Config::default());
        Ok(())
    }

    #[test]
    fn missing_file_is_defaults_but_bad_file_is_error() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.toml");
        assert_eq!(Config::load_or_default(&path)?, Config::default());
        std::fs::write(&path, "[capture\n")?;
        assert!(Config::load_or_default(&path).is_err());
        Ok(())
    }
}
