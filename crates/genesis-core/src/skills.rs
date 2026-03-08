//! Skill system for the Genesis agent.
//!
//! Skills are reusable procedures that the agent can create, invoke, and
//! improve over time. They're stored in SQLite and injected into the
//! system prompt so the agent knows what skills are available.

use genesis_storage::{SkillStore, StoredSkill};

/// Format a list of skills as a prompt section for the agent.
///
/// Returns `None` if there are no skills to include.
pub fn format_skills_for_prompt(skills: &[StoredSkill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut section = String::from("## Available Skills\n\n");
    section.push_str("You have the following learned skills available. ");
    section.push_str("When a user's request matches a skill's trigger, follow the skill's instructions.\n\n");

    for skill in skills {
        section.push_str(&format!("### {}\n", skill.name));
        section.push_str(&format!("**Description:** {}\n", skill.description));
        if let Some(trigger) = &skill.trigger_hint {
            section.push_str(&format!("**Trigger:** {trigger}\n"));
        }
        if !skill.tags.is_empty() {
            section.push_str(&format!("**Tags:** {}\n", skill.tags.join(", ")));
        }
        section.push_str(&format!("**Instructions:**\n{}\n\n", skill.instructions));
    }

    Some(section)
}

/// Load all skills from the store and format them for prompt injection.
///
/// Returns `None` if the store has no skills or an error occurs (fails silently
/// since skill loading should not block agent startup).
pub fn load_skills_prompt(database_path: &std::path::Path) -> Option<String> {
    let store = SkillStore::new(database_path);
    let skills = store.list_all().ok()?;
    format_skills_for_prompt(&skills)
}

/// Match a user message against skill trigger hints.
///
/// Returns skills whose trigger hint appears (case-insensitive substring match)
/// in the user's message. This is a simple heuristic — the LLM itself makes
/// the final decision on which skill to invoke.
pub fn match_skills(skills: &[StoredSkill], user_message: &str) -> Vec<StoredSkill> {
    let lower = user_message.to_lowercase();
    skills
        .iter()
        .filter(|s| {
            s.trigger_hint
                .as_ref()
                .map(|t| {
                    // Check if any significant word from the trigger appears in the message
                    t.to_lowercase()
                        .split_whitespace()
                        .filter(|w| w.len() > 3) // skip short words like "when", "the", "a"
                        .any(|word| lower.contains(word))
                })
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use genesis_storage::StoredSkill;

    fn sample_skill(name: &str, trigger: Option<&str>) -> StoredSkill {
        StoredSkill {
            name: name.to_owned(),
            description: format!("{name} description"),
            instructions: format!("Step 1: do {name}"),
            trigger_hint: trigger.map(|t| t.to_owned()),
            tags: vec!["test".to_owned()],
            version: 1,
            created_at: "2026-03-08".to_owned(),
            updated_at: "2026-03-08".to_owned(),
        }
    }

    #[test]
    fn format_skills_returns_none_for_empty() {
        assert!(format_skills_for_prompt(&[]).is_none());
    }

    #[test]
    fn format_skills_includes_skill_details() {
        let skills = vec![sample_skill("code_review", Some("review code"))];
        let prompt = format_skills_for_prompt(&skills).expect("should have content");
        assert!(prompt.contains("### code_review"));
        assert!(prompt.contains("code_review description"));
        assert!(prompt.contains("Step 1: do code_review"));
        assert!(prompt.contains("review code"));
    }

    #[test]
    fn format_skills_includes_multiple_skills() {
        let skills = vec![
            sample_skill("deploy", Some("deploy to production")),
            sample_skill("test", Some("run tests")),
        ];
        let prompt = format_skills_for_prompt(&skills).expect("should have content");
        assert!(prompt.contains("### deploy"));
        assert!(prompt.contains("### test"));
    }

    #[test]
    fn match_skills_finds_trigger_matches() {
        let skills = vec![
            sample_skill("code_review", Some("when user asks to review code")),
            sample_skill("deploy", Some("when user wants to deploy")),
            sample_skill("no_trigger", None),
        ];

        let matches = match_skills(&skills, "Can you review my code?");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "code_review");
    }

    #[test]
    fn match_skills_returns_empty_for_no_match() {
        let skills = vec![
            sample_skill("deploy", Some("when user wants to deploy to production")),
        ];

        let matches = match_skills(&skills, "What is the weather today?");
        assert!(matches.is_empty());
    }

    #[test]
    fn match_skills_is_case_insensitive() {
        let skills = vec![
            sample_skill("deploy", Some("Deploy to production")),
        ];

        let matches = match_skills(&skills, "can you DEPLOY this?");
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn load_skills_prompt_returns_none_for_missing_db() {
        let result = load_skills_prompt(std::path::Path::new("/nonexistent/path/db.sqlite"));
        assert!(result.is_none());
    }

    #[test]
    fn load_skills_prompt_integrates_with_storage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let store = SkillStore::new(&db_path);
        store
            .upsert("greet", "Greet the user", "Say hello warmly", Some("greeting"), &["social"])
            .expect("upsert");

        let prompt = load_skills_prompt(&db_path).expect("should have prompt");
        assert!(prompt.contains("### greet"));
        assert!(prompt.contains("Say hello warmly"));
    }
}
