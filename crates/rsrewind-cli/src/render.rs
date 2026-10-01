//! Human-readable output. JSON output serializes the core types directly and lives in `main.rs`.

use rsrewind_core::{SearchHit, TimelineEntry};
use std::fmt::Write as _;

/// Matches the README example:
///
/// ```text
/// 2026-09-30 20:32:11
/// Application: Teams
/// Window: Jonathan Redmon
/// "...TAP should work once Web Sign-In is enabled..."
/// ```
pub fn search_hits(hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return "No matches.\n".into();
    }
    let mut out = String::new();
    for (i, hit) in hits.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = writeln!(out, "{}", hit.timestamp);
        let _ = writeln!(
            out,
            "Application: {}",
            display_app(hit.application.as_deref())
        );
        let _ = writeln!(
            out,
            "Window: {}",
            hit.window_title.as_deref().unwrap_or("(unknown)")
        );
        // The query layer's snippet already carries its own `…` where it was cut.
        let _ = writeln!(out, "\"{}\"", one_line(&hit.snippet));
        let _ = writeln!(
            out,
            "Image: {}  (id {})",
            hit.media_path, hit.visual_state_id
        );
    }
    out
}

pub fn timeline(entries: &[TimelineEntry]) -> String {
    if entries.is_empty() {
        return "Nothing recorded yet.\n".into();
    }
    let mut out = String::new();
    for entry in entries {
        let span_secs = (entry.ended_at.as_millis() - entry.started_at.as_millis()).max(0) / 1000;
        let _ = writeln!(
            out,
            "{}  {:>5}s  {:<24}  {}  [id {}, ocr {}]",
            entry.started_at,
            span_secs,
            truncate(display_app(entry.application.as_deref()), 24),
            truncate(entry.window_title.as_deref().unwrap_or(""), 60),
            entry.visual_state_id,
            entry.ocr_status,
        );
    }
    out
}

/// `Teams.exe` reads better as `Teams`.
pub fn display_app(process_name: Option<&str>) -> &str {
    match process_name {
        Some(name) => name
            .strip_suffix(".exe")
            .or_else(|| name.strip_suffix(".EXE"))
            .unwrap_or(name),
        None => "(unknown)",
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsrewind_core::{Timestamp, VisualStateId};

    fn hit() -> SearchHit {
        SearchHit {
            visual_state_id: VisualStateId(7),
            timestamp: Timestamp(0),
            timestamp_utc: "1970-01-01T00:00:00Z".into(),
            application: Some("Teams.exe".into()),
            window_title: Some("Jonathan Redmon".into()),
            monitor: None,
            snippet: "TAP should work once\n[Web] Sign-In is enabled".into(),
            media_path: r"C:\x\media\a.webp".into(),
            rank: -1.0,
        }
    }

    #[test]
    fn renders_search_like_the_readme() {
        let text = search_hits(&[hit()]);
        assert!(text.contains("Application: Teams\n"), "{text}");
        assert!(text.contains("Window: Jonathan Redmon\n"), "{text}");
        // Newlines in the snippet collapse to spaces; the renderer adds no ellipses of its own.
        assert!(
            text.contains("\"TAP should work once [Web] Sign-In is enabled\"\n"),
            "{text}"
        );
        assert!(text.contains("(id 7)"), "{text}");
    }

    #[test]
    fn snippet_ellipses_come_from_the_query_layer_and_pass_through() {
        let mut cut = hit();
        cut.snippet = "…TAP should work once\n[Web] Sign-In is enabled…".into();
        let text = search_hits(&[cut]);
        assert!(
            text.contains("\"…TAP should work once [Web] Sign-In is enabled…\"\n"),
            "{text}"
        );
    }

    #[test]
    fn empty_results_say_so() {
        assert_eq!(search_hits(&[]), "No matches.\n");
        assert_eq!(timeline(&[]), "Nothing recorded yet.\n");
    }

    #[test]
    fn truncation_is_char_safe() {
        assert_eq!(truncate("日本語のページ", 4), "日本語…");
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn app_names() {
        assert_eq!(display_app(Some("Teams.exe")), "Teams");
        assert_eq!(display_app(Some("NOTEPAD.EXE")), "NOTEPAD");
        assert_eq!(display_app(Some("weird")), "weird");
        assert_eq!(display_app(None), "(unknown)");
    }
}
