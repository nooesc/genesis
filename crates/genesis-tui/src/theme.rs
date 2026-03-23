//! Theme system — semantic color palette with built-in themes.
//!
//! Phase 1: trait definition + 4 built-in themes (Eve, Catppuccin, Dracula, Tokyo Night).

use ratatui::style::Color;

/// Semantic color palette for the TUI.
pub trait Theme: Send + Sync {
    fn name(&self) -> &str;

    // ── Base ──
    fn primary(&self) -> Color;
    fn accent(&self) -> Color;
    fn background(&self) -> Color;

    // ── Text ──
    fn text(&self) -> Color;
    fn text_dim(&self) -> Color;
    fn text_muted(&self) -> Color;

    // ── Status ──
    fn success(&self) -> Color;
    fn error(&self) -> Color;
    fn warning(&self) -> Color;

    // ── Borders ──
    fn border(&self) -> Color;
    fn border_active(&self) -> Color;

    // ── Diff ──
    fn diff_add_fg(&self) -> Color;
    fn diff_add_bg(&self) -> Color;
    fn diff_del_fg(&self) -> Color;
    fn diff_del_bg(&self) -> Color;

    // ── Status bar ──
    fn bar_bg(&self) -> Color;
    fn bar_bg_active(&self) -> Color;
}

/// Eve theme — the default Genesis palette (current hardcoded colors).
pub struct EveTheme;

impl Theme for EveTheme {
    fn name(&self) -> &str {
        "eve"
    }
    fn primary(&self) -> Color {
        Color::Rgb(180, 167, 214)
    } // EVE_LAVENDER
    fn accent(&self) -> Color {
        Color::Rgb(123, 104, 174)
    } // EVE_PURPLE
    fn background(&self) -> Color {
        Color::Rgb(20, 20, 25)
    }
    fn text(&self) -> Color {
        Color::Rgb(208, 208, 208)
    }
    fn text_dim(&self) -> Color {
        Color::Rgb(108, 108, 108)
    }
    fn text_muted(&self) -> Color {
        Color::Rgb(138, 138, 138)
    }
    fn success(&self) -> Color {
        Color::Rgb(135, 175, 95)
    }
    fn error(&self) -> Color {
        Color::Rgb(215, 95, 95)
    }
    fn warning(&self) -> Color {
        Color::Rgb(215, 175, 95)
    }
    fn border(&self) -> Color {
        Color::Rgb(88, 88, 88)
    }
    fn border_active(&self) -> Color {
        Color::Rgb(180, 167, 214)
    }
    fn diff_add_fg(&self) -> Color {
        Color::Rgb(135, 175, 95)
    }
    fn diff_add_bg(&self) -> Color {
        Color::Rgb(33, 58, 43)
    }
    fn diff_del_fg(&self) -> Color {
        Color::Rgb(215, 95, 95)
    }
    fn diff_del_bg(&self) -> Color {
        Color::Rgb(74, 34, 29)
    }
    fn bar_bg(&self) -> Color {
        Color::Rgb(30, 28, 36)
    }
    fn bar_bg_active(&self) -> Color {
        Color::Rgb(38, 34, 48)
    }
}

/// Catppuccin Mocha theme.
pub struct CatppuccinTheme;

impl Theme for CatppuccinTheme {
    fn name(&self) -> &str {
        "catppuccin"
    }
    fn primary(&self) -> Color {
        Color::Rgb(137, 180, 250)
    } // Blue
    fn accent(&self) -> Color {
        Color::Rgb(203, 166, 247)
    } // Mauve
    fn background(&self) -> Color {
        Color::Rgb(30, 30, 46)
    } // Base
    fn text(&self) -> Color {
        Color::Rgb(205, 214, 244)
    } // Text
    fn text_dim(&self) -> Color {
        Color::Rgb(127, 132, 156)
    } // Overlay0
    fn text_muted(&self) -> Color {
        Color::Rgb(147, 153, 178)
    }
    fn success(&self) -> Color {
        Color::Rgb(166, 227, 161)
    } // Green
    fn error(&self) -> Color {
        Color::Rgb(243, 139, 168)
    } // Red
    fn warning(&self) -> Color {
        Color::Rgb(249, 226, 175)
    } // Yellow
    fn border(&self) -> Color {
        Color::Rgb(69, 71, 90)
    } // Surface1
    fn border_active(&self) -> Color {
        Color::Rgb(137, 180, 250)
    }
    fn diff_add_fg(&self) -> Color {
        Color::Rgb(166, 227, 161)
    }
    fn diff_add_bg(&self) -> Color {
        Color::Rgb(30, 50, 40)
    }
    fn diff_del_fg(&self) -> Color {
        Color::Rgb(243, 139, 168)
    }
    fn diff_del_bg(&self) -> Color {
        Color::Rgb(60, 30, 40)
    }
    fn bar_bg(&self) -> Color {
        Color::Rgb(24, 24, 37)
    } // Mantle
    fn bar_bg_active(&self) -> Color {
        Color::Rgb(30, 30, 46)
    }
}

/// Dracula theme.
pub struct DraculaTheme;

impl Theme for DraculaTheme {
    fn name(&self) -> &str {
        "dracula"
    }
    fn primary(&self) -> Color {
        Color::Rgb(189, 147, 249)
    } // Purple
    fn accent(&self) -> Color {
        Color::Rgb(255, 121, 198)
    } // Pink
    fn background(&self) -> Color {
        Color::Rgb(40, 42, 54)
    } // Background
    fn text(&self) -> Color {
        Color::Rgb(248, 248, 242)
    } // Foreground
    fn text_dim(&self) -> Color {
        Color::Rgb(98, 114, 164)
    } // Comment
    fn text_muted(&self) -> Color {
        Color::Rgb(130, 140, 180)
    }
    fn success(&self) -> Color {
        Color::Rgb(80, 250, 123)
    } // Green
    fn error(&self) -> Color {
        Color::Rgb(255, 85, 85)
    } // Red
    fn warning(&self) -> Color {
        Color::Rgb(241, 250, 140)
    } // Yellow
    fn border(&self) -> Color {
        Color::Rgb(68, 71, 90)
    } // Current Line
    fn border_active(&self) -> Color {
        Color::Rgb(189, 147, 249)
    }
    fn diff_add_fg(&self) -> Color {
        Color::Rgb(80, 250, 123)
    }
    fn diff_add_bg(&self) -> Color {
        Color::Rgb(30, 55, 35)
    }
    fn diff_del_fg(&self) -> Color {
        Color::Rgb(255, 85, 85)
    }
    fn diff_del_bg(&self) -> Color {
        Color::Rgb(60, 30, 30)
    }
    fn bar_bg(&self) -> Color {
        Color::Rgb(33, 34, 44)
    }
    fn bar_bg_active(&self) -> Color {
        Color::Rgb(40, 42, 54)
    }
}

/// Tokyo Night theme.
pub struct TokyoNightTheme;

impl Theme for TokyoNightTheme {
    fn name(&self) -> &str {
        "tokyonight"
    }
    fn primary(&self) -> Color {
        Color::Rgb(122, 162, 247)
    } // Blue
    fn accent(&self) -> Color {
        Color::Rgb(187, 154, 247)
    } // Purple
    fn background(&self) -> Color {
        Color::Rgb(26, 27, 38)
    } // bg
    fn text(&self) -> Color {
        Color::Rgb(192, 202, 245)
    } // fg
    fn text_dim(&self) -> Color {
        Color::Rgb(86, 95, 137)
    } // comment
    fn text_muted(&self) -> Color {
        Color::Rgb(130, 140, 170)
    }
    fn success(&self) -> Color {
        Color::Rgb(158, 206, 106)
    } // green
    fn error(&self) -> Color {
        Color::Rgb(247, 118, 142)
    } // red
    fn warning(&self) -> Color {
        Color::Rgb(224, 175, 104)
    } // yellow
    fn border(&self) -> Color {
        Color::Rgb(41, 46, 66)
    } // border
    fn border_active(&self) -> Color {
        Color::Rgb(122, 162, 247)
    }
    fn diff_add_fg(&self) -> Color {
        Color::Rgb(158, 206, 106)
    }
    fn diff_add_bg(&self) -> Color {
        Color::Rgb(25, 45, 30)
    }
    fn diff_del_fg(&self) -> Color {
        Color::Rgb(247, 118, 142)
    }
    fn diff_del_bg(&self) -> Color {
        Color::Rgb(55, 25, 35)
    }
    fn bar_bg(&self) -> Color {
        Color::Rgb(22, 22, 30)
    }
    fn bar_bg_active(&self) -> Color {
        Color::Rgb(26, 27, 38)
    }
}

/// NO_COLOR-compliant theme — uses only terminal default colors.
/// Activated when the NO_COLOR environment variable is set.
pub struct NoColorTheme;

impl Theme for NoColorTheme {
    fn name(&self) -> &str {
        "nocolor"
    }
    fn primary(&self) -> Color {
        Color::Reset
    }
    fn accent(&self) -> Color {
        Color::Reset
    }
    fn background(&self) -> Color {
        Color::Reset
    }
    fn text(&self) -> Color {
        Color::Reset
    }
    fn text_dim(&self) -> Color {
        Color::Reset
    }
    fn text_muted(&self) -> Color {
        Color::Reset
    }
    fn success(&self) -> Color {
        Color::Green
    }
    fn error(&self) -> Color {
        Color::Red
    }
    fn warning(&self) -> Color {
        Color::Yellow
    }
    fn border(&self) -> Color {
        Color::Reset
    }
    fn border_active(&self) -> Color {
        Color::Reset
    }
    fn diff_add_fg(&self) -> Color {
        Color::Green
    }
    fn diff_add_bg(&self) -> Color {
        Color::Reset
    }
    fn diff_del_fg(&self) -> Color {
        Color::Red
    }
    fn diff_del_bg(&self) -> Color {
        Color::Reset
    }
    fn bar_bg(&self) -> Color {
        Color::Reset
    }
    fn bar_bg_active(&self) -> Color {
        Color::Reset
    }
}

/// All built-in themes (including auto-activated nocolor).
pub const THEME_NAMES: &[&str] = &["eve", "catppuccin", "dracula", "tokyonight", "nocolor"];

/// Theme names available for user selection (excludes auto-activated nocolor).
pub const USER_THEME_NAMES: &[&str] = &["eve", "catppuccin", "dracula", "tokyonight"];

/// Create a theme by name. Returns Eve theme for unknown names.
pub fn theme_by_name(name: &str) -> Box<dyn Theme> {
    match name {
        "catppuccin" => Box::new(CatppuccinTheme),
        "dracula" => Box::new(DraculaTheme),
        "tokyonight" => Box::new(TokyoNightTheme),
        "nocolor" => Box::new(NoColorTheme),
        _ => Box::new(EveTheme),
    }
}

/// Check if NO_COLOR is set and return the appropriate theme.
/// Falls back to the configured theme name if NO_COLOR is not set.
pub fn resolve_theme(configured_name: &str) -> Box<dyn Theme> {
    if genesis_config::env::is_present(genesis_config::env::NO_COLOR) {
        return Box::new(NoColorTheme);
    }
    theme_by_name(configured_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eve_theme_name() {
        assert_eq!(EveTheme.name(), "eve");
    }

    #[test]
    fn all_themes_have_names() {
        for name in THEME_NAMES {
            let theme = theme_by_name(name);
            assert_eq!(theme.name(), *name);
        }
    }

    #[test]
    fn unknown_theme_falls_back_to_eve() {
        let theme = theme_by_name("nonexistent");
        assert_eq!(theme.name(), "eve");
    }

    #[test]
    fn theme_colors_are_rgb() {
        let theme = EveTheme;
        assert!(matches!(theme.primary(), Color::Rgb(_, _, _)));
        assert!(matches!(theme.accent(), Color::Rgb(_, _, _)));
        assert!(matches!(theme.text(), Color::Rgb(_, _, _)));
        assert!(matches!(theme.success(), Color::Rgb(_, _, _)));
        assert!(matches!(theme.error(), Color::Rgb(_, _, _)));
    }

    #[test]
    fn catppuccin_colors_differ_from_eve() {
        let eve = EveTheme;
        let cat = CatppuccinTheme;
        assert_ne!(eve.primary(), cat.primary());
        assert_ne!(eve.background(), cat.background());
    }

    #[test]
    fn theme_names_list_matches_implementations() {
        assert_eq!(THEME_NAMES.len(), 5);
    }

    #[test]
    fn nocolor_theme_uses_reset() {
        let theme = NoColorTheme;
        assert_eq!(theme.primary(), Color::Reset);
        assert_eq!(theme.text(), Color::Reset);
    }

    #[test]
    fn nocolor_theme_uses_ansi_for_status() {
        let theme = NoColorTheme;
        assert_eq!(theme.success(), Color::Green);
        assert_eq!(theme.error(), Color::Red);
        assert_eq!(theme.warning(), Color::Yellow);
    }

    #[test]
    fn resolve_theme_without_no_color_uses_configured() {
        // When NO_COLOR is not set, resolve_theme returns the configured theme.
        if !genesis_config::env::is_present(genesis_config::env::NO_COLOR) {
            let theme = resolve_theme("dracula");
            assert_eq!(theme.name(), "dracula");
        }
    }

    #[test]
    fn user_theme_names_excludes_nocolor() {
        assert!(!USER_THEME_NAMES.contains(&"nocolor"));
        assert!(USER_THEME_NAMES.contains(&"eve"));
    }

    #[test]
    fn user_theme_names_is_subset_of_theme_names() {
        for name in USER_THEME_NAMES {
            assert!(
                THEME_NAMES.contains(name),
                "{name} missing from THEME_NAMES"
            );
        }
    }
}
