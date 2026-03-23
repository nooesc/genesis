//! Route handler modules for the Genesis gateway.
//!
//! Each submodule contains handlers for a logical group of endpoints.

pub(crate) mod admin;
pub(crate) mod chat;
pub(crate) mod health;
pub(crate) mod memories;
pub(crate) mod pairing;
pub(crate) mod schedules;
pub(crate) mod sessions;
pub(crate) mod skills;
pub(crate) mod subagents;
pub(crate) mod tools;
pub(crate) mod user_model;

// Re-export all handler functions so `build_router` can reference them unqualified.
pub(crate) use admin::*;
pub(crate) use chat::*;
pub(crate) use health::*;
pub(crate) use memories::*;
pub(crate) use pairing::*;
pub(crate) use schedules::*;
pub(crate) use sessions::*;
pub(crate) use skills::*;
pub(crate) use subagents::*;
pub(crate) use tools::*;
pub(crate) use user_model::*;
