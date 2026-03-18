//! Terminal visual effects orchestration layer.
//!
//! Wraps [`tachyonfx::EffectManager`] with genesis-specific effect identifiers,
//! an enable/disable gate, and delta-time tracking for the render loop.

pub mod ambient;
pub mod boot;
pub mod transitions;

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tachyonfx::EffectManager;

/// Identifies each unique effect slot managed by [`GenesisEffects`].
///
/// Using a unique key per logical animation lets the manager cancel an
/// in-flight effect when a new one replaces it (e.g. a second boot title
/// animation supersedes the first).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectId {
    #[default]
    BootTitle,
    BootPortrait,
    BootStatus,
    BootSettle,
    TransitionOut,
    TransitionIn,
    ErrorFlash,
    CompressionSweep,
    IdleGlow,
    IdleBreathing,
    ActivePulse,
    StatusTransition,
}

/// Thin orchestration layer around [`tachyonfx::EffectManager`].
///
/// Guards all processing behind an `enabled` flag so callers never need
/// conditional logic at each call-site, and tracks frame timing for
/// delta-time computation.
pub struct GenesisEffects {
    manager: EffectManager<EffectId>,
    enabled: bool,
    no_color: bool,
    last_frame: Instant,
}

impl GenesisEffects {
    /// Create a new effects manager.
    ///
    /// * `enabled` — when `false`, [`process`](Self::process) is a no-op and
    ///   [`is_running`](Self::is_running) always returns `false`.
    /// * `no_color` — when `true`, colour-dependent effects should be skipped
    ///   by downstream code (checked via [`color_effects_enabled`](Self::color_effects_enabled)).
    pub fn new(enabled: bool, no_color: bool) -> Self {
        Self {
            manager: EffectManager::default(),
            enabled,
            no_color,
            last_frame: Instant::now(),
        }
    }

    /// Process all active effects for the given duration.
    ///
    /// Delegates to [`EffectManager::process_effects`].  No-op when
    /// effects are disabled.
    pub fn process(&mut self, dt: std::time::Duration, buf: &mut Buffer, area: Rect) {
        if !self.enabled {
            return;
        }
        self.manager
            .process_effects(tachyonfx::Duration::from(dt), buf, area);
    }

    /// Returns `true` when effects are enabled **and** at least one effect
    /// is still running.
    pub fn is_running(&self) -> bool {
        self.enabled && self.manager.is_running()
    }

    /// Cancel every known [`EffectId`].
    pub fn cancel_all(&mut self) {
        const ALL_IDS: &[EffectId] = &[
            EffectId::BootTitle,
            EffectId::BootPortrait,
            EffectId::BootStatus,
            EffectId::BootSettle,
            EffectId::TransitionOut,
            EffectId::TransitionIn,
            EffectId::ErrorFlash,
            EffectId::CompressionSweep,
            EffectId::IdleGlow,
            EffectId::IdleBreathing,
            EffectId::ActivePulse,
            EffectId::StatusTransition,
        ];
        for &id in ALL_IDS {
            self.manager.cancel_unique_effect(id);
        }
    }

    /// Compute the wall-clock delta since the last call and update the
    /// internal timestamp.
    ///
    /// Intended to be called once per frame, before [`process`](Self::process).
    pub fn frame_dt(&mut self) -> std::time::Duration {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame);
        self.last_frame = now;
        dt
    }

    /// Returns `true` when effects are enabled **and** colour output is
    /// permitted (`NO_COLOR` / `--no-color` was not set).
    pub fn color_effects_enabled(&self) -> bool {
        self.enabled && !self.no_color
    }

    /// Borrow the inner [`EffectManager`] mutably so callers can
    /// register new effects via [`EffectManager::add_unique_effect`] etc.
    pub fn manager_mut(&mut self) -> &mut EffectManager<EffectId> {
        &mut self.manager
    }

    /// Borrow the inner [`EffectManager`] immutably.
    pub fn manager(&self) -> &EffectManager<EffectId> {
        &self.manager
    }

    /// Launch the boot sequence animation across the four target areas.
    ///
    /// No-op when effects are disabled.  Safe to call multiple times — each
    /// call replaces any in-flight boot effects via the unique-effect system.
    pub fn start_boot_sequence(
        &mut self,
        title_area: Rect,
        portrait_area: Rect,
        status_area: Rect,
        full_area: Rect,
    ) {
        if !self.enabled {
            return;
        }
        boot::start_boot_sequence(
            &mut self.manager,
            title_area,
            portrait_area,
            status_area,
            full_area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_effects_manager_starts_idle() {
        let effects = GenesisEffects::new(true, false);
        assert!(!effects.is_running());
    }

    #[test]
    fn disabled_effects_never_run() {
        let effects = GenesisEffects::new(false, false);
        // Even if we try to process, is_running stays false
        assert!(!effects.is_running());
    }

    #[test]
    fn process_with_no_effects_is_noop() {
        let mut effects = GenesisEffects::new(true, false);
        let area = Rect::new(0, 0, 10, 5);
        let mut buf = Buffer::empty(area);
        effects.process(std::time::Duration::from_millis(16), &mut buf, area);
        // Should not panic
    }

    #[test]
    fn cancel_all_does_not_panic_when_empty() {
        let mut effects = GenesisEffects::new(true, false);
        effects.cancel_all();
        // cancel_all inserts zero-duration sentinels; one process cycle clears them.
        let area = Rect::new(0, 0, 10, 5);
        let mut buf = Buffer::empty(area);
        effects.process(std::time::Duration::from_millis(16), &mut buf, area);
        assert!(!effects.is_running());
    }

    #[test]
    fn frame_dt_returns_positive_duration() {
        let mut effects = GenesisEffects::new(true, false);
        // Burn a tiny bit of wall time so dt > 0.
        std::thread::sleep(std::time::Duration::from_millis(1));
        let dt = effects.frame_dt();
        assert!(dt.as_micros() > 0);
    }

    #[test]
    fn color_effects_enabled_respects_flags() {
        assert!(GenesisEffects::new(true, false).color_effects_enabled());
        assert!(!GenesisEffects::new(true, true).color_effects_enabled());
        assert!(!GenesisEffects::new(false, false).color_effects_enabled());
        assert!(!GenesisEffects::new(false, true).color_effects_enabled());
    }

    #[test]
    fn start_boot_sequence_makes_is_running_true() {
        let mut effects = GenesisEffects::new(true, false);
        let area = Rect::new(0, 0, 120, 24);
        let title = Rect::new(5, 1, 40, 4);
        let portrait = Rect::new(5, 6, 30, 14);
        let status = Rect::new(5, 20, 40, 4);

        effects.start_boot_sequence(title, portrait, status, area);
        assert!(effects.is_running());
    }

    #[test]
    fn start_boot_sequence_noop_when_disabled() {
        let mut effects = GenesisEffects::new(false, false);
        let area = Rect::new(0, 0, 120, 24);
        let title = Rect::new(5, 1, 40, 4);
        let portrait = Rect::new(5, 6, 30, 14);
        let status = Rect::new(5, 20, 40, 4);

        effects.start_boot_sequence(title, portrait, status, area);
        assert!(!effects.is_running());
    }
}
