//! Tool permission policy engine.
//!
//! Loads a JSON policy file and evaluates tool calls against allow/deny rules
//! before execution. Deny rules take priority over allow rules.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use glob::Pattern;
use serde::Deserialize;

/// Policy evaluation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Tool call is allowed.
    Allow,
    /// Tool call is denied with a reason.
    Deny(String),
}

/// A loaded and compiled tool policy.
#[derive(Debug, Clone)]
pub struct ToolPolicy {
    rules: HashMap<String, CompiledRule>,
    default: DefaultPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum DefaultPolicy {
    Allow,
    Deny,
}

/// Raw policy file format.
#[derive(Deserialize)]
struct PolicyFile {
    #[serde(default)]
    rules: Vec<RawRule>,
    #[serde(default = "default_allow")]
    default: DefaultPolicy,
}

fn default_allow() -> DefaultPolicy {
    DefaultPolicy::Allow
}

#[derive(Deserialize)]
struct RawRule {
    tool: String,
    /// Glob patterns for allowed argument values (matched against first string arg).
    #[serde(default)]
    allow: Vec<String>,
    /// Glob patterns for denied argument values.
    #[serde(default)]
    deny: Vec<String>,
    /// Glob patterns for allowed file paths (for tools that take a "path" argument).
    #[serde(default)]
    allow_paths: Vec<String>,
    /// Glob patterns for denied file paths.
    #[serde(default)]
    deny_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    allow_patterns: Vec<Pattern>,
    deny_patterns: Vec<Pattern>,
    allow_path_patterns: Vec<Pattern>,
    deny_path_patterns: Vec<Pattern>,
}

impl ToolPolicy {
    /// Create a policy that denies all tool calls. Used as a fail-closed
    /// fallback when a configured policy file cannot be loaded.
    pub fn deny_all() -> Self {
        Self {
            rules: HashMap::new(),
            default: DefaultPolicy::Deny,
        }
    }

    /// Load a policy from a JSON file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read policy file `{}`: {e}", path.display()))?;
        Self::from_json(&content)
    }

    /// Parse a policy from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let file: PolicyFile =
            serde_json::from_str(json).map_err(|e| format!("invalid policy JSON: {e}"))?;

        let mut rules = HashMap::new();
        for raw in file.rules {
            let compiled = CompiledRule {
                allow_patterns: compile_patterns(&raw.allow)?,
                deny_patterns: compile_patterns(&raw.deny)?,
                allow_path_patterns: compile_patterns(&raw.allow_paths)?,
                deny_path_patterns: compile_patterns(&raw.deny_paths)?,
            };
            if rules.contains_key(&raw.tool) {
                return Err(format!("duplicate rule for tool `{}`", raw.tool));
            }
            rules.insert(raw.tool, compiled);
        }

        Ok(Self {
            rules,
            default: file.default,
        })
    }

    /// Evaluate a tool call against the policy.
    ///
    /// `tool_name` is the tool being called.
    /// `arguments` is the tool's argument map (key -> value).
    pub fn evaluate(
        &self,
        tool_name: &str,
        arguments: &BTreeMap<String, String>,
    ) -> PolicyDecision {
        let Some(rule) = self.rules.get(tool_name) else {
            // No rule for this tool -- use default policy.
            return match self.default {
                DefaultPolicy::Allow => PolicyDecision::Allow,
                DefaultPolicy::Deny => PolicyDecision::Deny(format!(
                    "tool `{tool_name}` is not in the allow list (default policy: deny)"
                )),
            };
        };

        // Check path-based rules (for tools like write_file, read_file, etc.)
        if let Some(path_val) = arguments.get("path") {
            // Expand ~ to home directory.
            let expanded = expand_home(path_val);

            // Deny patterns take priority.
            for pattern in &rule.deny_path_patterns {
                if pattern.matches(&expanded) {
                    return PolicyDecision::Deny(format!(
                        "tool `{tool_name}` denied: path `{path_val}` matches deny pattern `{pattern}`"
                    ));
                }
            }

            // If allow patterns are specified, path must match at least one.
            if !rule.allow_path_patterns.is_empty() {
                let allowed = rule.allow_path_patterns.iter().any(|p| p.matches(&expanded));
                if !allowed {
                    return PolicyDecision::Deny(format!(
                        "tool `{tool_name}` denied: path `{path_val}` does not match any allow pattern"
                    ));
                }
            }
        }

        // Check command/argument-based rules (for tools like shell_exec).
        let cmd_checked =
            arguments.contains_key("command") || arguments.contains_key("cmd");
        if let Some(cmd) = arguments.get("command").or_else(|| arguments.get("cmd")) {
            // Deny patterns take priority.
            for pattern in &rule.deny_patterns {
                if pattern.matches(cmd) {
                    return PolicyDecision::Deny(format!(
                        "tool `{tool_name}` denied: argument matches deny pattern `{pattern}`"
                    ));
                }
            }

            // If allow patterns are specified, command must match at least one.
            if !rule.allow_patterns.is_empty() {
                let allowed = rule.allow_patterns.iter().any(|p| p.matches(cmd));
                if !allowed {
                    return PolicyDecision::Deny(format!(
                        "tool `{tool_name}` denied: command does not match any allow pattern"
                    ));
                }
            }
        }

        // If no known key (command/cmd) matched, check ALL argument values
        // against deny patterns. This runs regardless of whether allow patterns
        // exist — a deny-only rule must still block matching values.
        if !cmd_checked && !arguments.is_empty() {
            for val in arguments.values() {
                for pattern in &rule.deny_patterns {
                    if pattern.matches(val) {
                        return PolicyDecision::Deny(format!(
                            "tool `{tool_name}` denied: argument value matches deny pattern `{pattern}`"
                        ));
                    }
                }
            }
        }

        // If allow patterns exist but no known key matched, every argument
        // value must be checked against the allow list.
        if !cmd_checked && !rule.allow_patterns.is_empty() && !arguments.is_empty() {
            let any_allowed = arguments.values().any(|val| {
                rule.allow_patterns.iter().any(|p| p.matches(val))
            });
            if !any_allowed {
                return PolicyDecision::Deny(format!(
                    "tool `{tool_name}` denied: no argument matches any allow pattern"
                ));
            }
        }

        PolicyDecision::Allow
    }

}

fn compile_patterns(raw: &[String]) -> Result<Vec<Pattern>, String> {
    raw.iter()
        .map(|s| {
            let expanded = expand_home(s);
            Pattern::new(&expanded).map_err(|e| format!("invalid glob pattern `{s}`: {e}"))
        })
        .collect()
}

pub(crate) fn expand_home(path: &str) -> String {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), &path[1..]);
        }
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn default_allow_passes_unknown_tools() {
        let policy = ToolPolicy::from_json(r#"{"rules": [], "default": "allow"}"#).unwrap();
        assert_eq!(
            policy.evaluate("anything", &BTreeMap::new()),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn default_deny_blocks_unknown_tools() {
        let policy = ToolPolicy::from_json(r#"{"rules": [], "default": "deny"}"#).unwrap();
        match policy.evaluate("anything", &BTreeMap::new()) {
            PolicyDecision::Deny(reason) => assert!(reason.contains("not in the allow list")),
            PolicyDecision::Allow => panic!("should deny"),
        }
    }

    #[test]
    fn shell_allow_pattern_passes() {
        let policy = ToolPolicy::from_json(
            r#"{
            "rules": [{"tool": "shell_exec", "allow": ["git *", "cargo *"], "deny": []}],
            "default": "allow"
        }"#,
        )
        .unwrap();
        assert_eq!(
            policy.evaluate("shell_exec", &args(&[("command", "git status")])),
            PolicyDecision::Allow
        );
        assert_eq!(
            policy.evaluate("shell_exec", &args(&[("command", "cargo build")])),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn shell_deny_pattern_blocks() {
        let policy = ToolPolicy::from_json(
            r#"{
            "rules": [{"tool": "shell_exec", "allow": ["*"], "deny": ["rm -rf *", "sudo *"]}],
            "default": "allow"
        }"#,
        )
        .unwrap();
        match policy.evaluate("shell_exec", &args(&[("command", "rm -rf /")])) {
            PolicyDecision::Deny(reason) => assert!(reason.contains("deny pattern")),
            PolicyDecision::Allow => panic!("should deny rm -rf"),
        }
        match policy.evaluate("shell_exec", &args(&[("command", "sudo reboot")])) {
            PolicyDecision::Deny(reason) => assert!(reason.contains("deny pattern")),
            PolicyDecision::Allow => panic!("should deny sudo"),
        }
    }

    #[test]
    fn shell_unmatched_allow_blocks() {
        let policy = ToolPolicy::from_json(
            r#"{
            "rules": [{"tool": "shell_exec", "allow": ["git *", "cargo *"], "deny": []}],
            "default": "allow"
        }"#,
        )
        .unwrap();
        match policy.evaluate("shell_exec", &args(&[("command", "python3 exploit.py")])) {
            PolicyDecision::Deny(reason) => assert!(reason.contains("does not match any allow")),
            PolicyDecision::Allow => panic!("should deny python3"),
        }
    }

    #[test]
    fn deny_takes_priority_over_allow() {
        let policy = ToolPolicy::from_json(
            r#"{
            "rules": [{"tool": "shell_exec", "allow": ["*"], "deny": ["rm -rf *"]}],
            "default": "allow"
        }"#,
        )
        .unwrap();
        match policy.evaluate("shell_exec", &args(&[("command", "rm -rf /tmp")])) {
            PolicyDecision::Deny(_) => {}
            PolicyDecision::Allow => panic!("deny should take priority"),
        }
    }

    #[test]
    fn path_deny_blocks_sensitive_paths() {
        let policy = ToolPolicy::from_json(
            r#"{
            "rules": [{
                "tool": "write_file",
                "allow_paths": ["**"],
                "deny_paths": ["/etc/**"]
            }],
            "default": "allow"
        }"#,
        )
        .unwrap();
        match policy.evaluate("write_file", &args(&[("path", "/etc/passwd")])) {
            PolicyDecision::Deny(reason) => assert!(reason.contains("deny pattern")),
            PolicyDecision::Allow => panic!("should deny /etc write"),
        }
    }

    #[test]
    fn path_allow_restricts_to_workspace() {
        let policy = ToolPolicy::from_json(
            r#"{
            "rules": [{
                "tool": "write_file",
                "allow_paths": ["src/**", "tests/**"],
                "deny_paths": []
            }],
            "default": "allow"
        }"#,
        )
        .unwrap();
        assert_eq!(
            policy.evaluate("write_file", &args(&[("path", "src/main.rs")])),
            PolicyDecision::Allow
        );
        match policy.evaluate("write_file", &args(&[("path", "/tmp/evil.sh")])) {
            PolicyDecision::Deny(reason) => assert!(reason.contains("does not match any allow")),
            PolicyDecision::Allow => panic!("should deny out-of-workspace path"),
        }
    }

    #[test]
    fn tool_without_args_matches_default() {
        let policy = ToolPolicy::from_json(
            r#"{
            "rules": [{"tool": "shell_exec", "allow": ["git *"], "deny": []}],
            "default": "allow"
        }"#,
        )
        .unwrap();
        // shell_exec with no command arg -- no patterns match, but no deny either.
        assert_eq!(
            policy.evaluate("shell_exec", &BTreeMap::new()),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn invalid_json_returns_error() {
        assert!(ToolPolicy::from_json("not json").is_err());
    }

    #[test]
    fn invalid_glob_returns_error() {
        assert!(ToolPolicy::from_json(
            r#"{
            "rules": [{"tool": "x", "allow": ["[invalid"]}]
        }"#
        )
        .is_err());
    }

    #[test]
    fn empty_policy_file() {
        let policy = ToolPolicy::from_json(r#"{}"#).unwrap();
        assert_eq!(
            policy.evaluate("anything", &BTreeMap::new()),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn tilde_patterns_match_expanded_paths() {
        // Patterns with ~ should be expanded at compile time and match
        // paths that are also tilde-expanded at evaluation time.
        let home = dirs::home_dir().expect("home dir required for this test");
        let home_str = home.display().to_string();
        let policy = ToolPolicy::from_json(
            r#"{
            "rules": [{
                "tool": "write_file",
                "allow_paths": ["~/projects/**"],
                "deny_paths": ["~/.ssh/**"]
            }],
            "default": "allow"
        }"#,
        )
        .unwrap();

        // Deny pattern: ~/.ssh/id_rsa should be denied (input uses expanded path).
        let expanded_ssh = format!("{home_str}/.ssh/id_rsa");
        match policy.evaluate("write_file", &args(&[("path", &expanded_ssh)])) {
            PolicyDecision::Deny(reason) => assert!(reason.contains("deny pattern")),
            PolicyDecision::Allow => panic!("should deny ~/.ssh path"),
        }

        // Allow pattern: ~/projects/foo.rs should be allowed (input uses ~).
        assert_eq!(
            policy.evaluate("write_file", &args(&[("path", "~/projects/foo.rs")])),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn duplicate_rules_produce_error() {
        let result = ToolPolicy::from_json(
            r#"{
            "rules": [
                {"tool": "shell_exec", "allow": ["*"], "deny": []},
                {"tool": "shell_exec", "allow": ["git *"], "deny": []}
            ],
            "default": "allow"
        }"#,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("duplicate rule for tool"));
    }

    #[test]
    fn deny_all_blocks_everything() {
        let policy = ToolPolicy::deny_all();
        match policy.evaluate("shell_exec", &args(&[("command", "ls")])) {
            PolicyDecision::Deny(reason) => {
                assert!(reason.contains("not in the allow list"));
                assert!(reason.contains("default policy: deny"));
            }
            PolicyDecision::Allow => panic!("deny_all should block everything"),
        }
        match policy.evaluate("read_file", &args(&[("path", "/tmp/test")])) {
            PolicyDecision::Deny(_) => {}
            PolicyDecision::Allow => panic!("deny_all should block everything"),
        }
        match policy.evaluate("any_tool", &BTreeMap::new()) {
            PolicyDecision::Deny(_) => {}
            PolicyDecision::Allow => panic!("deny_all should block everything"),
        }
    }

    #[test]
    fn deny_only_rule_blocks_non_standard_argument() {
        // A rule with ONLY deny patterns (no allow patterns) must still block
        // matching values via non-standard argument keys.
        let policy = ToolPolicy::from_json(
            r#"{
            "rules": [{"tool": "custom_exec", "deny": ["evil_*"]}],
            "default": "allow"
        }"#,
        )
        .unwrap();

        // Should be denied — matches the deny pattern.
        let mut deny_args = BTreeMap::new();
        deny_args.insert("input".to_string(), "evil_payload".to_string());
        match policy.evaluate("custom_exec", &deny_args) {
            PolicyDecision::Deny(reason) => assert!(reason.contains("deny pattern")),
            PolicyDecision::Allow => panic!("deny-only rule should block matching value"),
        }

        // Should be allowed — no deny match and no allow patterns to enforce.
        let mut safe_args = BTreeMap::new();
        safe_args.insert("input".to_string(), "safe_value".to_string());
        assert_eq!(
            policy.evaluate("custom_exec", &safe_args),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn allow_patterns_check_unrecognized_arg_keys() {
        // If a tool uses non-standard argument keys (not "command"/"cmd"/"path"),
        // allow patterns should still be checked against all argument values.
        let policy = ToolPolicy::from_json(
            r#"{
            "rules": [{"tool": "custom_tool", "allow": ["safe_*"], "deny": ["evil_*"]}],
            "default": "allow"
        }"#,
        )
        .unwrap();

        // "input" is not a recognized key, but its value should be checked.
        assert_eq!(
            policy.evaluate("custom_tool", &args(&[("input", "safe_value")])),
            PolicyDecision::Allow
        );
        match policy.evaluate("custom_tool", &args(&[("input", "evil_value")])) {
            PolicyDecision::Deny(reason) => assert!(reason.contains("deny pattern")),
            PolicyDecision::Allow => panic!("should deny evil_ value"),
        }
        match policy.evaluate("custom_tool", &args(&[("input", "unknown_value")])) {
            PolicyDecision::Deny(reason) => {
                assert!(reason.contains("no argument matches any allow pattern"))
            }
            PolicyDecision::Allow => panic!("should deny unmatched value"),
        }
    }
}
