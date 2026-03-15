//! Conversation history cells.
//!
//! Each cell represents one displayable unit in the conversation: a user
//! message, an agent response, or a tool invocation.

pub mod agent_cell;
pub mod cell;
pub mod tool_cell;
pub mod user_cell;

pub use agent_cell::AgentCell;
pub use cell::HistoryCell;
pub use tool_cell::ToolCell;
pub use user_cell::UserCell;
