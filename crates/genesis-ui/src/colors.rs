//! Eve palette and UI palette — truecolor hex values for terminal rendering.
//!
//! Eve palette is used only for banner art. UI palette is used for everything else.
//! When colors are disabled, all helpers return the input string unmodified.

use owo_colors::Style;

// ── Eve Palette (banner art only) ──────────────────────────────────────

pub const EVE_LAVENDER: (u8, u8, u8) = (180, 167, 214); // #B4A7D6
pub const EVE_PURPLE: (u8, u8, u8) = (123, 104, 174);   // #7B68AE
pub const EVE_LILAC: (u8, u8, u8) = (213, 204, 230);    // #D5CCE6
pub const EVE_DARK: (u8, u8, u8) = (45, 27, 78);        // #2D1B4E
pub const EVE_AMBER: (u8, u8, u8) = (212, 165, 116);    // #D4A574

// ── UI Palette (everything else) ───────────────────────────────────────

pub const UI_DIM: (u8, u8, u8) = (108, 108, 108);       // #6C6C6C
pub const UI_TEXT: (u8, u8, u8) = (208, 208, 208);       // #D0D0D0
pub const UI_MUTED: (u8, u8, u8) = (138, 138, 138);     // #8A8A8A
pub const UI_ACCENT: (u8, u8, u8) = (180, 167, 214);    // #B4A7D6 (= EVE_LAVENDER)
pub const UI_SUCCESS: (u8, u8, u8) = (135, 175, 95);    // #87AF5F
pub const UI_ERROR: (u8, u8, u8) = (215, 95, 95);       // #D75F5F
pub const UI_WARNING: (u8, u8, u8) = (215, 175, 95);    // #D7AF5F

// ── Style constructors ─────────────────────────────────────────────────

/// Build an owo-colors `Style` from an RGB tuple.
fn rgb_style(rgb: (u8, u8, u8)) -> Style {
    Style::new().truecolor(rgb.0, rgb.1, rgb.2)
}

/// Pre-built styles for common UI elements.
pub struct Styles {
    pub dim: Style,
    pub text: Style,
    pub muted: Style,
    pub accent: Style,
    pub success: Style,
    pub error: Style,
    pub warning: Style,
    pub accent_bold: Style,
}

impl Styles {
    pub fn new() -> Self {
        Self {
            dim: rgb_style(UI_DIM),
            text: rgb_style(UI_TEXT),
            muted: rgb_style(UI_MUTED),
            accent: rgb_style(UI_ACCENT),
            success: rgb_style(UI_SUCCESS),
            error: rgb_style(UI_ERROR),
            warning: rgb_style(UI_WARNING),
            accent_bold: rgb_style(UI_ACCENT).bold(),
        }
    }

    /// A no-op set of styles that produces uncolored output.
    pub fn plain() -> Self {
        Self {
            dim: Style::new(),
            text: Style::new(),
            muted: Style::new(),
            accent: Style::new(),
            success: Style::new(),
            error: Style::new(),
            warning: Style::new(),
            accent_bold: Style::new(),
        }
    }
}

impl Default for Styles {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use owo_colors::OwoColorize;

    #[test]
    fn styles_new_does_not_panic() {
        let _styles = Styles::new();
    }

    #[test]
    fn styles_plain_produces_plain_styles() {
        let styles = Styles::plain();
        let styled = "hello".style(styles.dim);
        assert_eq!(styled.to_string(), "hello");
    }

    #[test]
    fn accent_matches_eve_lavender() {
        assert_eq!(UI_ACCENT, EVE_LAVENDER);
    }
}
