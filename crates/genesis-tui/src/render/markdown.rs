//! Markdown-to-ratatui adapter using pulldown-cmark.
//!
//! Converts markdown text into `Vec<Line<'static>>` with appropriate ratatui
//! styles, suitable for direct rendering inside the inline TUI viewport.
//!
//! ## Supported syntax
//! - **Bold** (`**text**` / `__text__`) → `Modifier::BOLD`
//! - *Italic* (`*text*` / `_text_`) → `Modifier::ITALIC`
//! - ***Bold italic*** (`***text***`) → `Modifier::BOLD | Modifier::ITALIC`
//! - ~~Strikethrough~~ (`~~text~~`) → `Modifier::CROSSED_OUT`
//! - `Inline code` (`` `code` ``) → gray background + light text
//! - Code fences (` ```lang … ``` `) → syntax-highlighted via syntect
//! - Headers (`# H1`, `## H2`, …) → bold + `EVE_LAVENDER` accent colour
//! - Unordered list items → bullet `•` with indent
//! - Ordered list items → number with indent
//! - Blockquotes (`> text`) → `│ ` prefix with dimmed colour
//! - Tables → aligned columns with box-drawing borders
//! - Links (`[text](url)`) → text with URL visible
//! - Horizontal rules (`---`) → `───` separator line

use std::sync::OnceLock;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::history::rgb;

// ── Eve / UI palette ─────────────────────────────────────────────────────────

/// `EVE_LAVENDER` — accent colour for headers.
const ACCENT: Color = rgb(genesis_ui::colors::EVE_LAVENDER);
/// Default plain-text colour.
const TEXT: Color = rgb(genesis_ui::colors::UI_TEXT);
/// Dim colour for blockquote prefix, horizontal rules, code-fence dashes.
const DIM: Color = rgb(genesis_ui::colors::UI_DIM);
/// Muted colour for code-fence language labels.
const MUTED: Color = rgb(genesis_ui::colors::UI_MUTED);
/// Inline-code background.
const CODE_BG: Color = Color::Rgb(50, 50, 50);
/// Inline-code foreground.
const CODE_FG: Color = Color::Rgb(200, 200, 200);
/// Link URL colour.
const LINK_COLOR: Color = Color::Rgb(100, 149, 237); // cornflower blue
/// Subtle background for odd-numbered table data rows (zebra striping).
const TABLE_STRIPE_BG: Color = Color::Rgb(35, 35, 40);
/// Blockquote depth-2 prefix colour (slightly brighter than DIM).
const BLOCKQUOTE_DEPTH2: Color = Color::Rgb(128, 128, 128);
/// Blockquote depth-3+ prefix colour (brighter still).
const BLOCKQUOTE_DEPTH3: Color = Color::Rgb(148, 148, 148);

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

// ── Blockquote prefix helper ─────────────────────────────────────────────────

/// Build per-depth blockquote prefix spans with varying characters and colours.
///
/// - Depth 1: `│ ` in DIM
/// - Depth 2: `┃ ` in slightly brighter grey
/// - Depth 3+: `║ ` in even brighter grey
fn blockquote_prefix(depth: usize) -> Vec<Span<'static>> {
    (0..depth)
        .map(|i| {
            let (ch, color) = match i {
                0 => ("│ ", DIM),
                1 => ("┃ ", BLOCKQUOTE_DEPTH2),
                _ => ("║ ", BLOCKQUOTE_DEPTH3),
            };
            Span::styled(ch.to_string(), Style::default().fg(color))
        })
        .collect()
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Convert a markdown string into a list of styled ratatui [`Line`]s.
///
/// The output is `'static` (all strings are owned) so the lines can be stored
/// in widgets or scrollback without lifetime complications.
pub fn markdown_to_lines(text: &str) -> Vec<Line<'static>> {
    let opts =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_SMART_PUNCTUATION;
    let parser = Parser::new_ext(text, opts);
    let mut writer = MarkdownWriter::new();
    writer.process(parser);
    writer.finish()
}

// ── MarkdownWriter state machine ──────────────────────────────────────────────

/// Tracks the formatting context as we walk pulldown-cmark events.
struct MarkdownWriter {
    /// Completed output lines.
    lines: Vec<Line<'static>>,
    /// Spans accumulating for the current line.
    current_spans: Vec<Span<'static>>,
    /// Stack of active styles (for nested bold/italic/etc.).
    style_stack: Vec<Style>,
    /// Current blockquote nesting depth.
    blockquote_depth: usize,
    /// List context stack: None = unordered, Some(n) = ordered starting at n.
    list_stack: Vec<Option<u64>>,
    /// Current ordered list item index (for numbered items).
    list_item_index: Vec<u64>,
    /// Whether we're at the start of a list item (need to emit bullet/number).
    at_list_item_start: bool,
    /// Code block accumulator: (language, buffer).
    code_block: Option<(String, String)>,
    /// Table state: column alignments.
    table_alignments: Vec<Alignment>,
    /// Table rows: each row is a Vec of cell contents (Vec<Span>).
    table_rows: Vec<Vec<Vec<Span<'static>>>>,
    /// Current table cell spans being accumulated.
    table_cell_spans: Vec<Span<'static>>,
    /// Whether we're inside a table.
    in_table: bool,
    /// Link destination being accumulated.
    link_url: Option<String>,
}

impl MarkdownWriter {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            current_spans: Vec::new(),
            style_stack: vec![Style::default().fg(TEXT)],
            blockquote_depth: 0,
            list_stack: Vec::new(),
            list_item_index: Vec::new(),
            at_list_item_start: false,
            code_block: None,
            table_alignments: Vec::new(),
            table_rows: Vec::new(),
            table_cell_spans: Vec::new(),
            in_table: false,
            link_url: None,
        }
    }

    fn current_style(&self) -> Style {
        self.style_stack
            .last()
            .copied()
            .unwrap_or(Style::default().fg(TEXT))
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(style);
    }

    fn pop_style(&mut self) {
        if self.style_stack.len() > 1 {
            self.style_stack.pop();
        }
    }

    /// Emit text with the current style.
    fn push_text(&mut self, text: &str) {
        if self.in_table {
            self.table_cell_spans
                .push(Span::styled(text.to_owned(), self.current_style()));
            return;
        }
        if let Some((_, ref mut buf)) = self.code_block {
            buf.push_str(text);
            return;
        }
        self.current_spans
            .push(Span::styled(text.to_owned(), self.current_style()));
    }

    /// Finish the current line and start a new one.
    fn finish_line(&mut self) {
        let mut spans = std::mem::take(&mut self.current_spans);

        // Add blockquote prefix if needed.
        if self.blockquote_depth > 0 {
            let mut prefix = blockquote_prefix(self.blockquote_depth);
            prefix.append(&mut spans);
            spans = prefix;
        }

        if spans.is_empty() {
            self.lines.push(Line::default());
        } else {
            self.lines.push(Line::from(spans));
        }
    }

    fn process(&mut self, parser: Parser<'_>) {
        for event in parser {
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: Event<'_>) {
        match event {
            // ── Block elements ────────────────────────────────────────────
            Event::Start(Tag::Heading { level, .. }) => {
                let _ = level; // all heading levels get same style
                self.push_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Heading(_)) => {
                self.pop_style();
                self.finish_line();
            }

            Event::Start(Tag::Paragraph) => {
                // Nothing special at paragraph start.
            }
            Event::End(TagEnd::Paragraph) => {
                self.finish_line();
                // Add blank line after paragraphs (unless in a list item,
                // table, or blockquote — blockquote prefix would produce
                // a visible blank row with just "│ ").
                if self.list_stack.is_empty() && !self.in_table && self.blockquote_depth == 0 {
                    self.finish_line();
                }
            }

            Event::Start(Tag::BlockQuote(_)) => {
                self.blockquote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
            }

            Event::Start(Tag::List(start)) => {
                if let Some(n) = start {
                    self.list_stack.push(Some(n));
                    self.list_item_index.push(n);
                } else {
                    self.list_stack.push(None);
                    self.list_item_index.push(0);
                }
            }
            Event::End(TagEnd::List(_)) => {
                self.list_stack.pop();
                self.list_item_index.pop();
            }

            Event::Start(Tag::Item) => {
                self.at_list_item_start = true;
            }
            Event::End(TagEnd::Item) => {
                // Item content is flushed by inner Paragraph end.
            }

            // ── Code blocks ──────────────────────────────────────────────
            Event::Start(Tag::CodeBlock(kind)) => {
                // Emit list prefix if this code block is the first content
                // in a list item (e.g. `- ```rust\ncode\n```).
                if self.at_list_item_start && !self.in_table {
                    self.emit_list_prefix();
                    self.at_list_item_start = false;
                    // Flush the bullet line before code block content is
                    // written directly to self.lines.
                    self.finish_line();
                }
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code_block = Some((lang, String::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((lang, code)) = self.code_block.take() {
                    // Emit language label line if the fence specified a language.
                    if !lang.is_empty() {
                        self.emit_code_lang_label(&lang);
                    }

                    let highlighted = highlight_code(&code, &lang);
                    // Add blockquote prefix to code lines if needed.
                    if self.blockquote_depth > 0 {
                        for mut line in highlighted {
                            let mut spans = blockquote_prefix(self.blockquote_depth);
                            spans.append(&mut line.spans);
                            self.lines.push(Line::from(spans));
                        }
                    } else {
                        self.lines.extend(highlighted);
                    }
                }
            }

            // ── Tables ───────────────────────────────────────────────────
            Event::Start(Tag::Table(alignments)) => {
                self.in_table = true;
                self.table_alignments = alignments;
                self.table_rows.clear();
            }
            Event::End(TagEnd::Table) => {
                self.in_table = false;
                self.render_table();
            }

            Event::Start(Tag::TableHead) | Event::End(TagEnd::TableHead) => {}

            Event::Start(Tag::TableRow) => {
                self.table_rows.push(Vec::new());
            }
            Event::End(TagEnd::TableRow) => {}

            Event::Start(Tag::TableCell) => {
                self.table_cell_spans.clear();
            }
            Event::End(TagEnd::TableCell) => {
                let spans = std::mem::take(&mut self.table_cell_spans);
                if let Some(row) = self.table_rows.last_mut() {
                    row.push(spans);
                }
            }

            // ── Inline formatting ────────────────────────────────────────
            Event::Start(Tag::Strong) => {
                let style = self.current_style().add_modifier(Modifier::BOLD);
                self.push_style(style);
            }
            Event::End(TagEnd::Strong) => {
                self.pop_style();
            }

            Event::Start(Tag::Emphasis) => {
                let style = self.current_style().add_modifier(Modifier::ITALIC);
                self.push_style(style);
            }
            Event::End(TagEnd::Emphasis) => {
                self.pop_style();
            }

            Event::Start(Tag::Strikethrough) => {
                let style = self.current_style().add_modifier(Modifier::CROSSED_OUT);
                self.push_style(style);
            }
            Event::End(TagEnd::Strikethrough) => {
                self.pop_style();
            }

            Event::Start(Tag::Link { dest_url, .. }) => {
                self.link_url = Some(dest_url.to_string());
            }
            Event::End(TagEnd::Link) => {
                if let Some(url) = self.link_url.take() {
                    if self.in_table {
                        self.table_cell_spans.push(Span::styled(
                            format!(" ({url})"),
                            Style::default().fg(LINK_COLOR),
                        ));
                    } else {
                        self.current_spans.push(Span::styled(
                            format!(" ({url})"),
                            Style::default().fg(LINK_COLOR),
                        ));
                    }
                }
            }

            // ── Text content ─────────────────────────────────────────────
            Event::Text(text) => {
                // Handle list item prefix on first text.
                if self.at_list_item_start && !self.in_table {
                    self.emit_list_prefix();
                    self.at_list_item_start = false;
                }
                self.push_text(&text);
            }

            Event::Code(code) => {
                // Emit list prefix if this is the first inline node in a list item.
                if self.at_list_item_start && !self.in_table {
                    self.emit_list_prefix();
                    self.at_list_item_start = false;
                }
                // Inline code.
                if self.in_table {
                    self.table_cell_spans.push(Span::styled(
                        code.to_string(),
                        Style::default().fg(CODE_FG).bg(CODE_BG),
                    ));
                } else {
                    self.current_spans.push(Span::styled(
                        code.to_string(),
                        Style::default().fg(CODE_FG).bg(CODE_BG),
                    ));
                }
            }

            Event::SoftBreak => {
                if self.in_table {
                    // Inside table cells, soft breaks become spaces.
                    self.table_cell_spans.push(Span::raw(" ".to_owned()));
                } else if self.code_block.is_some() {
                    // Inside code blocks, preserve newlines.
                    if let Some((_, ref mut buf)) = self.code_block {
                        buf.push('\n');
                    }
                } else {
                    // In a TUI rendering LLM output, preserve line breaks rather
                    // than collapsing to spaces (CommonMark default). LLMs
                    // frequently use single newlines as intentional line breaks.
                    self.finish_line();
                    // Re-apply list indentation on continuation lines.
                    if !self.list_stack.is_empty() {
                        let depth = self.list_stack.len();
                        // Each nesting level uses 2 chars of indent, plus 2 for the bullet "• ".
                        let prefix_width = depth * 2;
                        self.current_spans.push(Span::raw(" ".repeat(prefix_width)));
                    }
                }
            }

            Event::HardBreak => {
                self.finish_line();
            }

            Event::Rule => {
                let rule = "─".repeat(60);
                self.current_spans
                    .push(Span::styled(rule, Style::default().fg(DIM)));
                self.finish_line();
            }

            // Ignore HTML, footnotes, etc.
            _ => {}
        }
    }

    /// Emit the bullet or number prefix for a list item.
    fn emit_list_prefix(&mut self) {
        let depth = self.list_stack.len().saturating_sub(1);
        let indent = "  ".repeat(depth);

        if let Some(list_type) = self.list_stack.last() {
            match list_type {
                None => {
                    // Unordered list.
                    self.current_spans.push(Span::styled(
                        format!("{indent}• "),
                        Style::default().fg(TEXT),
                    ));
                }
                Some(_) => {
                    // Ordered list.
                    if let Some(idx) = self.list_item_index.last_mut() {
                        self.current_spans.push(Span::styled(
                            format!("{indent}{}. ", idx),
                            Style::default().fg(TEXT),
                        ));
                        *idx += 1;
                    }
                }
            }
        }
    }

    /// Emit a `─[ lang ]────…` label line above a fenced code block.
    fn emit_code_lang_label(&mut self, lang: &str) {
        const LABEL_WIDTH: usize = 60;
        let lang_lower = lang.to_lowercase();
        // "─[ rust ]" = 1 dash + "[ " + lang + " ]" = 5 fixed chars + lang len
        let prefix_len = 1 + 2 + lang_lower.len() + 2; // "─" + "[ " + lang + " ]"
        let trail = LABEL_WIDTH.saturating_sub(prefix_len);

        let mut spans = Vec::new();

        // Blockquote prefix if needed.
        if self.blockquote_depth > 0 {
            spans.extend(blockquote_prefix(self.blockquote_depth));
        }

        spans.push(Span::styled("─[ ", Style::default().fg(DIM)));
        spans.push(Span::styled(lang_lower, Style::default().fg(MUTED)));
        spans.push(Span::styled(
            format!(" ]{}", "─".repeat(trail)),
            Style::default().fg(DIM),
        ));

        self.lines.push(Line::from(spans));
    }

    /// Render the accumulated table data into lines.
    fn render_table(&mut self) {
        use unicode_width::UnicodeWidthStr;

        if self.table_rows.is_empty() {
            return;
        }

        // Compute column widths using Unicode display width (not byte count).
        let num_cols = self.table_rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let mut col_widths = vec![0usize; num_cols];
        for row in &self.table_rows {
            for (i, cell) in row.iter().enumerate() {
                let width: usize = cell
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                col_widths[i] = col_widths[i].max(width);
            }
        }

        // Render header row (first row).
        if let Some(header) = self.table_rows.first() {
            let line = self.render_table_row(header, &col_widths, true, false);
            self.lines.push(line);

            // Separator line — constructed without byte-slicing to avoid
            // panics on multi-byte characters like `─` (U+2500, 3 bytes).
            let sep: String = col_widths
                .iter()
                .enumerate()
                .map(|(i, &w)| {
                    let align = self
                        .table_alignments
                        .get(i)
                        .copied()
                        .unwrap_or(Alignment::None);
                    let seg_width = w + 2;
                    match align {
                        Alignment::Left => {
                            format!(":{}", "─".repeat(seg_width.saturating_sub(1)))
                        }
                        Alignment::Right => {
                            format!("{}:", "─".repeat(seg_width.saturating_sub(1)))
                        }
                        Alignment::Center => {
                            format!(":{}:", "─".repeat(seg_width.saturating_sub(2)))
                        }
                        Alignment::None => "─".repeat(seg_width),
                    }
                })
                .collect::<Vec<_>>()
                .join("┼");
            self.lines
                .push(Line::from(Span::styled(sep, Style::default().fg(DIM))));
        }

        // Render data rows with zebra striping on odd rows.
        for (i, row) in self.table_rows.iter().skip(1).enumerate() {
            let stripe = i % 2 == 1;
            let line = self.render_table_row(row, &col_widths, false, stripe);
            self.lines.push(line);
        }
    }

    fn render_table_row(
        &self,
        row: &[Vec<Span<'static>>],
        col_widths: &[usize],
        is_header: bool,
        stripe: bool,
    ) -> Line<'static> {
        use unicode_width::UnicodeWidthStr;
        let mut spans: Vec<Span<'static>> = Vec::new();

        for (i, cell) in row.iter().enumerate() {
            let width = col_widths.get(i).copied().unwrap_or(0);
            let content_len: usize = cell
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            let padding = width.saturating_sub(content_len);
            let align = self
                .table_alignments
                .get(i)
                .copied()
                .unwrap_or(Alignment::None);

            let (pad_left, pad_right) = match align {
                Alignment::Right => (padding, 0),
                Alignment::Center => (padding / 2, padding - padding / 2),
                _ => (0, padding),
            };

            // Add cell content with padding.
            spans.push(Span::raw(" ".to_owned()));
            if pad_left > 0 {
                spans.push(Span::raw(" ".repeat(pad_left)));
            }

            for s in cell {
                let mut style = s.style;
                if is_header {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if stripe {
                    style = style.bg(TABLE_STRIPE_BG);
                }
                spans.push(Span::styled(s.content.clone(), style));
            }

            if pad_right > 0 {
                spans.push(Span::raw(" ".repeat(pad_right)));
            }
            spans.push(Span::raw(" ".to_owned()));

            // Column separator.
            if i < row.len() - 1 {
                spans.push(Span::styled("│".to_owned(), Style::default().fg(DIM)));
            }
        }

        Line::from(spans)
    }

    /// Flush any remaining content and return the finished lines.
    fn finish(mut self) -> Vec<Line<'static>> {
        // Flush any remaining spans.
        if !self.current_spans.is_empty() {
            self.finish_line();
        }

        // Trim trailing empty lines (pulldown-cmark adds trailing paragraph breaks).
        while self.lines.last().is_some_and(|l| l.spans.is_empty()) {
            self.lines.pop();
        }

        self.lines
    }
}

// ── Code fence rendering ──────────────────────────────────────────────────────

/// Syntax-highlight `code` for `lang` using syntect and return one
/// [`Line`] per source line.
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
            let content = line.trim_end_matches('\n').to_owned();
            // Set Line::style to reset bg — prevents CODE_BG from bleeding
            // into trailing buffer cells that ratatui doesn't overwrite.
            result.push(
                Line::from(vec![Span::styled(
                    content,
                    Style::default().fg(CODE_FG).bg(CODE_BG),
                )])
                .style(Style::default()),
            );
            continue;
        }

        let spans: Vec<Span<'static>> = ranges
            .iter()
            .map(|(style, text)| {
                let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
                let content = text.trim_end_matches('\n').to_owned();
                Span::styled(content, Style::default().fg(fg).bg(CODE_BG))
            })
            .collect();

        // Line::style with default bg ensures trailing cells are cleared,
        // preventing CODE_BG from bleeding past the code content.
        result.push(Line::from(spans).style(Style::default()));
    }

    result
}

// ── syntect line-with-endings iterator ────────────────────────────────────────

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

    // ── Helper ────────────────────────────────────────────────────────────

    /// Collect all text content from lines into a single string for assertions.
    fn all_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Find a span whose content contains the given substring.
    fn find_span<'a>(lines: &'a [Line<'_>], needle: &str) -> Option<&'a Span<'a>> {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains(needle))
    }

    // ── Plain text ────────────────────────────────────────────────────────

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
        let span = find_span(&lines, "simple text").unwrap();
        assert_eq!(span.style.fg, Some(Color::Rgb(208, 208, 208)));
    }

    // ── Bold ─────────────────────────────────────────────────────────────

    #[test]
    fn bold_text_has_bold_modifier() {
        let lines = markdown_to_lines("**bold**");
        let bold_span = find_span(&lines, "bold").expect("bold span");
        assert!(
            bold_span.style.add_modifier.contains(Modifier::BOLD),
            "expected BOLD modifier on bold span"
        );
    }

    #[test]
    fn double_underscore_bold_has_bold_modifier() {
        let lines = markdown_to_lines("__bold__");
        let bold_span = find_span(&lines, "bold").expect("bold span");
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    // ── Italic ───────────────────────────────────────────────────────────

    #[test]
    fn italic_text_has_italic_modifier() {
        let lines = markdown_to_lines("*italic*");
        let italic_span = find_span(&lines, "italic").expect("italic span");
        assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn underscore_italic_has_italic_modifier() {
        let lines = markdown_to_lines("_italic_");
        let italic_span = find_span(&lines, "italic").expect("italic span");
        assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    // ── Inline code ──────────────────────────────────────────────────────

    #[test]
    fn inline_code_has_gray_background() {
        let lines = markdown_to_lines("`code`");
        let code_span = find_span(&lines, "code").expect("code span");
        assert_eq!(code_span.style.bg, Some(CODE_BG));
        assert_eq!(code_span.style.fg, Some(CODE_FG));
    }

    // ── Code fences ──────────────────────────────────────────────────────

    #[test]
    fn code_fence_produces_highlighted_lines() {
        let md = "```rust\nlet x = 42;\n```";
        let lines = markdown_to_lines(md);
        assert!(
            !lines.is_empty(),
            "code fence should produce at least one line"
        );
        // Skip the first line (language label) — only code lines have CODE_BG.
        for line in lines.iter().skip(1) {
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
        assert_eq!(
            lines.len(),
            3,
            "each code line should map to one ratatui Line"
        );
    }

    #[test]
    fn code_fence_with_lang_has_label_line() {
        let md = "```rust\nlet x = 42;\n```";
        let lines = markdown_to_lines(md);
        // First line should be the language label.
        let first_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            first_text.contains("rust"),
            "label line should contain the language name: {first_text}"
        );
        assert!(
            first_text.contains("─"),
            "label line should contain dash chars: {first_text}"
        );
        // The language name span should use MUTED colour.
        let lang_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("rust"))
            .expect("should have a span containing 'rust'");
        assert_eq!(lang_span.style.fg, Some(MUTED));
    }

    #[test]
    fn code_fence_without_lang_has_no_label() {
        let md = "```\nhello world\n```";
        let lines = markdown_to_lines(md);
        // No label line — every line should have CODE_BG spans (code content only).
        for line in &lines {
            let has_dash_label = line
                .spans
                .iter()
                .any(|s| s.content.contains("─[") || s.content.contains("─[ "));
            assert!(
                !has_dash_label,
                "bare code fence should not have a language label"
            );
        }
    }

    // ── Headers ──────────────────────────────────────────────────────────

    #[test]
    fn headers_are_bold_with_accent_color() {
        let lines = markdown_to_lines("# Hello");
        assert_eq!(lines.len(), 1);
        let span = find_span(&lines, "Hello").unwrap();
        assert_eq!(
            span.style.fg,
            Some(ACCENT),
            "header should use accent colour"
        );
        assert!(
            span.style.add_modifier.contains(Modifier::BOLD),
            "header should be bold"
        );
    }

    #[test]
    fn h2_header_is_bold_accent() {
        let lines = markdown_to_lines("## Section");
        let span = find_span(&lines, "Section").unwrap();
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(span.style.fg, Some(ACCENT));
    }

    // ── List items ───────────────────────────────────────────────────────

    #[test]
    fn list_items_preserved() {
        let lines = markdown_to_lines("- first item");
        assert!(!lines.is_empty());
        let text = all_text(&lines);
        assert!(text.contains("•"), "should contain bullet: {text}");
        assert!(text.contains("first item"));
    }

    #[test]
    fn asterisk_list_item_uses_bullet() {
        let lines = markdown_to_lines("* second item");
        let text = all_text(&lines);
        assert!(text.contains("•"), "should contain bullet: {text}");
    }

    #[test]
    fn ordered_list_item_preserves_number() {
        let lines = markdown_to_lines("1. first");
        let text = all_text(&lines);
        assert!(text.contains("1."), "should contain number: {text}");
    }

    // ── Mixed formatting ─────────────────────────────────────────────────

    #[test]
    fn mixed_formatting_in_single_line() {
        let lines = markdown_to_lines("normal **bold** and `code`");
        let bold_span = find_span(&lines, "bold").expect("bold span");
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));

        let code_span = find_span(&lines, "code").expect("code span");
        assert_eq!(code_span.style.bg, Some(CODE_BG));
    }

    #[test]
    fn bold_inside_header_retains_accent_color() {
        let lines = markdown_to_lines("# **bold heading**");
        let bold_span = find_span(&lines, "bold heading").expect("bold span in header");
        assert_eq!(bold_span.style.fg, Some(ACCENT));
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    // ── Multi-line document ──────────────────────────────────────────────

    #[test]
    fn multi_line_document_produces_correct_line_count() {
        let md = "# Title\n\nsome text\n\n- item one\n- item two";
        let lines = markdown_to_lines(md);
        // title, blank (paragraph break), text, blank, item1, item2
        assert!(
            lines.len() >= 4,
            "should produce at least 4 lines, got {}",
            lines.len()
        );
    }

    #[test]
    fn unclosed_code_fence_flushes_as_highlighted() {
        let md = "```rust\nlet x = 1;";
        let lines = markdown_to_lines(md);
        assert!(
            !lines.is_empty(),
            "unclosed fence should still produce lines"
        );
    }

    // ── Blank lines ──────────────────────────────────────────────────────

    #[test]
    fn blank_line_produces_empty_line() {
        let lines = markdown_to_lines("");
        assert_eq!(lines.len(), 0);
    }

    // ── NEW: Blockquotes ─────────────────────────────────────────────────

    #[test]
    fn blockquote_has_prefix() {
        let lines = markdown_to_lines("> quoted text");
        let text = all_text(&lines);
        assert!(
            text.contains("│"),
            "blockquote should have │ prefix: {text}"
        );
        assert!(text.contains("quoted text"));
    }

    #[test]
    fn nested_blockquote() {
        let lines = markdown_to_lines("> > nested");
        let text = all_text(&lines);
        // Depth 1 uses │, depth 2 uses ┃.
        assert!(text.contains("│"), "depth-1 prefix should use │: {text}");
        assert!(text.contains("┃"), "depth-2 prefix should use ┃: {text}");
    }

    #[test]
    fn blockquote_depth1_uses_thin_bar() {
        let lines = markdown_to_lines("> hello");
        let prefix_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains('│'))
            .expect("depth-1 should have │ prefix");
        assert_eq!(prefix_span.style.fg, Some(DIM));
    }

    #[test]
    fn blockquote_depth2_uses_thick_bar() {
        let lines = markdown_to_lines("> > hello");
        let prefix_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains('┃'))
            .expect("depth-2 should have ┃ prefix");
        assert_eq!(prefix_span.style.fg, Some(BLOCKQUOTE_DEPTH2));
    }

    #[test]
    fn blockquote_depth3_uses_double_bar() {
        let lines = markdown_to_lines("> > > hello");
        let prefix_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains('║'))
            .expect("depth-3 should have ║ prefix");
        assert_eq!(prefix_span.style.fg, Some(BLOCKQUOTE_DEPTH3));
    }

    // ── NEW: Tables ──────────────────────────────────────────────────────

    #[test]
    fn table_renders_with_columns() {
        let md = "| Name | Age |\n|------|-----|\n| Alice | 30 |\n| Bob | 25 |";
        let lines = markdown_to_lines(md);
        let text = all_text(&lines);
        assert!(text.contains("Alice"), "table should contain Alice: {text}");
        assert!(text.contains("Bob"), "table should contain Bob: {text}");
        assert!(
            text.contains("│"),
            "table should have column separator: {text}"
        );
        // Should have separator line.
        assert!(text.contains("─"), "table should have separator: {text}");
    }

    #[test]
    fn table_header_is_bold() {
        let md = "| Name | Age |\n|------|-----|\n| Alice | 30 |";
        let lines = markdown_to_lines(md);
        // First line should be the header row — find any span with bold.
        assert!(!lines.is_empty(), "table should produce lines");
        let header_line = &lines[0];
        let has_bold = header_line
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD) && !s.content.trim().is_empty());
        assert!(
            has_bold,
            "table header row should have bold spans: {:?}",
            header_line
        );
    }

    #[test]
    fn table_odd_data_rows_have_stripe_background() {
        // pulldown-cmark 0.13 emits header cells inside TableHead without a
        // TableRow wrapper, so header cells are not captured into table_rows.
        // The first TableRow (data row "A|B") becomes table_rows[0] and is
        // rendered as the visual header.  Remaining rows are data:
        //   table_rows[1] (C|D) → data index 0 → no stripe
        //   table_rows[2] (E|F) → data index 1 → stripe
        let md = "| H1 | H2 |\n|---|---|\n| A | B |\n| C | D |\n| E | F |";
        let lines = markdown_to_lines(md);
        // line 0 = header, line 1 = separator, line 2 = data row 0, line 3 = data row 1
        assert!(
            lines.len() >= 4,
            "should have header + sep + 2 data rows, got {}",
            lines.len()
        );
        let striped_line = &lines[3]; // data index 1 (odd) → striped
        let has_stripe = striped_line
            .spans
            .iter()
            .any(|s| s.style.bg == Some(TABLE_STRIPE_BG));
        assert!(
            has_stripe,
            "odd data row should have TABLE_STRIPE_BG: {:?}",
            striped_line
        );
        // Verify the even data row does NOT have the stripe.
        let even_line = &lines[2]; // data index 0 (even) → no stripe
        let has_no_stripe = !even_line
            .spans
            .iter()
            .any(|s| s.style.bg == Some(TABLE_STRIPE_BG));
        assert!(
            has_no_stripe,
            "even data row should NOT have TABLE_STRIPE_BG: {:?}",
            even_line
        );
    }

    #[test]
    fn table_header_row_has_no_stripe_background() {
        let md = "| H1 | H2 |\n|---|---|\n| A | B |\n| C | D |";
        let lines = markdown_to_lines(md);
        assert!(!lines.is_empty());
        let header_line = &lines[0];
        let has_stripe = header_line
            .spans
            .iter()
            .any(|s| s.style.bg == Some(TABLE_STRIPE_BG));
        assert!(
            !has_stripe,
            "header row should NOT have TABLE_STRIPE_BG: {:?}",
            header_line
        );
    }

    // ── NEW: Links ───────────────────────────────────────────────────────

    #[test]
    fn link_shows_url() {
        let lines = markdown_to_lines("[click here](https://example.com)");
        let text = all_text(&lines);
        assert!(text.contains("click here"), "link text: {text}");
        assert!(
            text.contains("https://example.com"),
            "link URL should be visible: {text}"
        );
    }

    // ── NEW: Nested formatting ───────────────────────────────────────────

    #[test]
    fn bold_italic_nesting() {
        let lines = markdown_to_lines("***bold italic***");
        let span = find_span(&lines, "bold italic").expect("bold italic span");
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        assert!(span.style.add_modifier.contains(Modifier::ITALIC));
    }

    // ── NEW: Horizontal rule ─────────────────────────────────────────────

    #[test]
    fn horizontal_rule_renders() {
        let lines = markdown_to_lines("---");
        let text = all_text(&lines);
        assert!(
            text.contains("─"),
            "horizontal rule should contain ─: {text}"
        );
    }

    // ── NEW: Strikethrough ───────────────────────────────────────────────

    #[test]
    fn strikethrough_has_crossed_out_modifier() {
        let lines = markdown_to_lines("~~deleted~~");
        let span = find_span(&lines, "deleted").expect("strikethrough span");
        assert!(span.style.add_modifier.contains(Modifier::CROSSED_OUT));
    }
}
