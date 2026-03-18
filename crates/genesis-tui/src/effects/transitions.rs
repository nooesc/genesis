//! Screen transition effects (dissolve, coalesce, sweep).
//!
//! Provides effects for the Welcome → Chat transition and runtime feedback:
//!
//! 1. [`welcome_dissolve_out`] — dissolves the welcome screen over ~300 ms.
//! 2. [`chat_coalesce_in`] — coalesces the chat screen in over ~300 ms with a
//!    100 ms blank gap at the start so the clear is visually distinct.
//! 3. [`error_flash`] — brief dissolve→coalesce glitch (~150 ms) on tool failure.
//! 4. [`compression_sweep`] — dim sweep across the chat area on context compression.

use ratatui::layout::Rect;
use tachyonfx::{fx, Effect, Interpolation, Motion};

/// Dissolve the welcome screen out over ~300 ms.
///
/// The effect targets the full viewport area passed in and uses a
/// [`QuadIn`](Interpolation::QuadIn) curve so the dissolve accelerates,
/// giving the impression that the screen is "snapping" away.
pub fn welcome_dissolve_out(area: Rect) -> Effect {
    fx::dissolve((300, Interpolation::QuadIn)).with_area(area)
}

/// Coalesce the chat screen in over ~300 ms with a 100 ms blank gap.
///
/// The leading sleep gives the terminal clear a moment to propagate before
/// the new content materialises, preventing a jarring visual cut.
pub fn chat_coalesce_in(area: Rect) -> Effect {
    fx::sequence(&[
        fx::sleep(100),
        fx::coalesce((300, Interpolation::QuadOut)).with_area(area),
    ])
}

/// Brief glitch effect when a tool call fails (~150 ms total).
///
/// Dissolves cells away with a fast [`QuadIn`](Interpolation::QuadIn) curve
/// (50 ms) then coalesces them back with [`QuadOut`](Interpolation::QuadOut)
/// (100 ms), creating a quick "corruption → recovery" visual.
pub fn error_flash(area: Rect) -> Effect {
    fx::sequence(&[
        fx::dissolve((50, Interpolation::QuadIn)).with_area(area),
        fx::coalesce((100, Interpolation::QuadOut)).with_area(area),
    ])
}

/// Dim sweep across the chat area when the agent compresses context (~400 ms).
///
/// Uses a dark overlay colour swept left-to-right with a
/// [`SineInOut`](Interpolation::SineInOut) curve, conveying that the visible
/// history is being condensed without losing the user's place.
pub fn compression_sweep(area: Rect) -> Effect {
    fx::sweep_in(
        Motion::LeftToRight,
        10,  // gradient_length
        0,   // randomness
        super::ambient::BAR_BG,
        (400, Interpolation::SineInOut),
    )
    .with_area(area)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_dissolve_out_creates_effect_without_panic() {
        let area = Rect::new(0, 0, 80, 24);
        let _effect = welcome_dissolve_out(area);
        // If we get here, construction succeeded.
    }

    #[test]
    fn chat_coalesce_in_creates_effect_without_panic() {
        let area = Rect::new(0, 0, 80, 24);
        let _effect = chat_coalesce_in(area);
    }

    #[test]
    fn dissolve_out_runs_and_completes() {
        use tachyonfx::EffectManager;

        let area = Rect::new(0, 0, 80, 24);
        let mut manager = EffectManager::<super::super::EffectId>::default();
        let effect = welcome_dissolve_out(area);
        manager.add_unique_effect(super::super::EffectId::TransitionOut, effect);

        assert!(manager.is_running());

        let mut buf = ratatui::buffer::Buffer::empty(area);
        // 300 ms should be enough for the 300 ms dissolve.
        manager.process_effects(tachyonfx::Duration::from_millis(400), &mut buf, area);
        assert!(!manager.is_running(), "dissolve should finish within 400 ms");
    }

    #[test]
    fn coalesce_in_runs_and_completes() {
        use tachyonfx::EffectManager;

        let area = Rect::new(0, 0, 80, 24);
        let mut manager = EffectManager::<super::super::EffectId>::default();
        let effect = chat_coalesce_in(area);
        manager.add_unique_effect(super::super::EffectId::TransitionIn, effect);

        assert!(manager.is_running());

        let mut buf = ratatui::buffer::Buffer::empty(area);
        // 100 ms sleep + 300 ms coalesce = 400 ms total.
        manager.process_effects(tachyonfx::Duration::from_millis(500), &mut buf, area);
        assert!(!manager.is_running(), "coalesce should finish within 500 ms");
    }

    #[test]
    fn error_flash_creates_effect_without_panic() {
        let area = Rect::new(0, 0, 80, 24);
        let _effect = error_flash(area);
        // If we get here, construction succeeded.
    }

    #[test]
    fn error_flash_runs_and_completes() {
        use tachyonfx::EffectManager;

        let area = Rect::new(0, 0, 80, 24);
        let mut manager = EffectManager::<super::super::EffectId>::default();
        let effect = error_flash(area);
        manager.add_unique_effect(super::super::EffectId::ErrorFlash, effect);

        assert!(manager.is_running());

        let mut buf = ratatui::buffer::Buffer::empty(area);
        // 50 ms dissolve + 100 ms coalesce = 150 ms total.
        manager.process_effects(tachyonfx::Duration::from_millis(200), &mut buf, area);
        assert!(!manager.is_running(), "error flash should finish within 200 ms");
    }

    #[test]
    fn compression_sweep_creates_effect_without_panic() {
        let area = Rect::new(0, 0, 80, 24);
        let _effect = compression_sweep(area);
        // If we get here, construction succeeded.
    }

    #[test]
    fn compression_sweep_runs_and_completes() {
        use tachyonfx::EffectManager;

        let area = Rect::new(0, 0, 80, 24);
        let mut manager = EffectManager::<super::super::EffectId>::default();
        let effect = compression_sweep(area);
        manager.add_unique_effect(super::super::EffectId::CompressionSweep, effect);

        assert!(manager.is_running());

        let mut buf = ratatui::buffer::Buffer::empty(area);
        // 400 ms sweep.
        manager.process_effects(tachyonfx::Duration::from_millis(500), &mut buf, area);
        assert!(!manager.is_running(), "compression sweep should finish within 500 ms");
    }
}
