//! Render helpers for the Genesis TUI.
//!
//! Currently contains a markdown-to-ratatui adapter that converts markdown
//! text into styled [`ratatui::text::Line`]s suitable for direct rendering
//! in the inline viewport.

pub mod markdown;
pub use markdown::markdown_to_lines;
