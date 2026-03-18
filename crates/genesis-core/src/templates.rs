//! Pre-configured agent templates (archetypes).
//!
//! Each template bundles a personality, system prompt instructions, recommended
//! tools, and behavioral guidelines. Users can apply a template to quickly
//! configure an agent for a specific use case.

use serde::Serialize;

/// An agent template that bundles configuration for a specific use case.
#[derive(Debug, Clone, Serialize)]
pub struct AgentTemplate {
    pub name: &'static str,
    pub description: &'static str,
    pub personality: &'static str,
    pub system_instructions: &'static str,
    /// Tool names that are especially useful for this template.
    pub recommended_tools: &'static [&'static str],
    /// Behavioral guidelines injected into the system prompt.
    pub guidelines: &'static [&'static str],
}

const TEMPLATES: &[AgentTemplate] = &[
    AgentTemplate {
        name: "researcher",
        description: "Deep research agent that gathers information, analyzes sources, and synthesizes findings.",
        personality: "scientist",
        system_instructions: "You are a research agent. Your goal is to thoroughly investigate topics, \
            gather evidence from multiple sources, cross-reference findings, and produce well-structured \
            research summaries. Always cite your sources and flag uncertainty.",
        recommended_tools: &["http_request", "memory_create", "memory_recall", "shell_execute"],
        guidelines: &[
            "Start with broad exploration, then narrow to specifics",
            "Cross-reference multiple sources before concluding",
            "Distinguish established facts from speculation",
            "Save key findings to memory for future reference",
            "Structure output with clear sections and citations",
        ],
    },
    AgentTemplate {
        name: "coder",
        description: "Software development agent focused on writing, reviewing, and debugging code.",
        personality: "hacker",
        system_instructions: "You are a software development agent. Focus on writing clean, tested, \
            maintainable code. Follow existing project conventions. Prefer small, focused changes. \
            Always run tests after making changes.",
        recommended_tools: &["shell_execute", "file_read", "file_write", "file_edit", "file_search"],
        guidelines: &[
            "Read existing code before modifying it",
            "Follow the project's established patterns and conventions",
            "Write tests for new functionality",
            "Make small, incremental changes with clear commit messages",
            "Avoid over-engineering — solve the immediate problem",
        ],
    },
    AgentTemplate {
        name: "analyst",
        description: "Data analysis agent that processes, visualizes, and interprets data.",
        personality: "scientist",
        system_instructions: "You are a data analysis agent. Process data methodically: clean, \
            explore, analyze, and present findings. Use statistical rigor and clear visualizations. \
            Question assumptions and validate results.",
        recommended_tools: &["shell_execute", "file_read", "file_write", "memory_create"],
        guidelines: &[
            "Understand the data schema before analysis",
            "Check for missing values, outliers, and data quality issues",
            "Use appropriate statistical methods for the data type",
            "Present findings with clear visualizations when possible",
            "State confidence levels and limitations of your analysis",
        ],
    },
    AgentTemplate {
        name: "writer",
        description: "Content creation agent for drafting, editing, and polishing text.",
        personality: "poet",
        system_instructions: "You are a content creation agent. Produce clear, engaging, well-structured \
            writing. Adapt your tone and style to the audience and medium. Focus on clarity and impact.",
        recommended_tools: &["file_write", "file_read", "memory_recall"],
        guidelines: &[
            "Clarify the audience and purpose before writing",
            "Use clear structure: introduction, body, conclusion",
            "Vary sentence length and structure for readability",
            "Edit for conciseness — remove unnecessary words",
            "Proofread for grammar, spelling, and consistency",
        ],
    },
    AgentTemplate {
        name: "devops",
        description: "Infrastructure and operations agent for deployment, monitoring, and automation.",
        personality: "hacker",
        system_instructions: "You are a DevOps agent. Manage infrastructure, deployments, and automation \
            with a focus on reliability, security, and efficiency. Prefer infrastructure-as-code and \
            automated solutions over manual steps.",
        recommended_tools: &["shell_execute", "http_request", "file_read", "file_write", "file_edit"],
        guidelines: &[
            "Always check current state before making changes",
            "Use infrastructure-as-code over manual configuration",
            "Implement changes incrementally with rollback plans",
            "Monitor and verify after every deployment",
            "Never store secrets in code or logs",
        ],
    },
    AgentTemplate {
        name: "tutor",
        description: "Teaching agent that explains concepts clearly and adapts to the learner's level.",
        personality: "coach",
        system_instructions: "You are a teaching agent. Explain concepts clearly using analogies and \
            examples. Adapt your explanations to the learner's level. Encourage questions and verify \
            understanding. Build knowledge incrementally.",
        recommended_tools: &["memory_create", "memory_recall"],
        guidelines: &[
            "Assess the learner's current understanding first",
            "Build from fundamentals to advanced concepts",
            "Use concrete examples and analogies",
            "Ask questions to verify understanding",
            "Encourage exploration and experimentation",
        ],
    },
    AgentTemplate {
        name: "planner",
        description: "Project planning agent that breaks tasks into actionable steps with estimates.",
        personality: "detective",
        system_instructions: "You are a project planning agent. Break complex goals into concrete, \
            actionable tasks. Identify dependencies, risks, and milestones. Create realistic timelines \
            and prioritize effectively.",
        recommended_tools: &["memory_create", "memory_recall", "file_write"],
        guidelines: &[
            "Clarify the end goal and success criteria first",
            "Break work into tasks that take 2-4 hours each",
            "Identify dependencies and critical path",
            "Flag risks and mitigation strategies",
            "Prioritize by impact and urgency, not just order",
        ],
    },
    AgentTemplate {
        name: "reviewer",
        description: "Code and content review agent that provides constructive, actionable feedback.",
        personality: "detective",
        system_instructions: "You are a review agent. Examine code, documents, or artifacts with a \
            critical but constructive eye. Focus on correctness, clarity, and maintainability. \
            Provide specific, actionable feedback with examples.",
        recommended_tools: &["file_read", "shell_execute", "file_search"],
        guidelines: &[
            "Understand the intent before critiquing the implementation",
            "Prioritize issues by severity: bugs > design > style",
            "Provide specific suggestions, not just complaints",
            "Acknowledge what's done well, not just problems",
            "Verify your feedback is correct before sharing it",
        ],
    },
];

/// List all available agent templates.
pub fn list_templates() -> &'static [AgentTemplate] {
    TEMPLATES
}

/// Look up a template by name (case-insensitive).
pub fn get_template(name: &str) -> Option<&'static AgentTemplate> {
    let needle = name.trim().to_ascii_lowercase();
    TEMPLATES.iter().find(|t| t.name == needle)
}

/// Format a template's guidelines as a system prompt section.
pub fn format_template_prompt(template: &AgentTemplate) -> String {
    let mut prompt = String::new();
    prompt.push_str(template.system_instructions);
    if !template.guidelines.is_empty() {
        prompt.push_str("\n\nGuidelines:\n");
        for guideline in template.guidelines {
            prompt.push_str(&format!("- {guideline}\n"));
        }
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_templates_returns_all() {
        let templates = list_templates();
        assert!(templates.len() >= 8);
    }

    #[test]
    fn template_names_are_unique() {
        let templates = list_templates();
        let mut names: Vec<&str> = templates.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), templates.len());
    }

    #[test]
    fn get_template_finds_by_name() {
        let t = get_template("researcher").expect("researcher should exist");
        assert_eq!(t.name, "researcher");
        assert_eq!(t.personality, "scientist");
    }

    #[test]
    fn get_template_case_insensitive() {
        assert!(get_template("CODER").is_some());
        assert!(get_template("  Planner ").is_some());
    }

    #[test]
    fn get_template_returns_none_for_unknown() {
        assert!(get_template("nonexistent").is_none());
    }

    #[test]
    fn format_template_prompt_includes_instructions_and_guidelines() {
        let t = get_template("reviewer").unwrap();
        let prompt = format_template_prompt(t);
        assert!(prompt.contains("review agent"));
        assert!(prompt.contains("Guidelines:"));
        assert!(prompt.contains("severity"));
    }

    #[test]
    fn all_templates_have_valid_personalities() {
        for t in list_templates() {
            assert!(
                crate::personality::get_personality(t.personality).is_some(),
                "Template '{}' references unknown personality '{}'",
                t.name,
                t.personality
            );
        }
    }

    #[test]
    fn all_templates_have_descriptions() {
        for t in list_templates() {
            assert!(
                !t.description.is_empty(),
                "Template '{}' has empty description",
                t.name
            );
            assert!(
                !t.system_instructions.is_empty(),
                "Template '{}' has empty instructions",
                t.name
            );
        }
    }
}
