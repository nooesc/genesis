//! Ratatui widget implementations for the Genesis TUI.

pub mod approval_overlay;
pub mod chat_widget;
pub mod clarification;
pub mod command_popup;
pub mod help_overlay;
pub mod input_widget;
pub mod status_bar;
pub mod transcript;
pub mod welcome;

pub use chat_widget::ChatWidget;
pub use clarification::{ClarificationAction, ClarificationWidget};
pub use command_popup::{CommandAction, CommandPopup};
pub use help_overlay::{HelpAction, HelpOverlay};
pub use input_widget::{InputAction, InputWidget};
pub use status_bar::StatusBarWidget;
pub use transcript::{TranscriptAction, TranscriptOverlay};
pub use welcome::{WelcomeInfo, WelcomeWidget};
