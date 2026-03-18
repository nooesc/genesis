//! Unified diff parser and renderer.
//!
//! Parses unified diff format (from `git diff`, `diff -u`, etc.) and converts
//! it to styled ratatui Lines with colored additions, deletions, and context.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

// ── Diff palette ────────────────────────────────────────────────────────────

/// Addition foreground (UI_SUCCESS green).
const ADD_FG: Color = Color::Rgb(135, 175, 95);
/// Addition background (muted green tint).
const ADD_BG: Color = Color::Rgb(33, 58, 43);
/// Deletion foreground (UI_ERROR red).
const DEL_FG: Color = Color::Rgb(215, 95, 95);
/// Deletion background (muted red tint).
const DEL_BG: Color = Color::Rgb(74, 34, 29);
/// Context foreground (UI_DIM gray).
const CONTEXT_FG: Color = Color::Rgb(108, 108, 108);
/// Hunk header foreground (EVE_LAVENDER accent).
const HUNK_FG: Color = Color::Rgb(180, 167, 214);
/// Line-number gutter foreground (very dim).
const GUTTER_FG: Color = Color::Rgb(88, 88, 88);
/// File header foreground (UI_TEXT).
const FILE_HEADER_FG: Color = Color::Rgb(208, 208, 208);

/// Returns `true` if the text looks like a unified diff.
pub fn is_unified_diff(text: &str) -> bool {
    // A unified diff starts with "diff " or "---" followed by "+++" on the
    // next line, or contains @@ hunk headers.
    let text = text.trim_start();
    text.starts_with("diff ")
        || text.starts_with("--- ")
        || text.contains("\n@@ ")
        || text.starts_with("@@ ")
}

/// Parse a unified diff string into styled Lines.
pub fn diff_to_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    for raw_line in text.lines() {
        if raw_line.starts_with("diff ") {
            // File diff header: "diff --git a/file b/file"
            lines.push(diff_header_line(raw_line));
        } else if raw_line.starts_with("--- ") || raw_line.starts_with("+++ ") {
            // File path headers
            lines.push(file_header_line(raw_line));
        } else if raw_line.starts_with("@@ ") {
            // Hunk header: "@@ -n,m +n,m @@ optional context"
            if let Some((old_start, new_start)) = parse_hunk_header(raw_line) {
                old_line = old_start;
                new_line = new_start;
            }
            lines.push(hunk_header_line(raw_line));
        } else if let Some(content) = raw_line.strip_prefix('+') {
            // Addition
            lines.push(addition_line(new_line, content));
            new_line += 1;
        } else if let Some(content) = raw_line.strip_prefix('-') {
            // Deletion
            lines.push(deletion_line(old_line, content));
            old_line += 1;
        } else if let Some(content) = raw_line.strip_prefix(' ') {
            // Context
            lines.push(context_line(old_line, new_line, content));
            old_line += 1;
            new_line += 1;
        } else {
            // Other lines (e.g. "\ No newline at end of file", index, mode)
            lines.push(meta_line(raw_line));
        }
    }

    lines
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    // Parse "@@ -old_start,old_count +new_start,new_count @@"
    let after_at = line.strip_prefix("@@ -")?;
    let (old_part, rest) = after_at.split_once(' ')?;
    let old_start: u32 = old_part.split(',').next()?.parse().ok()?;
    let new_part = rest.strip_prefix('+')?;
    let new_start: u32 = new_part
        .split([',', ' '])
        .next()?
        .parse()
        .ok()?;
    Some((old_start, new_start))
}

fn diff_header_line(raw: &str) -> Line<'static> {
    Line::from(Span::styled(
        raw.to_owned(),
        Style::default()
            .fg(FILE_HEADER_FG)
            .add_modifier(Modifier::BOLD),
    ))
}

fn file_header_line(raw: &str) -> Line<'static> {
    Line::from(Span::styled(
        raw.to_owned(),
        Style::default()
            .fg(FILE_HEADER_FG)
            .add_modifier(Modifier::BOLD),
    ))
}

fn hunk_header_line(raw: &str) -> Line<'static> {
    Line::from(Span::styled(
        raw.to_owned(),
        Style::default().fg(HUNK_FG),
    ))
}

fn addition_line(line_num: u32, content: &str) -> Line<'static> {
    let gutter = format!("    {:>4} ", line_num);
    Line::from(vec![
        Span::styled(gutter, Style::default().fg(GUTTER_FG)),
        Span::styled(
            "+",
            Style::default().fg(ADD_FG).add_modifier(Modifier::BOLD),
        ),
        Span::styled(content.to_owned(), Style::default().fg(ADD_FG).bg(ADD_BG)),
    ])
}

fn deletion_line(line_num: u32, content: &str) -> Line<'static> {
    let gutter = format!("{:<4}     ", line_num);
    Line::from(vec![
        Span::styled(gutter, Style::default().fg(GUTTER_FG)),
        Span::styled(
            "-",
            Style::default().fg(DEL_FG).add_modifier(Modifier::BOLD),
        ),
        Span::styled(content.to_owned(), Style::default().fg(DEL_FG).bg(DEL_BG)),
    ])
}

fn context_line(old_num: u32, new_num: u32, content: &str) -> Line<'static> {
    let gutter = format!("{:<4}{:>4} ", old_num, new_num);
    Line::from(vec![
        Span::styled(gutter, Style::default().fg(GUTTER_FG)),
        Span::styled(" ", Style::default().fg(CONTEXT_FG)),
        Span::styled(content.to_owned(), Style::default().fg(CONTEXT_FG)),
    ])
}

fn meta_line(raw: &str) -> Line<'static> {
    Line::from(Span::styled(
        raw.to_owned(),
        Style::default().fg(CONTEXT_FG),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DIFF: &str = "\
diff --git a/src/main.rs b/src/main.rs
index abc1234..def5678 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,6 @@
 fn main() {
-    println!(\"hello\");
+    println!(\"hello world\");
+    println!(\"goodbye\");
     let x = 1;
 }";

    #[test]
    fn detects_unified_diff() {
        assert!(is_unified_diff(SAMPLE_DIFF));
        assert!(is_unified_diff(
            "@@ -1,3 +1,4 @@\n context\n-removed\n+added"
        ));
        assert!(is_unified_diff(
            "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new"
        ));
    }

    #[test]
    fn rejects_non_diff() {
        assert!(!is_unified_diff("hello world"));
        assert!(!is_unified_diff("wrote 42 bytes to foo.txt"));
        assert!(!is_unified_diff("fn main() { }"));
    }

    #[test]
    fn parses_hunk_header_with_counts() {
        assert_eq!(parse_hunk_header("@@ -1,5 +1,6 @@"), Some((1, 1)));
        assert_eq!(
            parse_hunk_header("@@ -10,3 +20,4 @@ fn main()"),
            Some((10, 20))
        );
    }

    #[test]
    fn parses_hunk_header_without_counts() {
        assert_eq!(parse_hunk_header("@@ -1 +1 @@"), Some((1, 1)));
    }

    #[test]
    fn diff_to_lines_produces_correct_count() {
        let lines = diff_to_lines(SAMPLE_DIFF);
        // diff header, index, ---, +++, @@, 6 content lines = 11 lines
        assert_eq!(lines.len(), 11);
    }

    #[test]
    fn additions_use_green() {
        let lines = diff_to_lines(SAMPLE_DIFF);
        // Find an addition line (should contain "+")
        let add_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "+"));
        assert!(add_line.is_some(), "should have an addition line");
        let spans = &add_line.unwrap().spans;
        let plus_span = spans.iter().find(|s| s.content.as_ref() == "+").unwrap();
        assert_eq!(plus_span.style.fg, Some(ADD_FG));
    }

    #[test]
    fn deletions_use_red() {
        let lines = diff_to_lines(SAMPLE_DIFF);
        let del_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "-"));
        assert!(del_line.is_some(), "should have a deletion line");
        let spans = &del_line.unwrap().spans;
        let minus_span = spans.iter().find(|s| s.content.as_ref() == "-").unwrap();
        assert_eq!(minus_span.style.fg, Some(DEL_FG));
    }

    #[test]
    fn hunk_headers_use_accent() {
        let lines = diff_to_lines(SAMPLE_DIFF);
        let hunk_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.starts_with("@@")));
        assert!(hunk_line.is_some());
        let span = &hunk_line.unwrap().spans[0];
        assert_eq!(span.style.fg, Some(HUNK_FG));
    }

    #[test]
    fn is_unified_diff_handles_leading_whitespace() {
        assert!(is_unified_diff("  diff --git a/x b/x"));
        assert!(is_unified_diff("\n\n@@ -1 +1 @@"));
    }

    #[test]
    fn empty_input() {
        assert_eq!(diff_to_lines("").len(), 0);
        assert!(!is_unified_diff(""));
    }
}
