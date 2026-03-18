//! Boot sequence effect choreography.
//!
//! Orchestrates four layered effects that run when the TUI first renders:
//!
//! 1. **Title materialization** (~800ms) — "GENESIS" coalesces from random chars.
//! 2. **Portrait materialization** (~1200ms, 200ms delay) — Eve art coalesces.
//! 3. **Status lines** (~1000ms, 1800ms delay) — Subsystem lines fade in.
//! 4. **Settle** (~400ms, 2800ms delay) — Subtle fade on the full welcome area.
//!
//! All effects are registered via [`start_boot_sequence`] on the parent
//! [`GenesisEffects`] manager.

use ratatui::layout::Rect;
use tachyonfx::{fx, Interpolation};

use super::EffectId;

/// Register the full boot sequence on the given effect manager.
///
/// Each sub-effect targets a specific screen region and is delayed to
/// create a staggered reveal.
///
/// Does nothing if the manager reference is not provided (i.e., effects are
/// disabled).
pub fn start_boot_sequence(
    manager: &mut tachyonfx::EffectManager<EffectId>,
    title_area: Rect,
    portrait_area: Rect,
    status_area: Rect,
    full_area: Rect,
) {
    // 1. Title materialization — coalesce from random chars over 800ms.
    let title_effect = fx::coalesce((800, Interpolation::QuadOut)).with_area(title_area);
    manager.add_unique_effect(EffectId::BootTitle, title_effect);

    // 2. Portrait materialization — coalesce with 200ms delay, 1200ms duration.
    let portrait_effect = fx::sequence(&[
        fx::sleep(200),
        fx::coalesce((1200, Interpolation::CubicOut)).with_area(portrait_area),
    ]);
    manager.add_unique_effect(EffectId::BootPortrait, portrait_effect);

    // 3. Status lines — fade from black with 1800ms delay, 1000ms duration.
    let status_effect = fx::sequence(&[
        fx::sleep(1800),
        fx::fade_from_fg(
            ratatui::style::Color::Black,
            (1000, Interpolation::SineOut),
        )
        .with_area(status_area),
    ]);
    manager.add_unique_effect(EffectId::BootStatus, status_effect);

    // 4. Settle — subtle fade at the end (2800ms delay, 400ms duration).
    //    A gentle fade-from on the full area gives a "settling" feel.
    let settle_effect = fx::sequence(&[
        fx::sleep(2800),
        fx::fade_from_fg(
            ratatui::style::Color::Rgb(180, 167, 214),
            (400, Interpolation::SineOut),
        )
        .with_area(full_area),
    ]);
    manager.add_unique_effect(EffectId::BootSettle, settle_effect);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tachyonfx::EffectManager;

    #[test]
    fn boot_sequence_registers_effects_without_panic() {
        let mut manager = EffectManager::<EffectId>::default();
        let area = Rect::new(0, 0, 120, 24);
        let title_area = Rect::new(5, 1, 40, 4);
        let portrait_area = Rect::new(5, 6, 30, 14);
        let status_area = Rect::new(5, 20, 40, 4);

        start_boot_sequence(&mut manager, title_area, portrait_area, status_area, area);

        assert!(manager.is_running(), "boot sequence should be running after start");
    }

    #[test]
    fn boot_sequence_completes_after_sufficient_time() {
        let mut manager = EffectManager::<EffectId>::default();
        let area = Rect::new(0, 0, 120, 24);
        let title_area = Rect::new(5, 1, 40, 4);
        let portrait_area = Rect::new(5, 6, 30, 14);
        let status_area = Rect::new(5, 20, 40, 4);
        let mut buf = ratatui::buffer::Buffer::empty(area);

        start_boot_sequence(&mut manager, title_area, portrait_area, status_area, area);

        // Process enough time for the full sequence (~3.2s = sleep 2800 + settle 400).
        let dt = tachyonfx::Duration::from_millis(4000);
        manager.process_effects(dt, &mut buf, area);

        assert!(!manager.is_running(), "boot sequence should be done after 4s");
    }

    #[test]
    fn boot_sequence_still_running_at_1s() {
        let mut manager = EffectManager::<EffectId>::default();
        let area = Rect::new(0, 0, 120, 24);
        let title_area = Rect::new(5, 1, 40, 4);
        let portrait_area = Rect::new(5, 6, 30, 14);
        let status_area = Rect::new(5, 20, 40, 4);
        let mut buf = ratatui::buffer::Buffer::empty(area);

        start_boot_sequence(&mut manager, title_area, portrait_area, status_area, area);

        // At 1 second, portrait coalesce and status fade should still be active.
        let dt = tachyonfx::Duration::from_millis(1000);
        manager.process_effects(dt, &mut buf, area);

        assert!(manager.is_running(), "boot sequence should still be running at 1s");
    }
}
