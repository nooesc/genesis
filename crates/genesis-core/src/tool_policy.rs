//! Tool permission scoping — JSON-based policy engine.
//!
//! Evaluates tool call arguments against allow/deny rules defined in a
//! policy file. Inspired by Progent (arxiv 2504.11703) which demonstrated
//! 0% attack success rate on AgentDojo/ASB benchmarks.
//!
//! ## Policy format
//!
//! ```json
//! {
//!   "default": "allow",
//!   "rules": [
//!     {
//!       "tool": "shell_exec",
//!       "allow": { "command_patterns": ["git *", "cargo *"] },
//!       "deny": { "command_patterns": ["rm -rf *", "sudo *"] }
//!     },
//!     {
//!       "tool": "write_file",
//!       "allow": { "paths": ["${workspace}/**"] },
//!       "deny": { "paths": ["~/.ssh/**", "~/.genesis/auth*"] }
//!     }
//!   ]
//! }
//! ```

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use tracing::debug;

/// A loaded tool policy with compiled rules.
#[derive(Debug, Clone)]
pub struct ToolPolicy {
    /// Default action when no rule matches: "allow" or "deny".
    pub default_action: PolicyAction,
    /// Rules indexed by tool name for fast lookup.
    rules: HashMap<String, Vec<PolicyRule>>,
}

/// Policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    Allow,
    Deny,
}

/// Result of a policy evaluation.
#[derive(Debug, Clone)]
pub struct PolicyResult {
    pub action: PolicyAction,
    pub reason: Option<String>,
}

impl PolicyResult {
    fn allow() -> Self {
        Self {
            action: PolicyAction::Allow,
            reason: None,
        }
    }

    fn deny(reason: impl Into<String>) -> Self {
        Self {
            action: PolicyAction::Deny,
            reason: Some(reason.into()),
        }
    }
}

/// A single policy rule for a tool.
#[derive(Debug, Clone)]
struct PolicyRule {
    /// Patterns that allow the tool call (checked before deny).
    allow_patterns: RulePatterns,
    /// Patterns that deny the tool call (checked after allow).
    deny_patterns: RulePatterns,
}

/// Patterns to match against tool arguments.
#[derive(Debug, Clone, Default)]
struct RulePatterns {
    /// Glob patterns for "command" argument (shell_exec, docker_exec, ssh_exec).
    command_patterns: Vec<String>,
    /// Glob patterns for "path" argument (read_file, write_file, patch, list_dir).
    path_patterns: Vec<String>,
    /// Glob patterns for "url" argument (web_request, browse).
    url_patterns: Vec<String>,
}

/// Raw policy file format (deserialized from JSON).
#[derive(Debug, Deserialize)]
struct PolicyFile {
    #[serde(default = "default_allow")]
    default: String,
    #[serde(default)]
    rules: Vec<PolicyRuleFile>,
}

#[derive(Debug, Deserialize)]
struct PolicyRuleFile {
    tool: String,
    #[serde(default)]
    allow: Option<PatternSet>,
    #[serde(default)]
    deny: Option<PatternSet>,
}

#[derive(Debug, Deserialize, Default)]
struct PatternSet {
    #[serde(default)]
    command_patterns: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    url_patterns: Vec<String>,
}

fn default_allow() -> String {
    "allow".to_owned()
}

impl ToolPolicy {
    /// Load a tool policy from a JSON file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read policy file {}: {e}", path.display()))?;
        Self::parse(&content)
    }

    /// Parse a tool policy from JSON content.
    pub fn parse(json: &str) -> Result<Self, String> {
        Self::parse_with_vars(json, &HashMap::new())
    }

    /// Parse with variable substitution (e.g., `${workspace}` → actual path).
    pub fn parse_with_vars(
        json: &str,
        vars: &HashMap<String, String>,
    ) -> Result<Self, String> {
        let file: PolicyFile =
            serde_json::from_str(json).map_err(|e| format!("invalid policy JSON: {e}"))?;

        let default_action = match file.default.as_str() {
            "deny" => PolicyAction::Deny,
            _ => PolicyAction::Allow,
        };

        let mut rules: HashMap<String, Vec<PolicyRule>> = HashMap::new();
        for rule_file in file.rules {
            let allow = rule_file.allow.unwrap_or_default();
            let deny = rule_file.deny.unwrap_or_default();

            let rule = PolicyRule {
                allow_patterns: RulePatterns {
                    command_patterns: expand_vars(&allow.command_patterns, vars),
                    path_patterns: expand_vars(&allow.paths, vars),
                    url_patterns: expand_vars(&allow.url_patterns, vars),
                },
                deny_patterns: RulePatterns {
                    command_patterns: expand_vars(&deny.command_patterns, vars),
                    path_patterns: expand_vars(&deny.paths, vars),
                    url_patterns: expand_vars(&deny.url_patterns, vars),
                },
            };
            rules.entry(rule_file.tool).or_default().push(rule);
        }

        Ok(Self {
            default_action,
            rules,
        })
    }

    /// Check whether a tool call is allowed by the policy.
    pub fn check(
        &self,
        tool_name: &str,
        arguments: &std::collections::BTreeMap<String, String>,
    ) -> PolicyResult {
        let rules = match self.rules.get(tool_name) {
            Some(rules) => rules,
            None => {
                // No rules for this tool — use default action.
                return match self.default_action {
                    PolicyAction::Allow => PolicyResult::allow(),
                    PolicyAction::Deny => PolicyResult::deny(format!(
                        "tool `{tool_name}` is not permitted by policy (no matching rule, default: deny)"
                    )),
                };
            }
        };

        // Extract relevant arguments for matching.
        let command = arguments.get("command");
        let path = arguments.get("path");
        let url = arguments.get("url");

        for rule in rules {
            // Check deny patterns first — deny takes precedence over allow.
            if let Some(cmd) = command {
                if matches_any(cmd, &rule.deny_patterns.command_patterns) {
                    return PolicyResult::deny(format!(
                        "command `{cmd}` is denied by policy for tool `{tool_name}`"
                    ));
                }
            }
            if let Some(p) = path {
                if matches_any(p, &rule.deny_patterns.path_patterns) {
                    return PolicyResult::deny(format!(
                        "path `{p}` is denied by policy for tool `{tool_name}`"
                    ));
                }
            }
            if let Some(u) = url {
                if matches_any(u, &rule.deny_patterns.url_patterns) {
                    return PolicyResult::deny(format!(
                        "url `{u}` is denied by policy for tool `{tool_name}`"
                    ));
                }
            }

            // Check allow patterns — if allow patterns exist and none match, deny.
            let has_allow = !rule.allow_patterns.command_patterns.is_empty()
                || !rule.allow_patterns.path_patterns.is_empty()
                || !rule.allow_patterns.url_patterns.is_empty();

            if has_allow {
                let allowed = (command.is_none()
                    || rule.allow_patterns.command_patterns.is_empty()
                    || command
                        .map(|c| matches_any(c, &rule.allow_patterns.command_patterns))
                        .unwrap_or(true))
                    && (path.is_none()
                        || rule.allow_patterns.path_patterns.is_empty()
                        || path
                            .map(|p| matches_any(p, &rule.allow_patterns.path_patterns))
                            .unwrap_or(true))
                    && (url.is_none()
                        || rule.allow_patterns.url_patterns.is_empty()
                        || url
                            .map(|u| matches_any(u, &rule.allow_patterns.url_patterns))
                            .unwrap_or(true));

                if !allowed {
                    return PolicyResult::deny(format!(
                        "tool `{tool_name}` call does not match any allow pattern"
                    ));
                }
            }
        }

        debug!(tool = tool_name, "tool call allowed by policy");
        PolicyResult::allow()
    }
}

/// Expand `${var}` references in patterns.
fn expand_vars(patterns: &[String], vars: &HashMap<String, String>) -> Vec<String> {
    patterns
        .iter()
        .map(|p| {
            let mut result = p.clone();
            for (key, value) in vars {
                result = result.replace(&format!("${{{key}}}"), value);
            }
            // Also expand ~ to home directory.
            if result.starts_with('~') {
                if let Some(home) = dirs::home_dir() {
                    result = result.replacen('~', &home.display().to_string(), 1);
                }
            }
            result
        })
        .collect()
}

/// Check if a value matches any glob pattern in the list.
fn matches_any(value: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if glob_match(pattern, value) {
            return true;
        }
    }
    false
}

/// Simple glob matching supporting `*` (any chars) and `**` (recursive).
fn glob_match(pattern: &str, value: &str) -> bool {
    // Use the `glob` crate's pattern matching for paths.
    if let Ok(glob_pattern) = glob::Pattern::new(pattern) {
        return glob_pattern.matches(value);
    }
    // Fallback: exact match.
    pattern == value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parse_basic_policy() {
        let json = r#"{
            "default": "allow",
            "rules": [
                {
                    "tool": "shell_exec",
                    "allow": { "command_patterns": ["git *", "cargo *"] },
                    "deny": { "command_patterns": ["rm -rf *", "sudo *"] }
                }
            ]
        }"#;
        let policy = ToolPolicy::parse(json).unwrap();
        assert_eq!(policy.default_action, PolicyAction::Allow);
        assert!(policy.rules.contains_key("shell_exec"));
    }

    #[test]
    fn allowed_command_passes() {
        let json = r#"{
            "default": "deny",
            "rules": [{
                "tool": "shell_exec",
                "allow": { "command_patterns": ["git *", "cargo *"] }
            }]
        }"#;
        let policy = ToolPolicy::parse(json).unwrap();

        let result = policy.check("shell_exec", &make_args(&[("command", "git status")]));
        assert_eq!(result.action, PolicyAction::Allow);

        let result = policy.check("shell_exec", &make_args(&[("command", "cargo test")]));
        assert_eq!(result.action, PolicyAction::Allow);
    }

    #[test]
    fn denied_command_blocked() {
        let json = r#"{
            "default": "allow",
            "rules": [{
                "tool": "shell_exec",
                "deny": { "command_patterns": ["rm -rf *", "sudo *"] }
            }]
        }"#;
        let policy = ToolPolicy::parse(json).unwrap();

        let result = policy.check("shell_exec", &make_args(&[("command", "rm -rf /")]));
        assert_eq!(result.action, PolicyAction::Deny);
        assert!(result.reason.unwrap().contains("rm -rf /"));

        let result = policy.check("shell_exec", &make_args(&[("command", "sudo reboot")]));
        assert_eq!(result.action, PolicyAction::Deny);
    }

    #[test]
    fn path_restriction_enforced() {
        let json = r#"{
            "default": "allow",
            "rules": [{
                "tool": "write_file",
                "deny": { "paths": ["/etc/**", "/root/**"] }
            }]
        }"#;
        let policy = ToolPolicy::parse(json).unwrap();

        let result = policy.check(
            "write_file",
            &make_args(&[("path", "/etc/passwd"), ("content", "test")]),
        );
        assert_eq!(result.action, PolicyAction::Deny);

        let result = policy.check(
            "write_file",
            &make_args(&[("path", "/home/user/file.txt"), ("content", "test")]),
        );
        assert_eq!(result.action, PolicyAction::Allow);
    }

    #[test]
    fn default_deny_blocks_unknown_tools() {
        let json = r#"{ "default": "deny", "rules": [] }"#;
        let policy = ToolPolicy::parse(json).unwrap();

        let result = policy.check("unknown_tool", &BTreeMap::new());
        assert_eq!(result.action, PolicyAction::Deny);
    }

    #[test]
    fn default_allow_passes_unknown_tools() {
        let json = r#"{ "default": "allow", "rules": [] }"#;
        let policy = ToolPolicy::parse(json).unwrap();

        let result = policy.check("unknown_tool", &BTreeMap::new());
        assert_eq!(result.action, PolicyAction::Allow);
    }

    #[test]
    fn deny_takes_precedence_over_allow() {
        let json = r#"{
            "default": "allow",
            "rules": [{
                "tool": "shell_exec",
                "allow": { "command_patterns": ["*"] },
                "deny": { "command_patterns": ["sudo *"] }
            }]
        }"#;
        let policy = ToolPolicy::parse(json).unwrap();

        // Matches both allow (*) and deny (sudo *) — deny wins.
        let result = policy.check("shell_exec", &make_args(&[("command", "sudo rm -rf /")]));
        assert_eq!(result.action, PolicyAction::Deny);

        // Matches allow but not deny — allow wins.
        let result = policy.check("shell_exec", &make_args(&[("command", "ls")]));
        assert_eq!(result.action, PolicyAction::Allow);
    }

    #[test]
    fn variable_expansion() {
        let mut vars = HashMap::new();
        vars.insert("workspace".to_owned(), "/home/user/project".to_owned());

        let json = r#"{
            "default": "deny",
            "rules": [{
                "tool": "write_file",
                "allow": { "paths": ["${workspace}/**"] }
            }]
        }"#;
        let policy = ToolPolicy::parse_with_vars(json, &vars).unwrap();

        let result = policy.check(
            "write_file",
            &make_args(&[("path", "/home/user/project/src/main.rs"), ("content", "x")]),
        );
        assert_eq!(result.action, PolicyAction::Allow);

        let result = policy.check(
            "write_file",
            &make_args(&[("path", "/etc/passwd"), ("content", "x")]),
        );
        assert_eq!(result.action, PolicyAction::Deny);
    }

    #[test]
    fn command_not_in_allow_list_denied() {
        let json = r#"{
            "default": "allow",
            "rules": [{
                "tool": "shell_exec",
                "allow": { "command_patterns": ["git *", "cargo *"] }
            }]
        }"#;
        let policy = ToolPolicy::parse(json).unwrap();

        let result = policy.check("shell_exec", &make_args(&[("command", "python script.py")]));
        assert_eq!(result.action, PolicyAction::Deny);
    }

    #[test]
    fn url_restriction() {
        let json = r#"{
            "default": "allow",
            "rules": [{
                "tool": "web_request",
                "deny": { "url_patterns": ["*evil.com*", "*malware*"] }
            }]
        }"#;
        let policy = ToolPolicy::parse(json).unwrap();

        let result = policy.check(
            "web_request",
            &make_args(&[("url", "https://evil.com/payload")]),
        );
        assert_eq!(result.action, PolicyAction::Deny);

        let result = policy.check(
            "web_request",
            &make_args(&[("url", "https://docs.rs/serde")]),
        );
        assert_eq!(result.action, PolicyAction::Allow);
    }
}
