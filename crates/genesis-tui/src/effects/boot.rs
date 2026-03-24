//! Boot sequence effect choreography.
//!
//! Orchestrates two layered effects that run when the TUI first renders:
//!
//! 1. **Title materialization** (~800ms) — "GENESIS" coalesces from random chars.
//! 2. **Status lines** (~800ms, 600ms delay) — Info lines fade in from black.

use ratatui::layout::Rect;
use tachyonfx::{fx, Interpolation};

use super::EffectId;

/// Register the boot sequence on the given effect manager.
///
/// Each sub-effect targets a specific screen region and is delayed to
/// create a staggered reveal.
pub fn start_boot_sequence(
    manager: &mut tachyonfx::EffectManager<EffectId>,
    title_area: Rect,
    status_area: Rect,
) {
    // 1. Title materialization — coalesce from random chars over 800ms.
    let title_effect = fx::coalesce((800, Interpolation::QuadOut)).with_area(title_area);
    manager.add_unique_effect(EffectId::BootTitle, title_effect);

    // 2. Status lines — fade from black with 600ms delay, 800ms duration.
    let status_effect = fx::sequence(&[
        fx::sleep(600),
        fx::fade_from_fg(ratatui::style::Color::Black, (800, Interpolation::SineOut))
            .with_area(status_area),
    ]);
    manager.add_unique_effect(EffectId::BootStatus, status_effect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tachyonfx::EffectManager;

    fn test_areas() -> (Rect, Rect) {
        let title_area = Rect::new(5, 1, 40, 4);
        let status_area = Rect::new(5, 20, 40, 4);
        (title_area, status_area)
    }

    #[test]
    fn boot_sequence_registers_effects_without_panic() {
        let mut manager = EffectManager::<EffectId>::default();
        let (title, status) = test_areas();
        start_boot_sequence(&mut manager, title, status);
        assert!(manager.is_running());
    }

    #[test]
    fn boot_sequence_completes_after_sufficient_time() {
        let mut manager = EffectManager::<EffectId>::default();
        let area = Rect::new(0, 0, 120, 24);
        let (title, status) = test_areas();
        let mut buf = ratatui::buffer::Buffer::empty(area);

        start_boot_sequence(&mut manager, title, status);

        let dt = tachyonfx::Duration::from_millis(4000);
        manager.process_effects(dt, &mut buf, area);
        assert!(!manager.is_running());
    }

    #[test]
    fn boot_sequence_still_running_at_500ms() {
        let mut manager = EffectManager::<EffectId>::default();
        let area = Rect::new(0, 0, 120, 24);
        let (title, status) = test_areas();
        let mut buf = ratatui::buffer::Buffer::empty(area);

        start_boot_sequence(&mut manager, title, status);

        let dt = tachyonfx::Duration::from_millis(500);
        manager.process_effects(dt, &mut buf, area);
        assert!(manager.is_running());
    }
}
