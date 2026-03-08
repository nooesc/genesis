//! Toolset distributions for training data generation.
//!
//! When generating batch training data, we want diverse tool configurations.
//! A toolset distribution defines inclusion probabilities for each tool,
//! allowing randomized but controlled tool availability per training sample.

use std::collections::{BTreeMap, HashSet};

use rand::Rng;
use serde::{Deserialize, Serialize};

/// A named toolset distribution mapping tool names to inclusion probabilities.
///
/// Each probability is in `[0.0, 1.0]`:
/// - `1.0` = always included
/// - `0.0` = never included
/// - `0.5` = 50% chance of inclusion per sample
///
/// Tools not listed in the distribution are excluded by default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsetDistribution {
    pub name: String,
    pub description: String,
    /// Map of tool name -> inclusion probability (0.0 to 1.0).
    pub tools: BTreeMap<String, f64>,
}

impl ToolsetDistribution {
    /// Sample the distribution and return the set of tool names to include.
    pub fn sample<R: Rng>(&self, rng: &mut R) -> HashSet<String> {
        self.tools
            .iter()
            .filter(|(_, &prob)| rng.random::<f64>() < prob)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Return all tools that have a non-zero probability.
    pub fn possible_tools(&self) -> HashSet<String> {
        self.tools
            .iter()
            .filter(|(_, &prob)| prob > 0.0)
            .map(|(name, _)| name.clone())
            .collect()
    }
}

// Core tools available in almost every distribution.
const CORE_TOOLS: &[&str] = &[
    "shell_exec",
    "read_file",
    "write_file",
    "patch",
    "list_dir",
    "list_tree",
    "glob_search",
    "search_files",
    "echo",
    "session_info",
];

// Git tools for development-oriented distributions.
const GIT_TOOLS: &[&str] = &["git_status", "git_diff", "git_log", "git_commit", "git_branch"];

// Web/research tools.
const WEB_TOOLS: &[&str] = &["web_search", "web_request", "browse"];

// Memory and session tools.
const MEMORY_TOOLS: &[&str] = &[
    "memory_store",
    "memory_recall",
    "session_history",
    "session_search",
    "session_export",
    "user_model",
    "user_observe",
];

// System/ops tools.
const SYSTEM_TOOLS: &[&str] = &[
    "system_info",
    "list_processes",
    "kill_process",
    "docker_exec",
    "ssh_exec",
];

// Agent coordination tools.
const AGENT_TOOLS: &[&str] = &[
    "spawn_subagent",
    "check_subagent",
    "list_subagents",
    "mixture_of_agents",
    "reason_with_model",
];

// Creative/multimodal tools.
const CREATIVE_TOOLS: &[&str] = &[
    "image_generation",
    "text_to_speech",
    "vision",
    "transcribe",
    "code_execution",
];

// Home automation tools.
const HA_TOOLS: &[&str] = &[
    "ha_get_state",
    "ha_call_service",
    "ha_list_entities",
    "ha_list_services",
];

// Skill management tools.
const SKILL_TOOLS: &[&str] = &[
    "skill_create",
    "skill_delete",
    "skill_get",
    "skill_list",
    "skill_record_usage",
];

// Safety/control tools.
const SAFETY_TOOLS: &[&str] = &["clarify", "send_message", "todo"];

// Scheduling tools.
const SCHEDULE_TOOLS: &[&str] = &["schedule_create", "schedule_delete", "schedule_list"];

fn tools_with_prob(names: &[&str], prob: f64) -> BTreeMap<String, f64> {
    names.iter().map(|n| (n.to_string(), prob)).collect()
}

fn merge(base: &mut BTreeMap<String, f64>, additions: BTreeMap<String, f64>) {
    for (k, v) in additions {
        base.insert(k, v);
    }
}

/// Get a named built-in distribution.
pub fn builtin_distribution(name: &str) -> Option<ToolsetDistribution> {
    match name {
        "full" => Some(full()),
        "development" => Some(development()),
        "research" => Some(research()),
        "safe" => Some(safe()),
        "minimal" => Some(minimal()),
        "creative" => Some(creative()),
        "ops" => Some(ops()),
        "home-assistant" => Some(home_assistant()),
        "coding-agent" => Some(coding_agent()),
        "random" => Some(random()),
        _ => None,
    }
}

/// List all built-in distribution names.
pub fn builtin_distribution_names() -> Vec<&'static str> {
    vec![
        "full",
        "development",
        "research",
        "safe",
        "minimal",
        "creative",
        "ops",
        "home-assistant",
        "coding-agent",
        "random",
    ]
}

/// Full distribution: every tool at 100%.
fn full() -> ToolsetDistribution {
    let mut tools = BTreeMap::new();
    for &group in &[
        CORE_TOOLS,
        GIT_TOOLS,
        WEB_TOOLS,
        MEMORY_TOOLS,
        SYSTEM_TOOLS,
        AGENT_TOOLS,
        CREATIVE_TOOLS,
        HA_TOOLS,
        SKILL_TOOLS,
        SAFETY_TOOLS,
        SCHEDULE_TOOLS,
    ] {
        merge(&mut tools, tools_with_prob(group, 1.0));
    }
    // Include remaining tools not in any group.
    tools.insert("trajectory".to_owned(), 1.0);
    tools.insert("dangerous_tool".to_owned(), 1.0);
    tools.insert("guarded_tool".to_owned(), 1.0);

    ToolsetDistribution {
        name: "full".to_owned(),
        description: "All tools enabled at 100% probability.".to_owned(),
        tools,
    }
}

/// Development: core + git + search + limited web. Focused on coding.
fn development() -> ToolsetDistribution {
    let mut tools = tools_with_prob(CORE_TOOLS, 1.0);
    merge(&mut tools, tools_with_prob(GIT_TOOLS, 1.0));
    merge(&mut tools, tools_with_prob(WEB_TOOLS, 0.3));
    merge(&mut tools, tools_with_prob(SAFETY_TOOLS, 0.8));
    merge(&mut tools, tools_with_prob(MEMORY_TOOLS, 0.4));
    tools.insert("code_execution".to_owned(), 0.5);
    tools.insert("trajectory".to_owned(), 0.2);

    ToolsetDistribution {
        name: "development".to_owned(),
        description: "Core file/shell tools + git. Web and memory at reduced probability."
            .to_owned(),
        tools,
    }
}

/// Research: core + web + memory, no git or system tools.
fn research() -> ToolsetDistribution {
    let mut tools = tools_with_prob(CORE_TOOLS, 1.0);
    merge(&mut tools, tools_with_prob(WEB_TOOLS, 1.0));
    merge(&mut tools, tools_with_prob(MEMORY_TOOLS, 0.8));
    merge(&mut tools, tools_with_prob(SAFETY_TOOLS, 1.0));
    merge(&mut tools, tools_with_prob(AGENT_TOOLS, 0.5));
    tools.insert("code_execution".to_owned(), 0.6);

    ToolsetDistribution {
        name: "research".to_owned(),
        description: "Web research focused: core + web + memory + agents.".to_owned(),
        tools,
    }
}

/// Safe: no shell, no file write, no destructive tools. Read-only research.
fn safe() -> ToolsetDistribution {
    let mut tools = BTreeMap::new();
    // Read-only file tools.
    for name in &[
        "read_file",
        "list_dir",
        "list_tree",
        "glob_search",
        "search_files",
        "echo",
        "session_info",
    ] {
        tools.insert(name.to_string(), 1.0);
    }
    merge(&mut tools, tools_with_prob(WEB_TOOLS, 0.8));
    merge(&mut tools, tools_with_prob(MEMORY_TOOLS, 0.6));
    merge(&mut tools, tools_with_prob(SAFETY_TOOLS, 1.0));
    tools.insert("reason_with_model".to_owned(), 0.5);

    ToolsetDistribution {
        name: "safe".to_owned(),
        description: "Read-only: no shell, no file writes, no destructive tools.".to_owned(),
        tools,
    }
}

/// Minimal: just file I/O and shell. Bare-bones agent.
fn minimal() -> ToolsetDistribution {
    let tools = tools_with_prob(
        &[
            "shell_exec",
            "read_file",
            "write_file",
            "patch",
            "list_dir",
            "echo",
        ],
        1.0,
    );

    ToolsetDistribution {
        name: "minimal".to_owned(),
        description: "Bare-bones: shell + basic file I/O only.".to_owned(),
        tools,
    }
}

/// Creative: core + multimodal/creative tools.
fn creative() -> ToolsetDistribution {
    let mut tools = tools_with_prob(CORE_TOOLS, 1.0);
    merge(&mut tools, tools_with_prob(CREATIVE_TOOLS, 0.9));
    merge(&mut tools, tools_with_prob(WEB_TOOLS, 0.6));
    merge(&mut tools, tools_with_prob(MEMORY_TOOLS, 0.5));
    merge(&mut tools, tools_with_prob(SAFETY_TOOLS, 0.8));

    ToolsetDistribution {
        name: "creative".to_owned(),
        description: "Core + multimodal tools (image, audio, vision, code execution).".to_owned(),
        tools,
    }
}

/// Ops: core + system administration + docker/ssh.
fn ops() -> ToolsetDistribution {
    let mut tools = tools_with_prob(CORE_TOOLS, 1.0);
    merge(&mut tools, tools_with_prob(SYSTEM_TOOLS, 1.0));
    merge(&mut tools, tools_with_prob(GIT_TOOLS, 0.5));
    merge(&mut tools, tools_with_prob(SAFETY_TOOLS, 0.8));
    merge(&mut tools, tools_with_prob(SCHEDULE_TOOLS, 0.6));

    ToolsetDistribution {
        name: "ops".to_owned(),
        description: "System ops: core + docker/ssh/processes + scheduling.".to_owned(),
        tools,
    }
}

/// Home Assistant: core + HA tools.
fn home_assistant() -> ToolsetDistribution {
    let mut tools = tools_with_prob(CORE_TOOLS, 0.8);
    merge(&mut tools, tools_with_prob(HA_TOOLS, 1.0));
    merge(&mut tools, tools_with_prob(SCHEDULE_TOOLS, 0.8));
    merge(&mut tools, tools_with_prob(SAFETY_TOOLS, 1.0));
    merge(&mut tools, tools_with_prob(MEMORY_TOOLS, 0.5));

    ToolsetDistribution {
        name: "home-assistant".to_owned(),
        description: "Home automation: core + Home Assistant + scheduling.".to_owned(),
        tools,
    }
}

/// Coding agent: high-fidelity developer distribution matching Claude Code behavior.
fn coding_agent() -> ToolsetDistribution {
    let mut tools = tools_with_prob(CORE_TOOLS, 1.0);
    merge(&mut tools, tools_with_prob(GIT_TOOLS, 1.0));
    merge(&mut tools, tools_with_prob(SAFETY_TOOLS, 1.0));
    merge(&mut tools, tools_with_prob(WEB_TOOLS, 0.7));
    merge(&mut tools, tools_with_prob(AGENT_TOOLS, 0.6));
    merge(&mut tools, tools_with_prob(MEMORY_TOOLS, 0.5));
    tools.insert("code_execution".to_owned(), 0.4);

    ToolsetDistribution {
        name: "coding-agent".to_owned(),
        description:
            "High-fidelity coding agent: all dev tools + git + web + agents at varied rates."
                .to_owned(),
        tools,
    }
}

/// Random: each tool has an independent 50% inclusion probability.
/// Produces highly varied configurations for diversity.
fn random() -> ToolsetDistribution {
    let mut tools = BTreeMap::new();
    for &group in &[
        CORE_TOOLS,
        GIT_TOOLS,
        WEB_TOOLS,
        MEMORY_TOOLS,
        SYSTEM_TOOLS,
        AGENT_TOOLS,
        CREATIVE_TOOLS,
        HA_TOOLS,
        SKILL_TOOLS,
        SAFETY_TOOLS,
        SCHEDULE_TOOLS,
    ] {
        merge(&mut tools, tools_with_prob(group, 0.5));
    }
    tools.insert("trajectory".to_owned(), 0.5);

    ToolsetDistribution {
        name: "random".to_owned(),
        description: "Every tool at 50% — maximum diversity.".to_owned(),
        tools,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn full_distribution_includes_all_tools() {
        let dist = builtin_distribution("full").unwrap();
        let tools = dist.possible_tools();
        // Should include all major tool groups.
        assert!(tools.contains("shell_exec"));
        assert!(tools.contains("read_file"));
        assert!(tools.contains("git_status"));
        assert!(tools.contains("web_search"));
        assert!(tools.contains("ha_get_state"));
        assert!(tools.contains("image_generation"));
        assert!(tools.contains("spawn_subagent"));
        assert!(tools.len() >= 55);
    }

    #[test]
    fn minimal_distribution_is_small() {
        let dist = builtin_distribution("minimal").unwrap();
        assert_eq!(dist.possible_tools().len(), 6);
        assert!(dist.possible_tools().contains("shell_exec"));
        assert!(dist.possible_tools().contains("read_file"));
        assert!(!dist.possible_tools().contains("web_search"));
    }

    #[test]
    fn safe_distribution_excludes_destructive() {
        let dist = builtin_distribution("safe").unwrap();
        let tools = dist.possible_tools();
        assert!(!tools.contains("shell_exec"));
        assert!(!tools.contains("write_file"));
        assert!(!tools.contains("patch"));
        assert!(tools.contains("read_file"));
        assert!(tools.contains("web_search"));
    }

    #[test]
    fn sample_is_deterministic_with_seed() {
        let dist = builtin_distribution("random").unwrap();
        let mut rng1 = rand::rngs::StdRng::seed_from_u64(42);
        let mut rng2 = rand::rngs::StdRng::seed_from_u64(42);
        let sample1 = dist.sample(&mut rng1);
        let sample2 = dist.sample(&mut rng2);
        assert_eq!(sample1, sample2);
    }

    #[test]
    fn sample_produces_different_results_with_different_seeds() {
        let dist = builtin_distribution("random").unwrap();
        let mut rng1 = rand::rngs::StdRng::seed_from_u64(1);
        let mut rng2 = rand::rngs::StdRng::seed_from_u64(999);
        let sample1 = dist.sample(&mut rng1);
        let sample2 = dist.sample(&mut rng2);
        // With 50% probability per tool, very unlikely to be identical.
        assert_ne!(sample1, sample2);
    }

    #[test]
    fn full_sample_includes_everything() {
        let dist = builtin_distribution("full").unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let sample = dist.sample(&mut rng);
        // All tools at 1.0 should always be included.
        assert_eq!(sample, dist.possible_tools());
    }

    #[test]
    fn all_builtin_names_resolve() {
        for name in builtin_distribution_names() {
            assert!(
                builtin_distribution(name).is_some(),
                "builtin distribution {name} should resolve"
            );
        }
    }

    #[test]
    fn unknown_distribution_returns_none() {
        assert!(builtin_distribution("nonexistent").is_none());
    }

    #[test]
    fn development_distribution_has_git() {
        let dist = builtin_distribution("development").unwrap();
        let tools = dist.possible_tools();
        assert!(tools.contains("git_status"));
        assert!(tools.contains("git_commit"));
        assert!(tools.contains("shell_exec"));
    }

    #[test]
    fn home_assistant_distribution_has_ha_tools() {
        let dist = builtin_distribution("home-assistant").unwrap();
        let tools = dist.possible_tools();
        assert!(tools.contains("ha_get_state"));
        assert!(tools.contains("ha_call_service"));
        assert!(tools.contains("ha_list_entities"));
        assert!(tools.contains("ha_list_services"));
    }
}
