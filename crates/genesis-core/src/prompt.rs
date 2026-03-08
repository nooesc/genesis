use std::path::Path;

use genesis_types::ToolDefinition;

const DEFAULT_AGENT_NAME: &str = "Eve";
const DEFAULT_AGENT_IDENTITY: &str = "You are Eve, an intelligent AI agent built on the Genesis framework. You are helpful, thoughtful, and precise. You have access to tools that extend your capabilities.";

/// Well-known context file paths, checked in order. The first file found wins.
const CONTEXT_FILE_CANDIDATES: &[&str] = &[
    ".genesis/context.md",
    ".genesis/instructions.md",
    "genesis.md",
    ".genesis.md",
];

/// Build a system prompt for the agent from the current context.
pub fn build_system_prompt(
    profile: &str,
    tools: &[ToolDefinition],
    custom_identity: Option<&str>,
) -> String {
    build_system_prompt_with_skills(profile, tools, custom_identity, None)
}

/// Build a system prompt with optional skills context.
pub fn build_system_prompt_with_skills(
    profile: &str,
    tools: &[ToolDefinition],
    custom_identity: Option<&str>,
    skills_section: Option<&str>,
) -> String {
    build_system_prompt_full(profile, tools, custom_identity, skills_section, None)
}

/// Build a system prompt with all optional sections.
pub fn build_system_prompt_full(
    profile: &str,
    tools: &[ToolDefinition],
    custom_identity: Option<&str>,
    skills_section: Option<&str>,
    user_model_section: Option<&str>,
) -> String {
    build_system_prompt_complete(
        profile,
        tools,
        custom_identity,
        skills_section,
        user_model_section,
        None,
    )
}

/// Build a system prompt with all optional sections including project context.
pub fn build_system_prompt_complete(
    profile: &str,
    tools: &[ToolDefinition],
    custom_identity: Option<&str>,
    skills_section: Option<&str>,
    user_model_section: Option<&str>,
    context_section: Option<&str>,
) -> String {
    let mut parts = Vec::new();

    // Identity section
    parts.push(
        custom_identity
            .unwrap_or(DEFAULT_AGENT_IDENTITY)
            .to_owned(),
    );

    // Profile
    parts.push(format!("Current profile: {profile}"));

    // Project context files
    if let Some(context) = context_section {
        parts.push(format!(
            "## Project Context\n\nThe following instructions come from the project's context file. Follow them carefully.\n\n{context}"
        ));
    }

    // User model section (what the agent knows about the user)
    if let Some(user_model) = user_model_section {
        parts.push(format!(
            "## What you know about the user\n\nUse these observations to personalize your responses. Update them with user_observe when you learn something new.\n\n{user_model}"
        ));
    }

    // Tool listing
    if !tools.is_empty() {
        let mut tool_section = String::from("Available tools:");
        for tool in tools {
            tool_section.push_str(&format!("\n- {}: {}", tool.name, tool.description));
        }
        parts.push(tool_section);
    }

    // Skills section
    if let Some(skills) = skills_section {
        parts.push(skills.to_owned());
    }

    // Skill instruction
    parts.push(
        "You can learn new skills using the skill_create tool. After completing a complex multi-step task successfully, consider saving the procedure as a skill for future reuse.".to_owned(),
    );

    parts.join("\n\n")
}

/// Load project context from well-known file locations relative to a directory.
///
/// Checks `.genesis/context.md`, `.genesis/instructions.md`, `genesis.md`,
/// and `.genesis.md` in order. Returns the contents of the first file found,
/// or `None` if none exist.
pub fn load_context_file(project_dir: &Path) -> Option<String> {
    for candidate in CONTEXT_FILE_CANDIDATES {
        let path = project_dir.join(candidate);
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if !contents.trim().is_empty() {
                return Some(contents);
            }
        }
    }
    None
}

/// Returns the default agent name.
pub fn agent_name() -> &'static str {
    DEFAULT_AGENT_NAME
}

#[cfg(test)]
mod tests {
    use super::*;
    use genesis_types::ToolDefinition;

    #[test]
    fn default_prompt_includes_eve_identity() {
        let prompt = build_system_prompt("default", &[], None);
        assert!(prompt.contains("Eve"));
        assert!(prompt.contains("Genesis"));
    }

    #[test]
    fn prompt_includes_profile_name() {
        let prompt = build_system_prompt("operator", &[], None);
        assert!(prompt.contains("Current profile: operator"));
    }

    #[test]
    fn prompt_lists_available_tools() {
        let tools = vec![
            ToolDefinition {
                name: "echo".to_owned(),
                description: "Echoes a message".to_owned(),
                parameters: None,
            },
            ToolDefinition {
                name: "search".to_owned(),
                description: "Searches things".to_owned(),
                parameters: None,
            },
        ];

        let prompt = build_system_prompt("default", &tools, None);
        assert!(prompt.contains("Available tools:"));
        assert!(prompt.contains("- echo: Echoes a message"));
        assert!(prompt.contains("- search: Searches things"));
    }

    #[test]
    fn custom_identity_replaces_default() {
        let prompt = build_system_prompt(
            "default",
            &[],
            Some("You are a custom agent."),
        );
        assert!(prompt.contains("You are a custom agent."));
        assert!(!prompt.contains("Eve"));
    }

    #[test]
    fn agent_name_returns_eve() {
        assert_eq!(agent_name(), "Eve");
    }

    #[test]
    fn prompt_with_skills_includes_skill_section() {
        let skills = "## Available Skills\n\n### deploy\n**Description:** Deploy app\n";
        let prompt = build_system_prompt_with_skills("operator", &[], None, Some(skills));
        assert!(prompt.contains("## Available Skills"));
        assert!(prompt.contains("### deploy"));
    }

    #[test]
    fn prompt_with_skills_includes_learning_instruction() {
        let prompt = build_system_prompt_with_skills("default", &[], None, None);
        assert!(prompt.contains("skill_create"));
        assert!(prompt.contains("learn new skills"));
    }

    #[test]
    fn prompt_with_user_model_includes_observations() {
        let user_model = "- **prefers_rust**: User strongly prefers Rust (confidence: 80%, 4 observations)";
        let prompt = build_system_prompt_full("default", &[], None, None, Some(user_model));
        assert!(prompt.contains("What you know about the user"));
        assert!(prompt.contains("prefers_rust"));
        assert!(prompt.contains("personalize"));
    }

    #[test]
    fn prompt_without_user_model_omits_section() {
        let prompt = build_system_prompt_full("default", &[], None, None, None);
        assert!(!prompt.contains("What you know about the user"));
    }

    #[test]
    fn prompt_with_context_section_includes_project_context() {
        let context = "Always use tabs for indentation.\nPrefer async/await over callbacks.";
        let prompt = build_system_prompt_complete("default", &[], None, None, None, Some(context));
        assert!(prompt.contains("## Project Context"));
        assert!(prompt.contains("Always use tabs"));
        assert!(prompt.contains("Follow them carefully"));
    }

    #[test]
    fn prompt_without_context_omits_section() {
        let prompt = build_system_prompt_complete("default", &[], None, None, None, None);
        assert!(!prompt.contains("Project Context"));
    }

    #[test]
    fn load_context_file_finds_genesis_md() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("genesis.md"), "Use Rust for everything.")
            .expect("write");
        let context = load_context_file(dir.path());
        assert_eq!(context.as_deref(), Some("Use Rust for everything."));
    }

    #[test]
    fn load_context_file_prefers_dot_genesis_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".genesis")).expect("mkdir");
        std::fs::write(
            dir.path().join(".genesis/context.md"),
            "From .genesis/context.md",
        )
        .expect("write context");
        std::fs::write(dir.path().join("genesis.md"), "From genesis.md").expect("write root");

        let context = load_context_file(dir.path());
        assert_eq!(context.as_deref(), Some("From .genesis/context.md"));
    }

    #[test]
    fn load_context_file_returns_none_when_no_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_context_file(dir.path()).is_none());
    }

    #[test]
    fn load_context_file_skips_empty_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("genesis.md"), "   \n  ").expect("write");
        assert!(load_context_file(dir.path()).is_none());
    }
}
