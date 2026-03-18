//! Ambient effects for status bar transitions.
//!
//! - **Status activate** — quick fade-from on the dance sprite area when
//!   transitioning from idle to active.
//! - **Status deactivate** — fade-to on animated elements when returning to idle.
//! - **Tool flash** — brief brightness flash on tool name when a tool starts.

use ratatui::layout::Rect;
use ratatui::style::Color;
use tachyonfx::{fx, Effect, Interpolation};

// ── Palette ──────────────────────────────────────────────────────────────────

/// Status bar background (idle).
const BAR_BG: Color = Color::Rgb(30, 28, 36);

// ── Status transition effects ───────────────────────────────────────────────

/// Quick fade-from effect when transitioning from idle to an active state.
///
/// Fades the dance sprite / spinner area from the bar background color,
/// creating a "materialise" look (200ms).
pub fn status_activate(area: Rect) -> Effect {
    fx::fade_from_fg(BAR_BG, (200, Interpolation::QuadOut)).with_area(area)
}

/// Fade-to effect when transitioning back to idle.
///
/// Fades animated elements toward the bar background (250ms).
pub fn status_deactivate(area: Rect) -> Effect {
    fx::fade_to_fg(BAR_BG, (250, Interpolation::QuadIn)).with_area(area)
}

/// Brief brightness flash on the tool name area when a tool starts running.
///
/// Uses an HSL lightness bump that fades back to normal (300ms ping-pong).
pub fn tool_flash(area: Rect) -> Effect {
    fx::ping_pong(
        fx::hsl_shift_fg([0.0, 0.0, 15.0], (300, Interpolation::QuadOut)).with_area(area),
    )
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use tachyonfx::EffectManager;

    use crate::effects::EffectId;

    fn test_area() -> Rect {
        Rect::new(0, 0, 80, 1)
    }

    #[test]
    fn status_activate_creates_effect_without_panic() {
        let _effect = status_activate(test_area());
    }

    #[test]
    fn status_deactivate_creates_effect_without_panic() {
        let _effect = status_deactivate(test_area());
    }

    #[test]
    fn tool_flash_creates_effect_without_panic() {
        let _effect = tool_flash(test_area());
    }

    #[test]
    fn status_transition_effect_completes() {
        let mut manager = EffectManager::<EffectId>::default();
        let area = test_area();
        let effect = status_activate(area);
        manager.add_unique_effect(EffectId::StatusTransition, effect);
        assert!(manager.is_running());

        let mut buf = Buffer::empty(area);
        // 200ms fade should complete within 300ms.
        manager.process_effects(tachyonfx::Duration::from_millis(300), &mut buf, area);
        assert!(!manager.is_running(), "activate effect should finish");
    }
}
