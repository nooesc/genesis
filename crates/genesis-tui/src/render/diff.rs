//! Unified diff parser and renderer.
//!
//! Parses unified diff format (from `git diff`, `diff -u`, etc.) and converts
//! it to styled ratatui Lines with colored additions, deletions, and context.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;

use super::markdown::{get_syntax_set, get_theme};

// ── Diff colors ─────────────────────────────────────────────────────────────

/// Colors for diff rendering — sourced from the active theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffColors {
    pub add_fg: Color,
    pub add_bg: Color,
    pub del_fg: Color,
    pub del_bg: Color,
    pub context_fg: Color,
    pub hunk_fg: Color,
    pub gutter_fg: Color,
    pub file_header_fg: Color,
}

impl Default for DiffColors {
    /// Default colors (Eve theme palette).
    fn default() -> Self {
        Self {
            add_fg: Color::Rgb(135, 175, 95),
            add_bg: Color::Rgb(33, 58, 43),
            del_fg: Color::Rgb(215, 95, 95),
            del_bg: Color::Rgb(74, 34, 29),
            context_fg: Color::Rgb(108, 108, 108),
            hunk_fg: Color::Rgb(180, 167, 214),
            gutter_fg: Color::Rgb(88, 88, 88),
            file_header_fg: Color::Rgb(208, 208, 208),
        }
    }
}

impl DiffColors {
    /// Create `DiffColors` from a [`Theme`](crate::theme::Theme).
    pub fn from_theme(theme: &dyn crate::theme::Theme) -> Self {
        Self {
            add_fg: theme.diff_add_fg(),
            add_bg: theme.diff_add_bg(),
            del_fg: theme.diff_del_fg(),
            del_bg: theme.diff_del_bg(),
            context_fg: theme.text_dim(),
            hunk_fg: theme.primary(),
            gutter_fg: theme.border(),
            file_header_fg: theme.text(),
        }
    }
}

/// Returns `true` if the text looks like a unified diff.
pub fn is_unified_diff(text: &str) -> bool {
    let text = text.trim_start();
    // "diff --git" or "diff -u" header is unambiguous.
    if text.starts_with("diff ") {
        return true;
    }
    // "--- " is only a diff when followed by "+++ " (avoids false positives
    // on YAML separators, markdown, or error messages starting with "---").
    if text.starts_with("--- ") && text.contains("\n+++ ") {
        return true;
    }
    // @@ hunk headers are distinctive enough on their own.
    text.contains("\n@@ ") || text.starts_with("@@ ")
}

/// Parse a unified diff string into styled Lines using default (Eve) colors.
pub fn diff_to_lines(text: &str) -> Vec<Line<'static>> {
    diff_to_lines_themed(text, &DiffColors::default())
}

/// Maximum total line count before syntax highlighting is skipped (performance guard).
const MAX_HIGHLIGHT_LINES: usize = 10_000;

/// Parse a unified diff string into styled Lines with theme-derived colors.
///
/// Content lines (additions, deletions, context) receive per-token syntax
/// highlighting via syntect when the file extension is recognized. The diff
/// background tint (add_bg/del_bg) is layered on top of the syntax colors.
pub fn diff_to_lines_themed(text: &str, colors: &DiffColors) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    // Detect file extension for syntax highlighting.
    let total_lines = text.lines().count();
    let do_highlight = total_lines <= MAX_HIGHLIGHT_LINES;
    let extension = if do_highlight {
        detect_extension(text)
    } else {
        None
    };

    let ss = get_syntax_set();
    let syntax = extension
        .and_then(|ext| ss.find_syntax_by_extension(ext))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let theme = get_theme();
    let mut highlighter = HighlightLines::new(syntax, theme);

    for raw_line in text.lines() {
        if raw_line.starts_with("diff ") {
            lines.push(diff_header_line(raw_line, colors));
            // Always reset highlighter on file boundaries to avoid
            // carrying parser state (e.g. mid-comment) into the next file.
            if do_highlight {
                let ext = extract_extension_from_diff_header(raw_line);
                let new_syntax = ext
                    .and_then(|e| ss.find_syntax_by_extension(e))
                    .unwrap_or_else(|| ss.find_syntax_plain_text());
                highlighter = HighlightLines::new(new_syntax, theme);
            }
        } else if raw_line.starts_with("--- ") || raw_line.starts_with("+++ ") {
            lines.push(file_header_line(raw_line, colors));
        } else if raw_line.starts_with("@@ ") {
            if let Some((old_start, new_start)) = parse_hunk_header(raw_line) {
                old_line = old_start;
                new_line = new_start;
            }
            lines.push(hunk_header_line(raw_line, colors));
        } else if let Some(content) = raw_line.strip_prefix('+') {
            lines.push(highlighted_content_line(
                if do_highlight {
                    Some(&mut highlighter)
                } else {
                    None
                },
                ss,
                content,
                ContentKind::Addition(new_line),
                colors,
            ));
            new_line += 1;
        } else if let Some(content) = raw_line.strip_prefix('-') {
            lines.push(highlighted_content_line(
                if do_highlight {
                    Some(&mut highlighter)
                } else {
                    None
                },
                ss,
                content,
                ContentKind::Deletion(old_line),
                colors,
            ));
            old_line += 1;
        } else if let Some(content) = raw_line.strip_prefix(' ') {
            lines.push(highlighted_content_line(
                if do_highlight {
                    Some(&mut highlighter)
                } else {
                    None
                },
                ss,
                content,
                ContentKind::Context(old_line, new_line),
                colors,
            ));
            old_line += 1;
            new_line += 1;
        } else {
            lines.push(meta_line(raw_line, colors));
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
    let new_start: u32 = new_part.split([',', ' ']).next()?.parse().ok()?;
    Some((old_start, new_start))
}

fn diff_header_line(raw: &str, colors: &DiffColors) -> Line<'static> {
    Line::from(Span::styled(
        raw.to_owned(),
        Style::default()
            .fg(colors.file_header_fg)
            .add_modifier(Modifier::BOLD),
    ))
}

fn file_header_line(raw: &str, colors: &DiffColors) -> Line<'static> {
    Line::from(Span::styled(
        raw.to_owned(),
        Style::default()
            .fg(colors.file_header_fg)
            .add_modifier(Modifier::BOLD),
    ))
}

fn hunk_header_line(raw: &str, colors: &DiffColors) -> Line<'static> {
    Line::from(Span::styled(
        raw.to_owned(),
        Style::default().fg(colors.hunk_fg),
    ))
}

/// What kind of content line this is (for gutter + background styling).
#[derive(Copy, Clone)]
enum ContentKind {
    Addition(u32),
    Deletion(u32),
    Context(u32, u32),
}

/// Build a syntax-highlighted content line with diff background tinting.
///
/// When `highlighter` is `None`, syntax highlighting is skipped (used for
/// oversized diffs where syntect would be too slow).
fn highlighted_content_line(
    highlighter: Option<&mut HighlightLines>,
    ss: &syntect::parsing::SyntaxSet,
    content: &str,
    kind: ContentKind,
    colors: &DiffColors,
) -> Line<'static> {
    let (gutter, sign, sign_fg, bg) = match kind {
        ContentKind::Addition(n) => (
            format!("    {:>4} ", n),
            "+",
            colors.add_fg,
            Some(colors.add_bg),
        ),
        ContentKind::Deletion(n) => (
            format!("{:<4}     ", n),
            "-",
            colors.del_fg,
            Some(colors.del_bg),
        ),
        ContentKind::Context(old, new) => (
            format!("{:<4}{:>4} ", old, new),
            " ",
            colors.context_fg,
            None,
        ),
    };

    let mut spans = vec![
        Span::styled(gutter, Style::default().fg(colors.gutter_fg)),
        Span::styled(
            sign.to_string(),
            Style::default().fg(sign_fg).add_modifier(if bg.is_some() {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
    ];

    // Feed the content through syntect for per-token highlighting.
    // Append a newline so syntect advances its parser state correctly.
    let line_with_nl = format!("{content}\n");
    let ranges = match highlighter {
        Some(hl) => hl.highlight_line(&line_with_nl, ss).unwrap_or_default(),
        None => Vec::new(),
    };

    if ranges.is_empty() {
        // Fallback: no highlighting available (or disabled), use flat diff color.
        let fg = match kind {
            ContentKind::Addition(_) => colors.add_fg,
            ContentKind::Deletion(_) => colors.del_fg,
            ContentKind::Context(_, _) => colors.context_fg,
        };
        let mut style = Style::default().fg(fg);
        if let Some(b) = bg {
            style = style.bg(b);
        }
        spans.push(Span::styled(content.to_owned(), style));
    } else {
        for (hl_style, text) in &ranges {
            let trimmed = text.trim_end_matches('\n');
            if trimmed.is_empty() {
                continue;
            }
            let fg = Color::Rgb(
                hl_style.foreground.r,
                hl_style.foreground.g,
                hl_style.foreground.b,
            );
            let mut style = Style::default().fg(fg);
            if let Some(b) = bg {
                style = style.bg(b);
            }
            spans.push(Span::styled(trimmed.to_owned(), style));
        }
    }

    Line::from(spans)
}

/// Detect the file extension from the first `diff --git` or `--- a/` line.
fn detect_extension(text: &str) -> Option<&str> {
    for line in text.lines() {
        if let Some(ext) = extract_extension_from_diff_header(line) {
            return Some(ext);
        }
        if line.starts_with("--- a/") || line.starts_with("+++ b/") {
            return line
                .rsplit('.')
                .next()
                .filter(|e| e.len() <= 10 && !e.contains('/'));
        }
        // Stop looking after the first hunk.
        if line.starts_with("@@ ") {
            break;
        }
    }
    None
}

/// Extract file extension from a "diff --git a/path b/path" line.
fn extract_extension_from_diff_header(line: &str) -> Option<&str> {
    if !line.starts_with("diff ") {
        return None;
    }
    // "diff --git a/foo/bar.rs b/foo/bar.rs" → "rs"
    let path = line.rsplit(' ').next()?;
    let path = path.strip_prefix("b/")?;
    path.rsplit('.')
        .next()
        .filter(|e| e.len() <= 10 && !e.contains('/'))
}

fn meta_line(raw: &str, colors: &DiffColors) -> Line<'static> {
    Line::from(Span::styled(
        raw.to_owned(),
        Style::default().fg(colors.context_fg),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

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
        // YAML separator / markdown / error messages starting with "---"
        assert!(!is_unified_diff("--- Error: not found"));
        assert!(!is_unified_diff("---\ntitle: something\n---"));
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
        let colors = DiffColors::default();
        let lines = diff_to_lines(SAMPLE_DIFF);
        // Find an addition line (should contain "+")
        let add_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "+"));
        assert!(add_line.is_some(), "should have an addition line");
        let spans = &add_line.unwrap().spans;
        let plus_span = spans.iter().find(|s| s.content.as_ref() == "+").unwrap();
        assert_eq!(plus_span.style.fg, Some(colors.add_fg));
    }

    #[test]
    fn deletions_use_red() {
        let colors = DiffColors::default();
        let lines = diff_to_lines(SAMPLE_DIFF);
        let del_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "-"));
        assert!(del_line.is_some(), "should have a deletion line");
        let spans = &del_line.unwrap().spans;
        let minus_span = spans.iter().find(|s| s.content.as_ref() == "-").unwrap();
        assert_eq!(minus_span.style.fg, Some(colors.del_fg));
    }

    #[test]
    fn hunk_headers_use_accent() {
        let colors = DiffColors::default();
        let lines = diff_to_lines(SAMPLE_DIFF);
        let hunk_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.starts_with("@@")));
        assert!(hunk_line.is_some());
        let span = &hunk_line.unwrap().spans[0];
        assert_eq!(span.style.fg, Some(colors.hunk_fg));
    }

    #[test]
    fn themed_diff_uses_custom_colors() {
        let colors = DiffColors {
            add_fg: Color::Cyan,
            add_bg: Color::Reset,
            del_fg: Color::Magenta,
            del_bg: Color::Reset,
            ..DiffColors::default()
        };
        let text = "@@ -1 +1 @@\n-old\n+new";
        let lines = diff_to_lines_themed(text, &colors);
        // Find addition line and verify color
        let add_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "+"))
            .unwrap();
        let plus_span = add_line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "+")
            .unwrap();
        assert_eq!(plus_span.style.fg, Some(Color::Cyan));
        // Find deletion line and verify color
        let del_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "-"))
            .unwrap();
        let minus_span = del_line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "-")
            .unwrap();
        assert_eq!(minus_span.style.fg, Some(Color::Magenta));
    }

    #[test]
    fn default_diff_colors_match_eve_theme() {
        let default = DiffColors::default();
        let eve = crate::theme::EveTheme;
        let themed = DiffColors::from_theme(&eve);
        assert_eq!(default.add_fg, themed.add_fg);
        assert_eq!(default.add_bg, themed.add_bg);
        assert_eq!(default.del_fg, themed.del_fg);
        assert_eq!(default.del_bg, themed.del_bg);
        assert_eq!(default.context_fg, themed.context_fg);
        assert_eq!(default.hunk_fg, themed.hunk_fg);
        assert_eq!(default.gutter_fg, themed.gutter_fg);
        assert_eq!(default.file_header_fg, themed.file_header_fg);
    }

    #[test]
    fn from_theme_uses_theme_colors() {
        let dracula = crate::theme::DraculaTheme;
        let colors = DiffColors::from_theme(&dracula);
        assert_eq!(colors.add_fg, dracula.diff_add_fg());
        assert_eq!(colors.del_fg, dracula.diff_del_fg());
        assert_eq!(colors.hunk_fg, dracula.primary());
        assert_eq!(colors.context_fg, dracula.text_dim());
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

    #[test]
    fn detect_extension_from_diff_header() {
        assert_eq!(
            extract_extension_from_diff_header("diff --git a/src/main.rs b/src/main.rs"),
            Some("rs")
        );
        assert_eq!(
            extract_extension_from_diff_header("diff --git a/foo.py b/foo.py"),
            Some("py")
        );
        assert_eq!(extract_extension_from_diff_header("not a diff line"), None);
    }

    #[test]
    fn detect_extension_from_full_diff() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs";
        assert_eq!(detect_extension(diff), Some("rs"));
    }

    #[test]
    fn syntax_highlighted_rust_diff_has_multiple_spans() {
        // A Rust diff should produce multiple colored spans per line
        // (keywords, types, etc. get different colors).
        let diff = "\
diff --git a/main.rs b/main.rs
--- a/main.rs
+++ b/main.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 42;
+    let x: u32 = 42;
 }";
        let lines = diff_to_lines(diff);
        // Find the addition line (contains "let x: u32")
        let add_line = lines.iter().find(|l| {
            l.spans
                .iter()
                .any(|s| s.content.contains("u32") || s.content.contains("let"))
        });
        assert!(
            add_line.is_some(),
            "should find the addition line with Rust code"
        );
        // With syntax highlighting, the addition line should have more than
        // just gutter + sign + flat content (3 spans). Keywords like "let"
        // and types like "u32" should get separate spans.
        let span_count = add_line.unwrap().spans.len();
        assert!(
            span_count > 3,
            "syntax-highlighted line should have >3 spans, got {span_count}"
        );
    }

    #[test]
    fn plain_text_diff_still_renders() {
        // A diff without a recognizable extension should still render correctly.
        let diff = "@@ -1 +1 @@\n-old line\n+new line";
        let lines = diff_to_lines(diff);
        assert_eq!(lines.len(), 3);
    }
}
