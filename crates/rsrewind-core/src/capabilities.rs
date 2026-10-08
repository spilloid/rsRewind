//! What a recording platform can actually see, stated explicitly instead of pretended equal.
//!
//! Windows can enumerate every visible top-level window with its rectangle, title and process,
//! which is what the privacy rule "skip a monitor if *any* visible window on it matches" needs. A
//! Wayland compositor without a window-list protocol cannot. The recorder never papers over that:
//! the platform reports a [`Capabilities`] value, the recorder refuses to start when the
//! configured rules cannot be enforced (unless `privacy.unenforced_ok` says otherwise), and the
//! value is recorded per session so `status`, `doctor` and exported segments tell the truth.

use crate::PrivacyPolicy;
use serde::{Deserialize, Serialize};

/// Segment capability tags (`docs/design/distributed.md` section 5). Strings, so a platform can
/// declare fewer without a format change.
pub const TAG_OCR: &str = "ocr";
pub const TAG_WINDOW_TITLES: &str = "window_titles";
pub const TAG_PROCESS_NAMES: &str = "process_names";
pub const TAG_MULTI_MONITOR: &str = "multi_monitor";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Every visible top-level window can be enumerated with its screen rectangle. Without it the
    /// any-visible-window privacy rule cannot be enforced at all.
    pub list_windows: bool,
    /// Window titles can be read (title rules, history attribution).
    pub window_titles: bool,
    /// Process names can be read (process rules, history attribution).
    pub process_names: bool,
    /// More than one monitor can be captured.
    pub multi_monitor: bool,
    /// Text recognition is available to this recorder.
    pub ocr: bool,
}

impl Capabilities {
    /// What the Windows recorder (WGC, EnumWindows, Windows.Media.Ocr) provides.
    pub const WINDOWS: Self = Self {
        list_windows: true,
        window_titles: true,
        process_names: true,
        multi_monitor: true,
        ocr: true,
    };

    /// Segment tags this platform may claim. `window_titles` is claimed only together with the
    /// window list: titles of a focused window alone say nothing about what else was on screen,
    /// and a consumer must not infer privacy coverage from the tag.
    pub fn segment_tags(&self) -> Vec<&'static str> {
        let mut tags = Vec::new();
        if self.ocr {
            tags.push(TAG_OCR);
        }
        if self.window_titles && self.list_windows {
            tags.push(TAG_WINDOW_TITLES);
        }
        if self.process_names {
            tags.push(TAG_PROCESS_NAMES);
        }
        if self.multi_monitor {
            tags.push(TAG_MULTI_MONITOR);
        }
        tags
    }

    /// What this platform is missing to enforce `policy`, as short, loggable names. Empty means
    /// every configured rule is enforced. The window list is always required (the rule is about
    /// *every* visible window); process names and titles only when a rule of that kind exists.
    pub fn privacy_gaps(&self, policy: &PrivacyPolicy) -> Vec<&'static str> {
        let mut gaps = Vec::new();
        if !self.list_windows {
            gaps.push("window list");
        }
        if !self.process_names
            && policy
                .excluded_processes
                .iter()
                .any(|p| !p.trim().is_empty())
        {
            gaps.push("process names");
        }
        if !self.window_titles
            && policy
                .excluded_title_patterns
                .iter()
                .any(|p| !p.trim().is_empty())
        {
            gaps.push("window titles");
        }
        gaps
    }
}

/// What one recording session could see, stored with the session (`settings`, no schema change)
/// so that later readers (`status`, `doctor`, `export`) do not have to guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCapabilities {
    pub capabilities: Capabilities,
    /// False when the session ran with `privacy.unenforced_ok` over a non-empty
    /// [`Capabilities::privacy_gaps`].
    pub privacy_enforced: bool,
    /// The gaps, for display. Names, never captured content.
    pub privacy_gaps: Vec<String>,
}

impl SessionCapabilities {
    pub fn new(capabilities: Capabilities, policy: &PrivacyPolicy) -> Self {
        let gaps = capabilities.privacy_gaps(policy);
        Self {
            capabilities,
            privacy_enforced: gaps.is_empty(),
            privacy_gaps: gaps.into_iter().map(String::from).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_claims_everything_it_always_claimed() {
        assert_eq!(
            Capabilities::WINDOWS.segment_tags(),
            vec!["ocr", "window_titles", "process_names", "multi_monitor"]
        );
        assert!(
            Capabilities::WINDOWS
                .privacy_gaps(&PrivacyPolicy::suggested_defaults())
                .is_empty()
        );
    }

    #[test]
    fn no_window_list_means_no_title_claim_and_unenforced_privacy() {
        let caps = Capabilities {
            list_windows: false,
            ..Capabilities::WINDOWS
        };
        assert!(!caps.segment_tags().contains(&TAG_WINDOW_TITLES));
        assert_eq!(
            caps.privacy_gaps(&PrivacyPolicy::default()),
            vec!["window list"]
        );
    }

    #[test]
    fn missing_fields_matter_only_when_a_rule_needs_them() {
        let caps = Capabilities {
            window_titles: false,
            process_names: false,
            ..Capabilities::WINDOWS
        };
        assert!(caps.privacy_gaps(&PrivacyPolicy::default()).is_empty());
        let gaps = caps.privacy_gaps(&PrivacyPolicy::suggested_defaults());
        assert_eq!(gaps, vec!["process names", "window titles"]);
        let session = SessionCapabilities::new(caps, &PrivacyPolicy::suggested_defaults());
        assert!(!session.privacy_enforced);
    }
}
