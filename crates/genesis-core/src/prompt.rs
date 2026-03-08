use genesis_types::ToolDefinition;

const DEFAULT_AGENT_NAME: &str = "Eve";
const DEFAULT_AGENT_IDENTITY: &str = "You are Eve, an intelligent AI agent built on the Genesis framework. You are helpful, thoughtful, and precise. You have access to tools that extend your capabilities.";

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
    let mut parts = Vec::new();

    // Identity section
    parts.push(
        custom_identity
            .unwrap_or(DEFAULT_AGENT_IDENTITY)
            .to_owned(),
    );

    // Profile
    parts.push(format!("Current profile: {profile}"));

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
}
