//! Ambient idle effects (glow, breathing, pulse).
//!
//! These effects run continuously when the agent is idle or active:
//!
//! - **Border glow** — slow brightness cycle on panel borders (~4-5s cycle).
//! - **Status breathing** — very slow (6-8s) brightness oscillation on the
//!   diamond indicator.
//! - **Active pulse** — background color breathing when the agent is working.
//! - **Status activate** — quick fade-from on the dance sprite area when
//!   transitioning from idle to active.
//! - **Status deactivate** — fade-to on animated elements when returning to idle.
//! - **Tool flash** — brief brightness flash on tool name when a tool starts.

use ratatui::layout::Rect;
use ratatui::style::Color;
use tachyonfx::{fx, Effect, Interpolation};

use super::EffectId;

// ── Palette ──────────────────────────────────────────────────────────────────

/// Status bar background (idle). Shared with transitions.
pub(crate) const BAR_BG: Color = Color::Rgb(30, 28, 36);

// ── Status transition effects (Task 12) ─────────────────────────────────────

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

// ── Active pulse (Task 14) ──────────────────────────────────────────────────

/// Subtle background color breathing while the agent is working.
///
/// Cycles the background lightness up slightly over 1.5s, then reverses,
/// creating a gentle pulse. Wrapped in `repeating` so it loops forever
/// until cancelled.
pub fn active_pulse(area: Rect) -> Effect {
    fx::repeating(fx::ping_pong(
        fx::hsl_shift(None, Some([0.0, 0.0, 3.0]), (1500, Interpolation::SineInOut))
            .with_area(area),
    ))
}

// ── Idle animations (Task 15) ───────────────────────────────────────────────

/// Slow brightness cycle on panel borders (~4.5s full cycle).
///
/// Shifts foreground lightness by a small amount, then reverses.
/// Wrapped in `repeating` for continuous looping.
pub fn idle_border_glow(area: Rect) -> Effect {
    fx::repeating(fx::ping_pong(
        fx::hsl_shift_fg([0.0, 0.0, 8.0], (2250, Interpolation::SineInOut)).with_area(area),
    ))
}

/// Very slow brightness oscillation on the diamond indicator (~7s cycle).
///
/// Shifts the foreground lightness of the indicator character so it
/// gently pulses, breathing life into the idle UI.
pub fn idle_breathing(area: Rect) -> Effect {
    fx::repeating(fx::ping_pong(
        fx::hsl_shift_fg([0.0, 0.0, 12.0], (3500, Interpolation::SineInOut)).with_area(area),
    ))
}

// ── Convenience constructors for GenesisEffects ─────────────────────────────

/// Register the idle glow effect on the given manager.
pub fn start_idle_glow(
    manager: &mut tachyonfx::EffectManager<EffectId>,
    area: Rect,
) {
    manager.add_unique_effect(EffectId::IdleGlow, idle_border_glow(area));
}

/// Register the idle breathing effect on the given manager.
pub fn start_idle_breathing(
    manager: &mut tachyonfx::EffectManager<EffectId>,
    area: Rect,
) {
    manager.add_unique_effect(EffectId::IdleBreathing, idle_breathing(area));
}

/// Register the active pulse effect on the given manager.
pub fn start_active_pulse(
    manager: &mut tachyonfx::EffectManager<EffectId>,
    area: Rect,
) {
    manager.add_unique_effect(EffectId::ActivePulse, active_pulse(area));
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use tachyonfx::EffectManager;

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
    fn active_pulse_creates_and_runs() {
        let mut manager = EffectManager::<EffectId>::default();
        let area = test_area();
        start_active_pulse(&mut manager, area);
        assert!(manager.is_running());

        // Process a frame — should not panic.
        let mut buf = Buffer::empty(area);
        manager.process_effects(tachyonfx::Duration::from_millis(16), &mut buf, area);
        // Repeating effect is still running.
        assert!(manager.is_running());
    }

    #[test]
    fn idle_border_glow_creates_and_runs() {
        let mut manager = EffectManager::<EffectId>::default();
        let area = test_area();
        start_idle_glow(&mut manager, area);
        assert!(manager.is_running());

        let mut buf = Buffer::empty(area);
        manager.process_effects(tachyonfx::Duration::from_millis(100), &mut buf, area);
        assert!(manager.is_running(), "repeating glow should keep running");
    }

    #[test]
    fn idle_breathing_creates_and_runs() {
        let mut manager = EffectManager::<EffectId>::default();
        let area = test_area();
        start_idle_breathing(&mut manager, area);
        assert!(manager.is_running());

        let mut buf = Buffer::empty(area);
        manager.process_effects(tachyonfx::Duration::from_millis(100), &mut buf, area);
        assert!(manager.is_running(), "repeating breathing should keep running");
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
