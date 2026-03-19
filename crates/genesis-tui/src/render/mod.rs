//! Render helpers for the Genesis TUI.
//!
//! Currently contains a markdown-to-ratatui adapter that converts markdown
//! text into styled [`ratatui::text::Line`]s suitable for direct rendering
//! in the inline viewport.

pub mod borders;
pub mod diff;
pub mod markdown;
pub mod renderable;
pub mod shadow;
pub use borders::draw_box;
pub use diff::{diff_to_lines, diff_to_lines_themed, is_unified_diff, DiffColors};
pub use markdown::markdown_to_lines;
pub use renderable::Renderable;
pub use shadow::render_shadow;
