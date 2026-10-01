//! Privacy rules: decide, *before anything is persisted*, whether a window may be recorded.
//!
//! Matching is deliberately simple so a user can predict it: process names compare
//! case-insensitively and exactly (`1Password.exe`), title patterns are case-insensitive globs
//! where `*` matches any run of characters and `?` one character. No regexes: a rule a user cannot
//! read is a rule they cannot trust.

use crate::{ApplicationContext, WindowContext};
use serde::{Deserialize, Serialize};

// `deny_unknown_fields` is load-bearing: a misspelled key here would otherwise be silently ignored
// and the user's exclusion would never apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrivacyPolicy {
    pub excluded_processes: Vec<String>,
    pub excluded_title_patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivacyDecision {
    Allow,
    /// Do not persist pixels. `rule` names the matching rule (safe to log: it is user config, not
    /// captured content).
    Exclude {
        rule: String,
    },
}

impl PrivacyDecision {
    pub fn is_excluded(&self) -> bool {
        matches!(self, Self::Exclude { .. })
    }
}

impl PrivacyPolicy {
    /// Defaults offered on first run. Configurable, never mandatory: a user can delete them.
    pub fn suggested_defaults() -> Self {
        Self {
            excluded_processes: [
                "1Password.exe",
                "Bitwarden.exe",
                "KeePass.exe",
                "KeePassXC.exe",
                "Dashlane.exe",
                "CredentialUIBroker.exe",
                "LogonUI.exe",
                "consent.exe",
            ]
            .map(String::from)
            .to_vec(),
            excluded_title_patterns: ["*InPrivate*", "*Incognito*", "*Private Browsing*"]
                .map(String::from)
                .to_vec(),
        }
    }

    pub fn evaluate(
        &self,
        application: Option<&ApplicationContext>,
        window: Option<&WindowContext>,
    ) -> PrivacyDecision {
        if let Some(app) = application
            && let Some(rule) = self
                .excluded_processes
                .iter()
                .find(|rule| rule.trim().eq_ignore_ascii_case(app.process_name.trim()))
        {
            return PrivacyDecision::Exclude {
                rule: format!("process:{rule}"),
            };
        }
        if let Some(window) = window
            && let Some(rule) = self
                .excluded_title_patterns
                .iter()
                .find(|pattern| glob_matches(pattern, &window.title))
        {
            return PrivacyDecision::Exclude {
                rule: format!("title:{rule}"),
            };
        }
        PrivacyDecision::Allow
    }
}

/// Case-insensitive glob: `*` = any run (including empty), `?` = exactly one character.
/// An empty pattern matches nothing, so a stray blank line in config cannot exclude everything.
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    if pattern.trim().is_empty() {
        return false;
    }
    let pattern: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();

    // Iterative wildcard match with single-star backtracking: O(p*t) worst case, no recursion.
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_text = 0usize;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            star_text = t;
            p += 1;
        } else if let Some(star_at) = star {
            p = star_at + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> ApplicationContext {
        ApplicationContext {
            process_name: name.into(),
            exe_path: None,
        }
    }

    fn win(title: &str) -> WindowContext {
        WindowContext {
            title: title.into(),
            class_name: None,
        }
    }

    #[test]
    fn process_match_is_case_insensitive_and_exact() {
        let policy = PrivacyPolicy {
            excluded_processes: vec!["1Password.exe".into()],
            ..Default::default()
        };
        assert!(
            policy
                .evaluate(Some(&app("1PASSWORD.EXE")), None)
                .is_excluded()
        );
        assert!(
            !policy
                .evaluate(Some(&app("1Password.exe.bak")), None)
                .is_excluded()
        );
        assert!(
            !policy
                .evaluate(Some(&app("Not1Password.exe")), None)
                .is_excluded()
        );
    }

    #[test]
    fn title_globs() {
        assert!(glob_matches(
            "*InPrivate*",
            "New tab - [InPrivate] - Microsoft Edge"
        ));
        assert!(glob_matches("*inprivate*", "x INPRIVATE y"));
        assert!(glob_matches("Bank*", "Bank of Example"));
        assert!(!glob_matches("Bank*", "My Bank"));
        assert!(glob_matches("a?c", "abc"));
        assert!(!glob_matches("a?c", "ac"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("**x**", "x"));
        assert!(glob_matches("*a*b*c*", "zzazzbzzczz"));
        assert!(!glob_matches("*a*b*c*", "zzczzbzza"));
    }

    #[test]
    fn blank_patterns_never_match() {
        assert!(!glob_matches("", "anything"));
        assert!(!glob_matches("   ", "anything"));
        let policy = PrivacyPolicy {
            excluded_processes: vec![String::new()],
            excluded_title_patterns: vec![String::new()],
        };
        assert_eq!(
            policy.evaluate(Some(&app("x.exe")), Some(&win("t"))),
            PrivacyDecision::Allow
        );
    }

    #[test]
    fn non_ascii_titles() {
        assert!(glob_matches("*ÜBERSICHT*", "Konto-Übersicht – Bank"));
        assert!(glob_matches("*日本*", "日本語のページ"));
    }

    #[test]
    fn reports_which_rule_matched() {
        let policy = PrivacyPolicy::suggested_defaults();
        assert_eq!(
            policy.evaluate(None, Some(&win("Docs - Incognito"))),
            PrivacyDecision::Exclude {
                rule: "title:*Incognito*".into()
            }
        );
        assert_eq!(
            policy.evaluate(Some(&app("notepad.exe")), Some(&win("notes.txt"))),
            PrivacyDecision::Allow
        );
    }

    #[test]
    fn pathological_pattern_is_bounded() {
        let pattern = "*a".repeat(50);
        let text = "a".repeat(2_000) + "b";
        assert!(!glob_matches(&pattern, &text));
    }
}
