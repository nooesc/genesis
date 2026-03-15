//! Markdown-to-ratatui adapter.
//!
//! Converts markdown text into `Vec<Line<'static>>` with appropriate ratatui
//! styles, suitable for direct rendering inside the inline TUI viewport.
//!
//! ## Supported syntax
//! - **Bold** (`**text**` / `__text__`) → `Modifier::BOLD`
//! - *Italic* (`*text*` / `_text_`) → `Modifier::ITALIC`
//! - `Inline code` (`` `code` ``) → gray background + light text
//! - Code fences (` ```lang … ``` `) → syntax-highlighted via syntect
//! - Headers (`# H1`, `## H2`, …) → bold + `EVE_LAVENDER` accent colour
//! - Unordered list items (`- item`, `* item`) → bullet `•`
//! - Ordered list items (`1. item`) → original number preserved
//! - Plain text → default text colour `Color::Rgb(208, 208, 208)`
//!
//! ## Non-goals (can be added later)
//! Links, images, tables, blockquotes, HTML tags.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

// ── Eve / UI palette (mirrored from genesis_ui::colors to avoid a runtime dep) ──

/// `EVE_LAVENDER` — accent colour for headers.
const ACCENT: Color = Color::Rgb(180, 167, 214);
/// Default plain-text colour.
const TEXT: Color = Color::Rgb(208, 208, 208);
/// Inline-code background.
const CODE_BG: Color = Color::Rgb(50, 50, 50);
/// Inline-code foreground.
const CODE_FG: Color = Color::Rgb(200, 200, 200);

// ── Lazy-loaded syntect state ─────────────────────────────────────────────────

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

fn get_syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn get_theme() -> &'static Theme {
    THEME.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes["base16-ocean.dark"].clone()
    })
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Convert a markdown string into a list of styled ratatui [`Line`]s.
///
/// The output is `'static` (all strings are owned) so the lines can be stored
/// in widgets or scrollback without lifetime complications.
pub fn markdown_to_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_code_fence = false;
    let mut fence_lang = String::new();
    let mut code_buf: Vec<String> = Vec::new();

    for raw_line in text.lines() {
        if in_code_fence {
            // Check for closing fence (``` or ~~~).
            if raw_line.trim_start().starts_with("```")
                || raw_line.trim_start().starts_with("~~~")
            {
                // Render the accumulated code block.
                let highlighted = highlight_code(&code_buf.join("\n"), &fence_lang);
                lines.extend(highlighted);
                in_code_fence = false;
                fence_lang.clear();
                code_buf.clear();
            } else {
                code_buf.push(raw_line.to_owned());
            }
        } else if raw_line.trim_start().starts_with("```")
            || raw_line.trim_start().starts_with("~~~")
        {
            // Opening fence — extract optional language tag.
            let trimmed = raw_line.trim_start();
            let lang = trimmed
                .trim_start_matches('`')
                .trim_start_matches('~')
                .trim()
                .to_owned();
            fence_lang = lang;
            in_code_fence = true;
            code_buf.clear();
        } else {
            lines.push(render_inline(raw_line));
        }
    }

    // Flush an unclosed code fence as plain styled lines.
    if in_code_fence && !code_buf.is_empty() {
        let highlighted = highlight_code(&code_buf.join("\n"), &fence_lang);
        lines.extend(highlighted);
    }

    lines
}

// ── Code fence rendering ──────────────────────────────────────────────────────

/// Syntax-highlight `code` for `lang` using syntect and return one
/// [`Line`] per source line.
///
/// Falls back to plain `CODE_FG` text on a `CODE_BG` background when the
/// language is unknown or when syntect returns an empty range list.
fn highlight_code(code: &str, lang: &str) -> Vec<Line<'static>> {
    let ss = get_syntax_set();
    let syntax = if lang.is_empty() {
        ss.find_syntax_plain_text()
    } else {
        ss.find_syntax_by_token(lang)
            .unwrap_or_else(|| ss.find_syntax_plain_text())
    };

    let theme = get_theme();
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut result: Vec<Line<'static>> = Vec::new();

    for line in LinesWithEndings::from(code) {
        let ranges = highlighter.highlight_line(line, ss).unwrap_or_default();

        if ranges.is_empty() {
            // Plain fallback for this line.
            let content = line.trim_end_matches('\n').to_owned();
            result.push(Line::from(vec![Span::styled(
                content,
                Style::default().fg(CODE_FG).bg(CODE_BG),
            )]));
            continue;
        }

        let spans: Vec<Span<'static>> = ranges
            .iter()
            .map(|(style, text)| {
                let fg = Color::Rgb(
                    style.foreground.r,
                    style.foreground.g,
                    style.foreground.b,
                );
                // Strip the trailing newline that syntect includes in the last
                // token of each line so it doesn't bleed into the next ratatui
                // line.
                let content = text.trim_end_matches('\n').to_owned();
                Span::styled(content, Style::default().fg(fg).bg(CODE_BG))
            })
            .collect();

        result.push(Line::from(spans));
    }

    result
}

// ── Inline formatting parser ──────────────────────────────────────────────────

/// Render a single (non-fence) markdown line into a ratatui [`Line`].
///
/// Handles:
/// - `#` / `##` / … headers
/// - `- ` / `* ` unordered list items
/// - `N. ` ordered list items
/// - Inline `**bold**`, `*italic*`, `__bold__`, `_italic_`, `` `code` ``
fn render_inline(line: &str) -> Line<'static> {
    // ── Headers ─────────────────────────────────────────────────────────────
    if let Some(rest) = parse_header(line) {
        let spans = parse_inline_spans(rest, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));
        return Line::from(spans);
    }

    // ── Unordered list items ─────────────────────────────────────────────────
    if let Some(rest) = parse_unordered_list(line) {
        let mut spans = vec![Span::styled("• ".to_owned(), Style::default().fg(TEXT))];
        spans.extend(parse_inline_spans(rest, Style::default().fg(TEXT)));
        return Line::from(spans);
    }

    // ── Ordered list items ───────────────────────────────────────────────────
    if let Some((prefix, rest)) = parse_ordered_list(line) {
        let mut spans =
            vec![Span::styled(prefix.to_owned(), Style::default().fg(TEXT))];
        spans.extend(parse_inline_spans(rest, Style::default().fg(TEXT)));
        return Line::from(spans);
    }

    // ── Plain text (with possible inline formatting) ─────────────────────────
    Line::from(parse_inline_spans(line, Style::default().fg(TEXT)))
}

// ── Line-type detectors ───────────────────────────────────────────────────────

/// If `line` starts with one or more `#` followed by a space, return the
/// remaining heading text (without the leading `# `).
fn parse_header(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches('#');
    let hashes = line.len() - trimmed.len();
    if hashes > 0 && hashes <= 6 {
        if let Some(rest) = trimmed.strip_prefix(' ') {
            return Some(rest);
        }
    }
    None
}

/// If `line` is an unordered list item (`- ` or `* ` prefix), return the
/// item text (without the leading marker and space).
fn parse_unordered_list(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("- ") {
        return Some(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("* ") {
        return Some(rest);
    }
    None
}

/// If `line` is an ordered list item (`N. `), return the numeric prefix
/// string (e.g. `"1. "`) and the rest of the text.
fn parse_ordered_list(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    // Find `<digits>. ` pattern.
    let dot_pos = trimmed.find(". ")?;
    let digits = &trimmed[..dot_pos];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let rest = &trimmed[dot_pos + 2..];
    Some((format!("{}. ", digits), rest))
}

// ── Inline span parser ────────────────────────────────────────────────────────

/// Parse a string containing inline markdown (`**bold**`, `*italic*`,
/// `__bold__`, `_italic_`, `` `code` ``) into a list of styled [`Span`]s.
///
/// `base` is the style applied to unstyled plain-text regions.  Inline
/// formatters *add* modifiers on top of `base` so that e.g. bold inside a
/// header preserves the header accent colour.
fn parse_inline_spans(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    let len = bytes.len();
    // Accumulate plain text until we hit a formatting marker.
    let mut plain_start = 0;

    while i < len {
        // ── Bold: `**` or `__` ──────────────────────────────────────────
        if starts_with_at(bytes, i, b"**") {
            if let Some(end) = find_closing(bytes, i + 2, b"**") {
                flush_plain(text, plain_start, i, base, &mut spans);
                let inner = &text[i + 2..end];
                spans.push(Span::styled(
                    inner.to_owned(),
                    base.add_modifier(Modifier::BOLD),
                ));
                i = end + 2;
                plain_start = i;
                continue;
            }
        }
        if starts_with_at(bytes, i, b"__") {
            if let Some(end) = find_closing(bytes, i + 2, b"__") {
                flush_plain(text, plain_start, i, base, &mut spans);
                let inner = &text[i + 2..end];
                spans.push(Span::styled(
                    inner.to_owned(),
                    base.add_modifier(Modifier::BOLD),
                ));
                i = end + 2;
                plain_start = i;
                continue;
            }
        }

        // ── Italic: `*` or `_` (single, not double) ──────────────────────
        // We check for single `*` / `_` only after ruling out the double case.
        if bytes[i] == b'*' && !starts_with_at(bytes, i, b"**") {
            if let Some(end) = find_closing_char(bytes, i + 1, b'*') {
                flush_plain(text, plain_start, i, base, &mut spans);
                let inner = &text[i + 1..end];
                spans.push(Span::styled(
                    inner.to_owned(),
                    base.add_modifier(Modifier::ITALIC),
                ));
                i = end + 1;
                plain_start = i;
                continue;
            }
        }
        if bytes[i] == b'_' && !starts_with_at(bytes, i, b"__") {
            if let Some(end) = find_closing_char(bytes, i + 1, b'_') {
                flush_plain(text, plain_start, i, base, &mut spans);
                let inner = &text[i + 1..end];
                spans.push(Span::styled(
                    inner.to_owned(),
                    base.add_modifier(Modifier::ITALIC),
                ));
                i = end + 1;
                plain_start = i;
                continue;
            }
        }

        // ── Inline code: `` ` `` ─────────────────────────────────────────
        if bytes[i] == b'`' {
            if let Some(end) = find_closing_char(bytes, i + 1, b'`') {
                flush_plain(text, plain_start, i, base, &mut spans);
                let inner = &text[i + 1..end];
                spans.push(Span::styled(
                    inner.to_owned(),
                    Style::default().fg(CODE_FG).bg(CODE_BG),
                ));
                i = end + 1;
                plain_start = i;
                continue;
            }
        }

        i += 1;
    }

    // Flush any remaining plain text.
    flush_plain(text, plain_start, len, base, &mut spans);

    if spans.is_empty() {
        // Always return at least one span so the line is non-empty.
        spans.push(Span::styled(String::new(), base));
    }

    spans
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Append a plain-text span for `text[start..end]` if non-empty.
#[inline]
fn flush_plain(text: &str, start: usize, end: usize, style: Style, spans: &mut Vec<Span<'static>>) {
    if start < end {
        spans.push(Span::styled(text[start..end].to_owned(), style));
    }
}

/// Returns `true` if `bytes[pos..]` starts with `needle`.
#[inline]
fn starts_with_at(bytes: &[u8], pos: usize, needle: &[u8]) -> bool {
    bytes.len() >= pos + needle.len() && &bytes[pos..pos + needle.len()] == needle
}

/// Find the next occurrence of `needle` (2-byte sequence) in `bytes[from..]`.
/// Returns the **start** index of the match within the original `bytes` slice.
fn find_closing(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let mut i = from;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Find the next occurrence of a single `byte` in `bytes[from..]`.
/// Returns the index of that byte in the original `bytes` slice.
fn find_closing_char(bytes: &[u8], from: usize, byte: u8) -> Option<usize> {
    bytes[from..].iter().position(|&b| b == byte).map(|p| from + p)
}

// ── syntect line-with-endings iterator (internal use) ────────────────────────

/// Minimal re-implementation of syntect's `LinesWithEndings` that keeps the
/// trailing `\n` on each line so the highlighter sees complete lines.
struct LinesWithEndings<'a> {
    text: &'a str,
}

impl<'a> LinesWithEndings<'a> {
    fn from(text: &'a str) -> Self {
        Self { text }
    }
}

impl<'a> Iterator for LinesWithEndings<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.text.is_empty() {
            return None;
        }
        match self.text.find('\n') {
            Some(pos) => {
                let (line, rest) = self.text.split_at(pos + 1);
                self.text = rest;
                Some(line)
            }
            None => {
                let line = self.text;
                self.text = "";
                Some(line)
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    // ── Plain text ────────────────────────────────────────────────────────────

    #[test]
    fn plain_text_produces_single_span() {
        let lines = markdown_to_lines("hello world");
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world");
        assert_eq!(spans[0].style.fg, Some(TEXT));
    }

    #[test]
    fn plain_text_color_is_default_text() {
        let lines = markdown_to_lines("simple text");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(208, 208, 208)));
    }

    // ── Bold ─────────────────────────────────────────────────────────────────

    #[test]
    fn bold_text_has_bold_modifier() {
        let lines = markdown_to_lines("**bold**");
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        // Should have a span containing "bold" with BOLD modifier.
        let bold_span = spans.iter().find(|s| s.content == "bold").expect("bold span");
        assert!(
            bold_span.style.add_modifier.contains(Modifier::BOLD),
            "expected BOLD modifier on bold span"
        );
    }

    #[test]
    fn double_underscore_bold_has_bold_modifier() {
        let lines = markdown_to_lines("__bold__");
        let spans = &lines[0].spans;
        let bold_span = spans.iter().find(|s| s.content == "bold").expect("bold span");
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    // ── Italic ────────────────────────────────────────────────────────────────

    #[test]
    fn italic_text_has_italic_modifier() {
        let lines = markdown_to_lines("*italic*");
        let spans = &lines[0].spans;
        let italic_span = spans.iter().find(|s| s.content == "italic").expect("italic span");
        assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn underscore_italic_has_italic_modifier() {
        let lines = markdown_to_lines("_italic_");
        let spans = &lines[0].spans;
        let italic_span = spans.iter().find(|s| s.content == "italic").expect("italic span");
        assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    // ── Inline code ───────────────────────────────────────────────────────────

    #[test]
    fn inline_code_has_gray_background() {
        let lines = markdown_to_lines("`code`");
        let spans = &lines[0].spans;
        let code_span = spans.iter().find(|s| s.content == "code").expect("code span");
        assert_eq!(
            code_span.style.bg,
            Some(CODE_BG),
            "inline code should have gray background"
        );
        assert_eq!(
            code_span.style.fg,
            Some(CODE_FG),
            "inline code should have light foreground"
        );
    }

    // ── Code fences ───────────────────────────────────────────────────────────

    #[test]
    fn code_fence_produces_highlighted_lines() {
        let md = "```rust\nlet x = 42;\n```";
        let lines = markdown_to_lines(md);
        // Should produce at least 1 line for the code.
        assert!(!lines.is_empty(), "code fence should produce at least one line");
        // Every span in every line should have the CODE_BG background.
        for line in &lines {
            for span in &line.spans {
                assert_eq!(
                    span.style.bg,
                    Some(CODE_BG),
                    "code fence span should have CODE_BG background"
                );
            }
        }
    }

    #[test]
    fn code_fence_no_lang_still_renders() {
        let md = "```\nhello world\n```";
        let lines = markdown_to_lines(md);
        assert!(!lines.is_empty());
    }

    #[test]
    fn code_fence_each_source_line_becomes_ratatui_line() {
        let md = "```\nline1\nline2\nline3\n```";
        let lines = markdown_to_lines(md);
        // 3 source lines → 3 ratatui Lines.
        assert_eq!(lines.len(), 3, "each code line should map to one ratatui Line");
    }

    // ── Headers ───────────────────────────────────────────────────────────────

    #[test]
    fn headers_are_bold_with_accent_color() {
        let lines = markdown_to_lines("# Hello");
        assert_eq!(lines.len(), 1);
        let span = &lines[0].spans[0];
        assert_eq!(span.content, "Hello");
        assert_eq!(span.style.fg, Some(ACCENT), "header should use accent colour");
        assert!(
            span.style.add_modifier.contains(Modifier::BOLD),
            "header should be bold"
        );
    }

    #[test]
    fn h2_header_is_bold_accent() {
        let lines = markdown_to_lines("## Section");
        let span = &lines[0].spans[0];
        assert_eq!(span.content, "Section");
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(span.style.fg, Some(ACCENT));
    }

    // ── List items ────────────────────────────────────────────────────────────

    #[test]
    fn list_items_preserved() {
        let lines = markdown_to_lines("- first item");
        assert_eq!(lines.len(), 1);
        // First span should be the bullet character.
        assert_eq!(lines[0].spans[0].content, "• ");
        // Second span should contain the item text.
        let text: String = lines[0].spans[1..].iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("first item"));
    }

    #[test]
    fn asterisk_list_item_uses_bullet() {
        let lines = markdown_to_lines("* second item");
        assert_eq!(lines[0].spans[0].content, "• ");
    }

    #[test]
    fn ordered_list_item_preserves_number() {
        let lines = markdown_to_lines("1. first");
        assert_eq!(lines.len(), 1);
        // First span should include the "1. " prefix.
        assert_eq!(lines[0].spans[0].content, "1. ");
    }

    // ── Mixed formatting in a single line ─────────────────────────────────────

    #[test]
    fn mixed_formatting_in_single_line() {
        let lines = markdown_to_lines("normal **bold** and `code`");
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        // Should have: "normal " (plain), "bold" (bold), " and " (plain), "code" (code).
        let bold_span = spans.iter().find(|s| s.content == "bold").expect("bold span");
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));

        let code_span = spans.iter().find(|s| s.content == "code").expect("code span");
        assert_eq!(code_span.style.bg, Some(CODE_BG));
    }

    #[test]
    fn bold_inside_header_retains_accent_color() {
        // Bold inside a header should keep the accent fg but also be bold.
        let lines = markdown_to_lines("# **bold heading**");
        let bold_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "bold heading")
            .expect("bold span in header");
        assert_eq!(bold_span.style.fg, Some(ACCENT));
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    // ── Multi-line document ───────────────────────────────────────────────────

    #[test]
    fn multi_line_document_produces_correct_line_count() {
        let md = "# Title\n\nsome text\n\n- item one\n- item two";
        let lines = markdown_to_lines(md);
        // title, blank, text, blank, item1, item2 → 6 lines
        assert_eq!(lines.len(), 6);
    }

    #[test]
    fn unclosed_code_fence_flushes_as_highlighted() {
        // An unclosed fence should still produce lines rather than dropping them.
        let md = "```rust\nlet x = 1;";
        let lines = markdown_to_lines(md);
        assert!(!lines.is_empty(), "unclosed fence should still produce lines");
    }

    // ── Blank lines ───────────────────────────────────────────────────────────

    #[test]
    fn blank_line_produces_empty_line() {
        let lines = markdown_to_lines("");
        // A single empty string → zero lines (no '\n' to iterate over).
        assert_eq!(lines.len(), 0);
    }
}
