use std::collections::BTreeMap;

use mlua::{Function, MultiValue, Value};
use serde::{Deserialize, Serialize};

use crate::api::PluginContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    PreTurn,
    PostTurn,
    PreToolCall,
    PostToolCall,
    OnMessage,
    OnError,
    OnComplete,
    OnPluginLoad,
}

impl HookEvent {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "PreTurn" | "pre_turn" => Some(Self::PreTurn),
            "PostTurn" | "post_turn" => Some(Self::PostTurn),
            "PreToolCall" | "pre_tool_call" => Some(Self::PreToolCall),
            "PostToolCall" | "post_tool_call" => Some(Self::PostToolCall),
            "OnMessage" | "on_message" => Some(Self::OnMessage),
            "OnError" | "on_error" => Some(Self::OnError),
            "OnComplete" | "on_complete" => Some(Self::OnComplete),
            "OnPluginLoad" | "on_plugin_load" => Some(Self::OnPluginLoad),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreHookOutcome<T> {
    Allow(T),
    Veto { reason: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostHookOutcome<T> {
    Keep(T),
    Rewrite(T),
}

impl<T> PreHookOutcome<T> {
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Allow(value) => Some(value),
            Self::Veto { .. } => None,
        }
    }
}

impl<T> PostHookOutcome<T> {
    pub fn into_value(self) -> T {
        match self {
            Self::Keep(value) | Self::Rewrite(value) => value,
        }
    }
}

#[derive(Debug, Default)]
pub struct HookRegistry {
    callbacks: BTreeMap<HookEvent, Vec<HookCallback>>,
}

#[derive(Debug, Clone)]
pub(crate) struct HookCallback {
    pub(crate) function: Function,
    pub(crate) plugin_context: Option<PluginContext>,
}

impl HookRegistry {
    pub(crate) fn register(
        &mut self,
        event: HookEvent,
        callback: Function,
        plugin_context: Option<PluginContext>,
    ) {
        self.callbacks.entry(event).or_default().push(HookCallback {
            function: callback,
            plugin_context,
        });
    }

    pub(crate) fn callbacks(&self, event: HookEvent) -> Vec<HookCallback> {
        self.callbacks.get(&event).cloned().unwrap_or_default()
    }
}

pub(crate) fn parse_pre_hook_result(
    values: MultiValue,
    original: String,
) -> Result<PreHookOutcome<String>, mlua::Error> {
    let mut iter = values.into_iter();
    match iter.next() {
        None | Some(Value::Nil) | Some(Value::Boolean(true)) => Ok(PreHookOutcome::Allow(original)),
        Some(Value::Boolean(false)) => {
            let reason = match iter.next() {
                Some(Value::String(s)) => Some(s.to_str()?.to_owned()),
                Some(Value::Nil) | None => None,
                Some(other) => Some(other.to_string()?),
            };
            Ok(PreHookOutcome::Veto { reason })
        }
        Some(Value::String(s)) => Ok(PreHookOutcome::Allow(s.to_str()?.to_owned())),
        Some(other) => Ok(PreHookOutcome::Allow(other.to_string()?)),
    }
}

pub(crate) fn parse_post_hook_result(
    values: MultiValue,
    current: String,
) -> Result<PostHookOutcome<String>, mlua::Error> {
    let mut iter = values.into_iter();
    match iter.next() {
        None | Some(Value::Nil) | Some(Value::Boolean(true)) => Ok(PostHookOutcome::Keep(current)),
        Some(Value::String(s)) => Ok(PostHookOutcome::Rewrite(s.to_str()?.to_owned())),
        Some(other) => Ok(PostHookOutcome::Rewrite(other.to_string()?)),
    }
}
