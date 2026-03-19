//! Render helpers for the Genesis TUI.
//!
//! Currently contains a markdown-to-ratatui adapter that converts markdown
//! text into styled [`ratatui::text::Line`]s suitable for direct rendering
//! in the inline viewport.

pub mod diff;
pub mod markdown;
pub mod shadow;
pub use diff::{diff_to_lines, is_unified_diff};
pub use markdown::markdown_to_lines;
pub use shadow::render_shadow;
