//! Human-readable output. JSON output serializes the core types directly and lives in `main.rs`.

use rsrewind_core::{Gap, SearchHit, SourceId, TimelineEntry};
use std::collections::HashMap;
use std::fmt::Write as _;

/// Matches the README example:
///
/// ```text
/// 2026-09-30 20:32:11
/// Application: Teams
/// Window: Jonathan Redmon
/// "...TAP should work once Web Sign-In is enabled..."
/// ```
/// Display names of imported sources (`None` when the probe sent no label).
pub type SourceLabels = HashMap<SourceId, Option<String>>;

/// How a source is named in human output: its label and short id, so two probes with the same
/// host name stay distinguishable. Labels arrive from other machines and are made printable.
pub fn machine(source: SourceId, labels: &SourceLabels) -> String {
    match labels.get(&source).and_then(Option::as_deref) {
        Some(label) => format!("{} ({})", plain(label), source.short()),
        None => source.short(),
    }
}

/// History from another machine gets a `Machine:` line; this machine's hits print as before.
pub fn search_hits(hits: &[SearchHit], labels: &SourceLabels) -> String {
    if hits.is_empty() {
        return "No matches.\n".into();
    }
    let mut out = String::new();
    for (i, hit) in hits.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = writeln!(out, "{}", hit.timestamp);
        if let Some(source) = hit.source {
            let _ = writeln!(out, "Machine: {}", machine(source, labels));
        }
        let _ = writeln!(
            out,
            "Application: {}",
            plain(display_app(hit.application.as_deref()))
        );
        let _ = writeln!(
            out,
            "Window: {}",
            plain(hit.window_title.as_deref().unwrap_or("(unknown)"))
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

/// A machine column appears only when some entry came from another machine.
pub fn timeline(entries: &[TimelineEntry], labels: &SourceLabels) -> String {
    if entries.is_empty() {
        return "Nothing recorded yet.\n".into();
    }
    let any_remote = entries.iter().any(|e| e.source.is_some());
    let mut out = String::new();
    for entry in entries {
        if any_remote {
            let name = entry
                .source
                .map_or_else(|| "this machine".to_string(), |s| machine(s, labels));
            let _ = write!(out, "{:<24}  ", truncate(&name, 24));
        }
        let span_secs = (entry.ended_at.as_millis() - entry.started_at.as_millis()).max(0) / 1000;
        let _ = writeln!(
            out,
            "{}  {:>5}s  {:<24}  {}  [id {}, ocr {}]",
            entry.started_at,
            span_secs,
            truncate(&plain(display_app(entry.application.as_deref())), 24),
            truncate(&plain(entry.window_title.as_deref().unwrap_or("")), 60),
            entry.visual_state_id,
            entry.ocr_status,
        );
    }
    out
}

/// `2h 14m`, `5m`, `45s`.
pub fn duration(millis: i64) -> String {
    let secs = millis.max(0) / 1000;
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m"),
        (0, _, _) => format!("{h}h {m:02}m"),
        _ => format!("{d}d {h}h"),
    }
}

fn gap_line(gap: &Gap, labels: &SourceLabels) -> String {
    let who = gap
        .source
        .map(|s| format!("{}: ", machine(s, labels)))
        .unwrap_or_default();
    format!(
        "  ── {who}{} not recorded: {} ({} to {}) ──",
        duration(gap.millis()),
        gap.reason.describe(),
        gap.from,
        gap.to
    )
}

/// `rsrewind gaps`, oldest first.
pub fn gaps(gaps: &[Gap], labels: &SourceLabels) -> String {
    if gaps.is_empty() {
        return "No gaps: something was recorded throughout.\n".into();
    }
    let mut out = String::new();
    for gap in gaps {
        let _ = writeln!(out, "{}", gap_line(gap, labels).trim());
    }
    out
}

/// [`timeline`] (newest first) with a line for each gap, placed where the jump in time happens.
pub fn timeline_with_gaps(
    entries: &[TimelineEntry],
    gaps: &[Gap],
    labels: &SourceLabels,
) -> String {
    let rows = timeline(entries, labels);
    if entries.is_empty() {
        return rows;
    }
    let mut pending: Vec<&Gap> = gaps.iter().collect();
    // Newest gap first, to walk alongside the newest-first rows.
    pending.sort_by_key(|g| std::cmp::Reverse(g.to));
    let mut out = String::new();
    let mut next = pending.into_iter().peekable();
    for (entry, row) in entries.iter().zip(rows.lines()) {
        while let Some(gap) = next.next_if(|g| g.to > entry.started_at) {
            let _ = writeln!(out, "{}", gap_line(gap, labels));
        }
        let _ = writeln!(out, "{row}");
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
    plain(&text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Replaces control characters (ESC and friends) with `?` before text goes to a terminal. Window
/// titles, process names, labels and OCR text come from whatever was on a screen, possibly a remote
/// one, and must not be able to drive the terminal that prints them.
pub fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
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
    use rsrewind_core::{EventId, GapReason, Timestamp, VisualStateId};

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
            source: None,
        }
    }

    fn entry(at_secs: i64) -> TimelineEntry {
        TimelineEntry {
            event_id: EventId(at_secs),
            visual_state_id: VisualStateId(at_secs),
            started_at: Timestamp(at_secs * 1000),
            ended_at: Timestamp(at_secs * 1000 + 5000),
            application: Some("code".into()),
            window_title: Some(format!("moment {at_secs}")),
            monitor: None,
            media_path: String::new(),
            ocr_status: "pending".into(),
            source: None,
        }
    }

    fn gap(from_secs: i64, to_secs: i64, reason: GapReason) -> Gap {
        Gap {
            from: Timestamp(from_secs * 1000),
            to: Timestamp(to_secs * 1000),
            reason,
            source: None,
        }
    }

    #[test]
    fn gap_lines_sit_where_time_jumps() {
        let entries = [entry(10_000), entry(1_000)];
        let gaps = [
            gap(1_005, 9_000, GapReason::RecorderOff),
            gap(10_005, 12_000, GapReason::Idle),
        ];
        let text = timeline_with_gaps(&entries, &gaps, &SourceLabels::new());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        // Newest first: the current idle stretch, the newer moment, the time off, the older one.
        assert!(lines[0].contains("not recorded: idle or locked"), "{text}");
        assert!(lines[1].contains("moment 10000"), "{text}");
        assert!(
            lines[2].contains("2h 13m not recorded: recorder off"),
            "{text}"
        );
        assert!(lines[3].contains("moment 1000"), "{text}");
        assert_eq!(
            gaps_text(&[]),
            "No gaps: something was recorded throughout.\n"
        );
    }

    fn gaps_text(gaps: &[Gap]) -> String {
        super::gaps(gaps, &SourceLabels::new())
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(45_000), "45s");
        assert_eq!(duration(5 * 60_000 + 59_000), "5m");
        assert_eq!(duration(2 * 3_600_000 + 14 * 60_000), "2h 14m");
        assert_eq!(duration(3 * 86_400_000 + 5 * 3_600_000), "3d 5h");
        assert_eq!(duration(-5), "0s");
    }

    #[test]
    fn remote_hits_name_their_machine_and_local_output_is_unchanged() {
        let source = SourceId::from_bytes([0xab; 16]);
        let labels: SourceLabels = [(source, Some("kubert\u{1b}[2J".to_string()))].into();
        let local = search_hits(&[hit()], &labels);
        assert!(!local.contains("Machine:"), "{local}");
        let mut remote = hit();
        remote.source = Some(source);
        let text = search_hits(&[remote], &labels);
        assert!(!text.contains('\u{1b}'), "{text:?}");
        let line = format!("Machine: kubert?[2J ({})\n", source.short());
        assert!(text.contains(&line), "{text}");
        let unlabeled = search_hits(&[remote_hit(source)], &SourceLabels::new());
        assert!(
            unlabeled.contains(&format!("Machine: {}\n", source.short())),
            "{unlabeled}"
        );
    }

    fn remote_hit(source: SourceId) -> SearchHit {
        SearchHit {
            source: Some(source),
            ..hit()
        }
    }

    #[test]
    fn renders_search_like_the_readme() {
        let text = search_hits(&[hit()], &SourceLabels::new());
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
        let text = search_hits(&[cut], &SourceLabels::new());
        assert!(
            text.contains("\"…TAP should work once [Web] Sign-In is enabled…\"\n"),
            "{text}"
        );
    }

    #[test]
    fn empty_results_say_so() {
        assert_eq!(search_hits(&[], &SourceLabels::new()), "No matches.\n");
        assert_eq!(
            timeline(&[], &SourceLabels::new()),
            "Nothing recorded yet.\n"
        );
    }

    #[test]
    fn terminal_control_characters_never_reach_the_output() {
        let mut hostile = hit();
        hostile.window_title = Some("\u{1b}]0;pwned\u{7}title".into());
        hostile.application = Some("a\u{1b}[2Jb.exe".into());
        hostile.snippet = "x\u{1b}[31mred\u{9b}".into();
        let out = search_hits(&[hostile], &SourceLabels::new());
        assert!(!out.chars().any(|c| c.is_control() && c != '\n'), "{out:?}");
        assert!(out.contains("title"));
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
