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
    config: genesis_config::EffectsConfig,
    last_frame: Instant,
}

impl GenesisEffects {
    /// Create a new effects manager.
    ///
    /// * `enabled` — when `false`, [`process`](Self::process) is a no-op and
    ///   [`is_running`](Self::is_running) always returns `false`.
    /// * `no_color` — when `true`, colour-dependent effects should be skipped
    ///   by downstream code (checked via [`color_effects_enabled`](Self::color_effects_enabled)).
    /// * `config` — granular per-effect toggles from `tui.effects.*`.
    pub fn new(enabled: bool, no_color: bool, config: genesis_config::EffectsConfig) -> Self {
        Self {
            manager: EffectManager::default(),
            enabled,
            no_color,
            config,
            last_frame: Instant::now(),
        }
    }

    /// Whether a specific effect category is enabled.
    pub fn boot_enabled(&self) -> bool {
        self.enabled && self.config.boot_sequence
    }

    /// Whether transitions are enabled.
    pub fn transitions_enabled(&self) -> bool {
        self.enabled && self.config.transitions
    }

    /// Whether the braille canvas effects are enabled.
    pub fn braille_enabled(&self) -> bool {
        self.enabled && self.config.braille_canvas
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
    ///
    /// The match below ensures a compile error if a variant is added to
    /// `EffectId` but not listed here.
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
        // Exhaustiveness guard — adding a new EffectId variant without updating
        // ALL_IDS above will cause a compile error here.
        const _: () = {
            let _guard = |id: EffectId| match id {
                EffectId::BootTitle
                | EffectId::BootPortrait
                | EffectId::BootStatus
                | EffectId::BootSettle
                | EffectId::TransitionOut
                | EffectId::TransitionIn
                | EffectId::ErrorFlash
                | EffectId::CompressionSweep
                | EffectId::IdleGlow
                | EffectId::IdleBreathing
                | EffectId::ActivePulse
                | EffectId::StatusTransition => {}
            };
        };
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

    /// Start the Welcome → Chat transition: dissolve the welcome screen out.
    ///
    /// Cancels any running boot effects first so they don't compete with the
    /// dissolve. No-op when effects are disabled.
    pub fn start_welcome_to_chat_transition(&mut self, area: Rect) {
        if !self.enabled || !self.config.transitions {
            return;
        }
        self.cancel_all();
        self.manager
            .add_unique_effect(EffectId::TransitionOut, transitions::welcome_dissolve_out(area));
    }

    /// Start the chat screen coalesce-in effect after the terminal has been
    /// cleared.
    ///
    /// No-op when effects or transitions are disabled.
    pub fn start_chat_coalesce(&mut self, area: Rect) {
        if !self.enabled || !self.config.transitions {
            return;
        }
        self.manager
            .add_unique_effect(EffectId::TransitionIn, transitions::chat_coalesce_in(area));
    }

    /// Trigger a brief glitch flash over `area` when a tool call fails.
    ///
    /// Plays a 50 ms dissolve followed by a 100 ms coalesce, giving the
    /// impression that cells briefly corrupt then snap back to normal.
    /// No-op when effects are disabled.
    pub fn flash_error(&mut self, area: Rect) {
        if !self.enabled {
            return;
        }
        self.manager
            .add_unique_effect(EffectId::ErrorFlash, transitions::error_flash(area));
    }

    /// Play a dim sweep across `area` to signal context compression.
    ///
    /// A dark overlay sweeps in over ~400 ms, indicating that the agent has
    /// condensed the conversation history to stay within its token budget.
    /// No-op when effects are disabled.
    ///
    /// TODO: wire this to a `ContextCompressed` agent event once one is added
    /// to `AgentEvent` in genesis-core. Until then this method exists and is
    /// tested, but is not called from the main event handler.
    pub fn sweep_compression(&mut self, area: Rect) {
        if !self.enabled {
            return;
        }
        self.manager
            .add_unique_effect(EffectId::CompressionSweep, transitions::compression_sweep(area));
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
        if !self.enabled || !self.config.boot_sequence {
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

    // ── Status bar transition effects ────────────────────────────────────

    /// Trigger a status bar state transition effect.
    ///
    /// Called after `StatusBarWidget::set_state()` with the status bar area.
    /// Chooses the appropriate effect based on the previous and current state.
    pub fn on_status_change(
        &mut self,
        from: &crate::events::StatusState,
        to: &crate::events::StatusState,
        status_area: Rect,
    ) {
        use crate::events::StatusState;

        if !self.color_effects_enabled() {
            return;
        }

        match (from, to) {
            // Idle → any active state: fade-in the animation area.
            (StatusState::Idle, StatusState::Thinking)
            | (StatusState::Idle, StatusState::Streaming { .. })
            | (StatusState::Idle, StatusState::ToolRunning { .. }) => {
                self.manager.add_unique_effect(
                    EffectId::StatusTransition,
                    ambient::status_activate(status_area),
                );
                // Start the active pulse.
                self.start_active_pulse(status_area);
                // Cancel idle effects.
                self.stop_idle_effects();
            }
            // Any active → Idle: fade-out and stop pulse, start idle effects.
            (_, StatusState::Idle) => {
                self.manager.add_unique_effect(
                    EffectId::StatusTransition,
                    ambient::status_deactivate(status_area),
                );
                self.stop_active_pulse();
                self.start_idle_effects(status_area);
            }
            // Active → ToolRunning: tool name flash.
            (_, StatusState::ToolRunning { .. }) => {
                self.manager.add_unique_effect(
                    EffectId::StatusTransition,
                    ambient::tool_flash(status_area),
                );
            }
            // Other transitions (e.g., Thinking → Streaming): no extra effect.
            _ => {}
        }
    }

    /// Start the active background pulse.
    pub fn start_active_pulse(&mut self, area: Rect) {
        if !self.color_effects_enabled() {
            return;
        }
        ambient::start_active_pulse(&mut self.manager, area);
    }

    /// Stop the active background pulse.
    pub fn stop_active_pulse(&mut self) {
        self.manager.cancel_unique_effect(EffectId::ActivePulse);
    }

    /// Start idle ambient effects (border glow + breathing).
    pub fn start_idle_effects(&mut self, area: Rect) {
        if !self.color_effects_enabled() {
            return;
        }
        ambient::start_idle_glow(&mut self.manager, area);
        ambient::start_idle_breathing(&mut self.manager, area);
    }

    /// Stop idle ambient effects.
    pub fn stop_idle_effects(&mut self) {
        self.manager.cancel_unique_effect(EffectId::IdleGlow);
        self.manager.cancel_unique_effect(EffectId::IdleBreathing);
    }

    /// Whether effects are enabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_effects_manager_starts_idle() {
        let effects = GenesisEffects::new(true, false, Default::default());
        assert!(!effects.is_running());
    }

    #[test]
    fn disabled_effects_never_run() {
        let effects = GenesisEffects::new(false, false, Default::default());
        // Even if we try to process, is_running stays false
        assert!(!effects.is_running());
    }

    #[test]
    fn process_with_no_effects_is_noop() {
        let mut effects = GenesisEffects::new(true, false, Default::default());
        let area = Rect::new(0, 0, 10, 5);
        let mut buf = Buffer::empty(area);
        effects.process(std::time::Duration::from_millis(16), &mut buf, area);
        // Should not panic
    }

    #[test]
    fn cancel_all_does_not_panic_when_empty() {
        let mut effects = GenesisEffects::new(true, false, Default::default());
        effects.cancel_all();
        // cancel_all inserts zero-duration sentinels; one process cycle clears them.
        let area = Rect::new(0, 0, 10, 5);
        let mut buf = Buffer::empty(area);
        effects.process(std::time::Duration::from_millis(16), &mut buf, area);
        assert!(!effects.is_running());
    }

    #[test]
    fn frame_dt_returns_positive_duration() {
        let mut effects = GenesisEffects::new(true, false, Default::default());
        // Burn a tiny bit of wall time so dt > 0.
        std::thread::sleep(std::time::Duration::from_millis(1));
        let dt = effects.frame_dt();
        assert!(dt.as_micros() > 0);
    }

    #[test]
    fn color_effects_enabled_respects_flags() {
        assert!(GenesisEffects::new(true, false, Default::default()).color_effects_enabled());
        assert!(!GenesisEffects::new(true, true, Default::default()).color_effects_enabled());
        assert!(!GenesisEffects::new(false, false, Default::default()).color_effects_enabled());
        assert!(!GenesisEffects::new(false, true, Default::default()).color_effects_enabled());
    }

    #[test]
    fn start_boot_sequence_makes_is_running_true() {
        let mut effects = GenesisEffects::new(true, false, Default::default());
        let area = Rect::new(0, 0, 120, 24);
        let title = Rect::new(5, 1, 40, 4);
        let portrait = Rect::new(5, 6, 30, 14);
        let status = Rect::new(5, 20, 40, 4);

        effects.start_boot_sequence(title, portrait, status, area);
        assert!(effects.is_running());
    }

    #[test]
    fn start_boot_sequence_noop_when_disabled() {
        let mut effects = GenesisEffects::new(false, false, Default::default());
        let area = Rect::new(0, 0, 120, 24);
        let title = Rect::new(5, 1, 40, 4);
        let portrait = Rect::new(5, 6, 30, 14);
        let status = Rect::new(5, 20, 40, 4);

        effects.start_boot_sequence(title, portrait, status, area);
        assert!(!effects.is_running());
    }

    #[test]
    fn start_welcome_to_chat_transition_cancels_boot_and_starts_dissolve() {
        let mut effects = GenesisEffects::new(true, false, Default::default());
        let area = Rect::new(0, 0, 120, 24);
        let title = Rect::new(5, 1, 40, 4);
        let portrait = Rect::new(5, 6, 30, 14);
        let status = Rect::new(5, 20, 40, 4);

        // Start the boot sequence first.
        effects.start_boot_sequence(title, portrait, status, area);
        assert!(effects.is_running(), "boot effects should be running");

        // Transition should cancel boot and register the dissolve-out effect.
        effects.start_welcome_to_chat_transition(area);

        // After cancellation + dissolve registration, at least the dissolve is live.
        // Process one frame to clear the zero-duration boot sentinels.
        let mut buf = Buffer::empty(area);
        effects.process(std::time::Duration::from_millis(1), &mut buf, area);

        // TransitionOut (dissolve 300 ms) is still running.
        assert!(effects.is_running(), "dissolve-out should be running after transition starts");
    }

    #[test]
    fn start_welcome_to_chat_transition_noop_when_disabled() {
        let mut effects = GenesisEffects::new(false, false, Default::default());
        let area = Rect::new(0, 0, 120, 24);
        effects.start_welcome_to_chat_transition(area);
        assert!(!effects.is_running());
    }

    #[test]
    fn start_chat_coalesce_starts_effect() {
        let mut effects = GenesisEffects::new(true, false, Default::default());
        let area = Rect::new(0, 0, 120, 24);
        effects.start_chat_coalesce(area);
        assert!(effects.is_running(), "coalesce-in should be running");
    }

    #[test]
    fn start_chat_coalesce_noop_when_disabled() {
        let mut effects = GenesisEffects::new(false, false, Default::default());
        let area = Rect::new(0, 0, 120, 24);
        effects.start_chat_coalesce(area);
        assert!(!effects.is_running());
    }

    #[test]
    fn flash_error_creates_effect_without_panic() {
        let mut effects = GenesisEffects::new(true, false, Default::default());
        let area = Rect::new(0, 0, 80, 24);
        effects.flash_error(area);
        assert!(effects.is_running(), "error flash should be running");
    }

    #[test]
    fn flash_error_is_noop_when_disabled() {
        let mut effects = GenesisEffects::new(false, false, Default::default());
        let area = Rect::new(0, 0, 80, 24);
        effects.flash_error(area);
        assert!(!effects.is_running(), "flash_error should be no-op when disabled");
    }

    #[test]
    fn sweep_compression_creates_effect_without_panic() {
        let mut effects = GenesisEffects::new(true, false, Default::default());
        let area = Rect::new(0, 0, 80, 24);
        effects.sweep_compression(area);
        assert!(effects.is_running(), "compression sweep should be running");
    }

    #[test]
    fn sweep_compression_is_noop_when_disabled() {
        let mut effects = GenesisEffects::new(false, false, Default::default());
        let area = Rect::new(0, 0, 80, 24);
        effects.sweep_compression(area);
        assert!(!effects.is_running(), "sweep_compression should be no-op when disabled");
    }

    #[test]
    fn on_status_change_idle_to_thinking_starts_effects() {
        use crate::events::StatusState;

        let mut effects = GenesisEffects::new(true, false, Default::default());
        let area = Rect::new(0, 0, 80, 1);
        effects.on_status_change(&StatusState::Idle, &StatusState::Thinking, area);
        assert!(effects.is_running(), "transition + pulse should be running");
    }

    #[test]
    fn on_status_change_thinking_to_idle_starts_idle_effects() {
        use crate::events::StatusState;

        let mut effects = GenesisEffects::new(true, false, Default::default());
        let area = Rect::new(0, 0, 80, 1);
        effects.on_status_change(&StatusState::Thinking, &StatusState::Idle, area);
        assert!(effects.is_running(), "deactivate + idle effects should be running");
    }

    #[test]
    fn on_status_change_noop_when_disabled() {
        use crate::events::StatusState;

        let mut effects = GenesisEffects::new(false, false, Default::default());
        let area = Rect::new(0, 0, 80, 1);
        effects.on_status_change(&StatusState::Idle, &StatusState::Thinking, area);
        assert!(!effects.is_running());
    }

    #[test]
    fn on_status_change_noop_when_no_color() {
        use crate::events::StatusState;

        let mut effects = GenesisEffects::new(true, true, Default::default());
        let area = Rect::new(0, 0, 80, 1);
        effects.on_status_change(&StatusState::Idle, &StatusState::Thinking, area);
        assert!(!effects.is_running());
    }

    #[test]
    fn start_idle_effects_makes_running() {
        let mut effects = GenesisEffects::new(true, false, Default::default());
        let area = Rect::new(0, 0, 80, 24);
        effects.start_idle_effects(area);
        assert!(effects.is_running(), "idle effects should be running");
    }

    #[test]
    fn stop_idle_effects_cancels() {
        let mut effects = GenesisEffects::new(true, false, Default::default());
        let area = Rect::new(0, 0, 80, 24);
        effects.start_idle_effects(area);
        effects.stop_idle_effects();
        // Process one frame to clear cancelled effects.
        let mut buf = Buffer::empty(area);
        effects.process(std::time::Duration::from_millis(16), &mut buf, area);
        assert!(!effects.is_running(), "idle effects should be stopped");
    }

    #[test]
    fn active_pulse_starts_and_stops() {
        let mut effects = GenesisEffects::new(true, false, Default::default());
        let area = Rect::new(0, 0, 80, 1);
        effects.start_active_pulse(area);
        assert!(effects.is_running());

        effects.stop_active_pulse();
        let mut buf = Buffer::empty(area);
        effects.process(std::time::Duration::from_millis(16), &mut buf, area);
        assert!(!effects.is_running());
    }
}
