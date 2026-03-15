//! Genesis UI — terminal rendering primitives for the Genesis CLI.
//!
//! This crate provides colors, terminal detection, and formatted output
//! for the CLI. It does NOT depend on any other genesis crate.

pub mod banner;
pub mod colors;
pub mod markdown;
pub mod status_bar;
pub mod terminal;
pub mod tool_display;

use colors::Styles;
use owo_colors::OwoColorize;
use terminal::ColorMode;

/// Central UI context created once at CLI startup.
pub struct UiContext {
    pub colors_enabled: bool,
    styles: Styles,
}

impl UiContext {
    pub fn new(color_mode: ColorMode) -> Self {
        let colors_enabled = color_mode.is_enabled();
        let styles = if colors_enabled {
            Styles::new()
        } else {
            Styles::plain()
        };
        Self { colors_enabled, styles }
    }

    pub fn styles(&self) -> &Styles {
        &self.styles
    }

    pub fn eve_prompt(&self) -> String {
        if self.colors_enabled {
            "eve> ".style(self.styles.accent).to_string()
        } else {
            "eve> ".to_string()
        }
    }

    pub fn you_prompt(&self) -> String {
        if self.colors_enabled {
            "you> ".style(self.styles.dim).to_string()
        } else {
            "you> ".to_string()
        }
    }

    pub fn format_metadata(&self, text: &str) -> String {
        if self.colors_enabled {
            text.style(self.styles.dim).to_string()
        } else {
            text.to_string()
        }
    }

    pub fn format_error(&self, text: &str) -> String {
        if self.colors_enabled {
            text.style(self.styles.error).to_string()
        } else {
            text.to_string()
        }
    }

    pub fn format_success(&self, text: &str) -> String {
        if self.colors_enabled {
            text.style(self.styles.success).to_string()
        } else {
            text.to_string()
        }
    }

    pub fn format_warning(&self, text: &str) -> String {
        if self.colors_enabled {
            text.style(self.styles.warning).to_string()
        } else {
            text.to_string()
        }
    }

    pub fn format_accent(&self, text: &str) -> String {
        if self.colors_enabled {
            text.style(self.styles.accent).to_string()
        } else {
            text.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_context_with_colors_enabled() {
        let ctx = UiContext::new(ColorMode::Always);
        assert!(ctx.colors_enabled);
        assert!(ctx.eve_prompt().contains('\x1b'));
    }

    #[test]
    fn ui_context_with_colors_disabled() {
        let ctx = UiContext::new(ColorMode::Never);
        assert!(!ctx.colors_enabled);
        assert_eq!(ctx.eve_prompt(), "eve> ");
        assert_eq!(ctx.format_error("oops"), "oops");
    }
}
