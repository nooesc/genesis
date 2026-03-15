//! Streaming markdown renderer with syntax-highlighted code blocks.
//!
//! During streaming, text passes through line by line. Code fences
//! (` ``` ` ... ` ``` `) are buffered until the closing fence, then rendered
//! with syntax highlighting via syntect. Tables are buffered until a blank
//! line, then rendered via termimad.
//!
//! On `finish()`, any incomplete buffered content is flushed as plain text.

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;
use termimad::MadSkin;

use crate::colors::*;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

fn get_syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn get_theme_set() -> &'static ThemeSet {
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

/// Renders complete markdown blocks (non-streaming).
pub struct MarkdownRenderer {
    skin: MadSkin,
    pub(crate) colors_enabled: bool,
}

impl MarkdownRenderer {
    /// Create a new renderer, optionally with Eve palette colors.
    pub fn new(colors_enabled: bool) -> Self {
        let mut skin = MadSkin::default_dark();
        if colors_enabled {
            use termimad::crossterm::style::Color as TColor;
            skin.headers[0].set_fg(TColor::Rgb {
                r: UI_ACCENT.0,
                g: UI_ACCENT.1,
                b: UI_ACCENT.2,
            });
            skin.bold.set_fg(TColor::Rgb {
                r: UI_TEXT.0,
                g: UI_TEXT.1,
                b: UI_TEXT.2,
            });
            skin.italic.set_fg(TColor::Rgb {
                r: UI_DIM.0,
                g: UI_DIM.1,
                b: UI_DIM.2,
            });
        }
        Self {
            skin,
            colors_enabled,
        }
    }

    /// Render a complete markdown block (non-code) using termimad.
    pub fn render_block(&self, markdown: &str) -> String {
        self.skin.term_text(markdown).to_string()
    }

    /// Render a code block with syntax highlighting via syntect.
    pub fn render_code_block(&self, code: &str, language: &str) -> String {
        if !self.colors_enabled {
            // Plain text fallback: just indent the code
            let mut result = String::new();
            if !language.is_empty() {
                result.push_str(&format!("  --- {language} ---\n"));
            }
            for line in code.lines() {
                result.push_str(&format!("  | {line}\n"));
            }
            result.push_str("  ---\n");
            return result;
        }

        let syntax_set = get_syntax_set();
        let theme_set = get_theme_set();
        let theme = &theme_set.themes["base16-ocean.dark"];
        let syntax = syntax_set
            .find_syntax_by_token(language)
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text());

        let mut highlighter = HighlightLines::new(syntax, theme);
        let mut result = String::new();

        // Header line
        let dim = format!(
            "\x1b[38;2;{};{};{}m",
            UI_DIM.0, UI_DIM.1, UI_DIM.2
        );
        let reset = "\x1b[0m";
        if !language.is_empty() {
            result.push_str(&format!("{dim}  \u{256d}\u{2500} {language} \u{2500}{reset}\n"));
        } else {
            result.push_str(&format!("{dim}  \u{256d}\u{2500}\u{2500}{reset}\n"));
        }

        // Highlighted code lines
        for line in code.lines() {
            let ranges = highlighter
                .highlight_line(line, syntax_set)
                .unwrap_or_default();
            let escaped = as_24_bit_terminal_escaped(&ranges, false);
            result.push_str(&format!("{dim}  \u{2502}{reset} {escaped}\x1b[0m\n"));
        }

        // Footer line
        result.push_str(&format!("{dim}  \u{2570}\u{2500}\u{2500}{reset}\n"));
        result
    }
}

// ── Streaming state machine ──────────────────────────────────────────

/// State of the streaming markdown parser.
#[derive(Debug, PartialEq)]
enum StreamState {
    /// Normal text, passed through line by line.
    Normal,
    /// Inside a code fence; buffering lines until the closing fence.
    InCodeFence,
    /// Saw a potential table header (`|...|`); waiting for a separator row
    /// (`|---|---|`) on the next line to confirm it is actually a table.
    PendingTable,
    /// Inside a confirmed table; buffering lines until a blank line.
    InTable,
}

/// Check if a line is a markdown table separator row (e.g. `|---|---|`).
///
/// Requires the line to start and end with `|` and every cell between pipes
/// to consist of only dashes, colons, and spaces (for alignment markers like
/// `:---:`, `---:`, `:---`).
fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return false;
    }
    // Strip the leading and trailing `|`
    let inner = &trimmed[1..trimmed.len() - 1];
    if inner.is_empty() {
        return false;
    }
    // Every cell between `|` must match the separator pattern
    inner.split('|').all(|cell| {
        let cell = cell.trim();
        !cell.is_empty() && cell.chars().all(|c| c == '-' || c == ':' || c == ' ')
    })
}

/// A streaming markdown processor.
///
/// Feed it chunks of text via [`push`], get back rendered output. Code
/// fences are buffered and rendered with syntax highlighting when the
/// closing fence arrives. Everything else passes through as-is.
pub struct StreamMarkdown {
    state: StreamState,
    /// Partial line buffer — text received that doesn't end with '\n'.
    line_buf: String,
    /// Accumulated code lines (only used in InCodeFence state).
    code_buf: String,
    /// Language tag from the opening ``` fence.
    code_lang: String,
    /// Accumulated table lines (only used in InTable state).
    table_buf: String,
    renderer: MarkdownRenderer,
}

impl StreamMarkdown {
    pub fn new(colors_enabled: bool) -> Self {
        Self {
            state: StreamState::Normal,
            line_buf: String::new(),
            code_buf: String::new(),
            code_lang: String::new(),
            table_buf: String::new(),
            renderer: MarkdownRenderer::new(colors_enabled),
        }
    }

    /// Feed a chunk of streamed text, returning any rendered output.
    ///
    /// May return an empty string if the chunk is being buffered (e.g.,
    /// inside a code fence that hasn't closed yet).
    pub fn push(&mut self, chunk: &str) -> String {
        if !self.renderer.colors_enabled {
            return chunk.to_string();
        }

        self.line_buf.push_str(chunk);
        let mut output = String::new();

        // Process all complete lines in the buffer
        loop {
            let Some(newline_pos) = self.line_buf.find('\n') else {
                break;
            };
            let line = self.line_buf[..newline_pos].to_string();
            self.line_buf = self.line_buf[newline_pos + 1..].to_string();

            match self.state {
                StreamState::Normal => {
                    if line.starts_with("```") {
                        // Opening code fence
                        self.code_lang = line
                            .trim_start_matches('`')
                            .trim()
                            .to_string();
                        self.code_buf.clear();
                        self.state = StreamState::InCodeFence;
                    } else if line.contains('|')
                        && line.trim().starts_with('|')
                        && line.trim().ends_with('|')
                    {
                        // Potential table header — wait for separator row
                        self.table_buf.clear();
                        self.table_buf.push_str(&line);
                        self.table_buf.push('\n');
                        self.state = StreamState::PendingTable;
                    } else {
                        // Regular text — pass through
                        output.push_str(&line);
                        output.push('\n');
                    }
                }
                StreamState::PendingTable => {
                    if is_table_separator(&line) {
                        // Confirmed table — the pending header + this separator
                        self.table_buf.push_str(&line);
                        self.table_buf.push('\n');
                        self.state = StreamState::InTable;
                    } else {
                        // Not a table — flush the pending header as normal text
                        output.push_str(&self.table_buf);
                        self.table_buf.clear();
                        self.state = StreamState::Normal;
                        // Re-process the current line as Normal text
                        output.push_str(&line);
                        output.push('\n');
                    }
                }
                StreamState::InCodeFence => {
                    if line.starts_with("```") {
                        // Closing code fence — render the buffered code
                        output.push_str(&self.renderer.render_code_block(
                            &self.code_buf,
                            &self.code_lang,
                        ));
                        self.code_buf.clear();
                        self.code_lang.clear();
                        self.state = StreamState::Normal;
                    } else {
                        // Accumulate code line
                        self.code_buf.push_str(&line);
                        self.code_buf.push('\n');
                    }
                }
                StreamState::InTable => {
                    if line.trim().is_empty() {
                        // End of table — render via termimad
                        output.push_str(
                            &self.renderer.render_block(&self.table_buf),
                        );
                        self.table_buf.clear();
                        self.state = StreamState::Normal;
                    } else {
                        self.table_buf.push_str(&line);
                        self.table_buf.push('\n');
                    }
                }
            }
        }

        output
    }

    /// Flush any remaining buffered content as plain text.
    ///
    /// Call this when the stream ends. Incomplete code fences or tables
    /// are emitted as unformatted text rather than being silently dropped.
    pub fn finish(&mut self) -> String {
        let mut output = String::new();

        // Flush any partial line
        if !self.line_buf.is_empty() {
            match self.state {
                StreamState::Normal => {
                    output.push_str(&self.line_buf);
                }
                StreamState::InCodeFence => {
                    self.code_buf.push_str(&self.line_buf);
                }
                StreamState::PendingTable | StreamState::InTable => {
                    self.table_buf.push_str(&self.line_buf);
                }
            }
            self.line_buf.clear();
        }

        // Flush buffered content from incomplete blocks
        match self.state {
            StreamState::Normal => {}
            StreamState::InCodeFence => {
                // Incomplete code fence — emit as plain text
                if !self.code_lang.is_empty() {
                    output.push_str(&format!("```{}\n", self.code_lang));
                } else {
                    output.push_str("```\n");
                }
                output.push_str(&self.code_buf);
            }
            StreamState::PendingTable => {
                // Never got a separator row — emit pending header as plain text
                output.push_str(&self.table_buf);
            }
            StreamState::InTable => {
                // Incomplete table — emit as plain text
                output.push_str(&self.table_buf);
            }
        }

        // Reset state
        self.state = StreamState::Normal;
        self.code_buf.clear();
        self.code_lang.clear();
        self.table_buf.clear();

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_code_block_produces_ansi_with_language_label() {
        let renderer = MarkdownRenderer::new(true);
        let output = renderer.render_code_block("let x = 42;", "rust");
        // Should contain ANSI escape codes
        assert!(output.contains('\x1b'));
        // Should contain the language label
        assert!(output.contains("rust"));
        // Should contain the box-drawing characters
        assert!(output.contains('\u{256d}')); // top-left corner
        assert!(output.contains('\u{2570}')); // bottom-left corner
    }

    #[test]
    fn render_code_block_plain_text_fallback() {
        let renderer = MarkdownRenderer::new(false);
        let output = renderer.render_code_block("let x = 42;", "rust");
        assert!(!output.contains('\x1b'));
        assert!(output.contains("rust"));
        assert!(output.contains("let x = 42;"));
    }

    #[test]
    fn render_code_block_no_language() {
        let renderer = MarkdownRenderer::new(true);
        let output = renderer.render_code_block("echo hello", "");
        assert!(output.contains('\x1b'));
        // No language label but still has border
        assert!(output.contains('\u{256d}'));
    }

    #[test]
    fn stream_normal_text_passes_through() {
        let mut stream = StreamMarkdown::new(true);
        let output = stream.push("hello world\n");
        assert_eq!(output, "hello world\n");
    }

    #[test]
    fn stream_normal_text_buffers_partial_lines() {
        let mut stream = StreamMarkdown::new(true);
        // No newline — should buffer
        let output = stream.push("hello");
        assert_eq!(output, "");
        // Newline completes the line
        let output = stream.push(" world\n");
        assert_eq!(output, "hello world\n");
    }

    #[test]
    fn stream_code_fence_buffers_and_renders_on_close() {
        let mut stream = StreamMarkdown::new(true);
        // Opening fence — no output
        let output = stream.push("```rust\n");
        assert_eq!(output, "");
        // Code lines — no output (buffering)
        let output = stream.push("let x = 42;\n");
        assert_eq!(output, "");
        // Closing fence — renders the code block
        let output = stream.push("```\n");
        assert!(!output.is_empty());
        assert!(output.contains('\x1b')); // has ANSI coloring
        assert!(output.contains("rust")); // has language label
    }

    #[test]
    fn stream_code_fence_with_colors_disabled() {
        let mut stream = StreamMarkdown::new(false);
        // When colors are disabled, everything passes through as-is
        let output = stream.push("```rust\nlet x = 42;\n```\n");
        assert_eq!(output, "```rust\nlet x = 42;\n```\n");
    }

    #[test]
    fn stream_code_fence_split_across_chunks() {
        let mut stream = StreamMarkdown::new(true);
        let o1 = stream.push("```py");
        assert_eq!(o1, "");
        let o2 = stream.push("thon\nprint('hello')\n");
        assert_eq!(o2, "");
        let o3 = stream.push("```\n");
        assert!(!o3.is_empty());
        assert!(o3.contains("python"));
    }

    #[test]
    fn finish_flushes_incomplete_code_fence() {
        let mut stream = StreamMarkdown::new(true);
        stream.push("```rust\nlet x = 42;\n");
        // No closing fence — finish should flush as plain text
        let remaining = stream.finish();
        assert!(remaining.contains("```rust"));
        assert!(remaining.contains("let x = 42;"));
    }

    #[test]
    fn finish_flushes_partial_line() {
        let mut stream = StreamMarkdown::new(true);
        stream.push("trailing text");
        let remaining = stream.finish();
        assert_eq!(remaining, "trailing text");
    }

    #[test]
    fn finish_resets_state() {
        let mut stream = StreamMarkdown::new(true);
        stream.push("```rust\ncode\n");
        let _ = stream.finish();
        // After finish, state should be Normal again
        let output = stream.push("hello\n");
        assert_eq!(output, "hello\n");
    }

    #[test]
    fn stream_table_buffers_until_blank_line() {
        let mut stream = StreamMarkdown::new(true);
        let o1 = stream.push("| a | b |\n");
        assert_eq!(o1, "");
        let o2 = stream.push("|---|---|\n");
        assert_eq!(o2, "");
        let o3 = stream.push("| 1 | 2 |\n");
        assert_eq!(o3, "");
        // Blank line ends the table
        let o4 = stream.push("\n");
        // termimad should produce some output
        assert!(!o4.is_empty());
    }

    #[test]
    fn pipe_line_without_separator_is_not_a_table() {
        let mut stream = StreamMarkdown::new(true);
        // A line with pipes but no separator row on the next line
        let o1 = stream.push("| this is just text with pipes |\n");
        assert_eq!(o1, ""); // pending — waiting for separator
        // Next line is regular text, not a separator
        let o2 = stream.push("some normal text\n");
        // Should flush the pending line as normal text, plus the current line
        assert!(o2.contains("| this is just text with pipes |"));
        assert!(o2.contains("some normal text"));
    }

    #[test]
    fn is_table_separator_detects_valid_separators() {
        assert!(super::is_table_separator("|---|---|"));
        assert!(super::is_table_separator("| --- | --- |"));
        assert!(super::is_table_separator("|:---:|---:|"));
        assert!(super::is_table_separator("| :--- | ---: |"));
    }

    #[test]
    fn is_table_separator_rejects_non_separators() {
        assert!(!super::is_table_separator("| hello | world |"));
        assert!(!super::is_table_separator("not a table"));
        assert!(!super::is_table_separator("||")); // empty cells
        assert!(!super::is_table_separator("| abc | 123 |"));
    }

    #[test]
    fn mixed_content_text_and_code() {
        let mut stream = StreamMarkdown::new(true);
        let o1 = stream.push("Some text\n");
        assert_eq!(o1, "Some text\n");
        let o2 = stream.push("```\nhello\n```\n");
        assert!(o2.contains('\x1b')); // rendered code block
        let o3 = stream.push("More text\n");
        assert_eq!(o3, "More text\n");
    }
}
