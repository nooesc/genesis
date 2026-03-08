//! Skill system for the Genesis agent.
//!
//! Skills are reusable procedures that the agent can create, invoke, and
//! improve over time. They're stored in SQLite and injected into the
//! system prompt so the agent knows what skills are available.

use genesis_storage::{SkillStore, SkillUsageStore, StoredSkill};

/// Format a list of skills as a prompt section for the agent.
///
/// Returns `None` if there are no skills to include.
pub fn format_skills_for_prompt(skills: &[StoredSkill]) -> Option<String> {
    format_skills_for_prompt_with_stats(skills, None)
}

/// Format skills with optional usage stats for self-improvement context.
pub fn format_skills_for_prompt_with_stats(
    skills: &[StoredSkill],
    usage_store: Option<&SkillUsageStore>,
) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut section = String::from("## Available Skills\n\n");
    section.push_str("You have the following learned skills available. ");
    section.push_str("When a user's request matches a skill's trigger, follow the skill's instructions.\n\n");
    section.push_str("**Self-improvement:** After using a skill, call `skill_record_usage` with the outcome ");
    section.push_str("(success/partial/failure) and feedback. If a skill's instructions can be improved ");
    section.push_str("based on what you learned, update it with `skill_create`.\n\n");

    for skill in skills {
        section.push_str(&format!("### {} (v{})\n", skill.name, skill.version));
        section.push_str(&format!("**Description:** {}\n", skill.description));
        if let Some(trigger) = &skill.trigger_hint {
            section.push_str(&format!("**Trigger:** {trigger}\n"));
        }
        if !skill.tags.is_empty() {
            section.push_str(&format!("**Tags:** {}\n", skill.tags.join(", ")));
        }

        // Include usage stats if available
        if let Some(store) = usage_store {
            if let Ok(stats) = store.stats(&skill.name) {
                if stats.total_uses > 0 {
                    section.push_str(&format!(
                        "**Usage:** {} uses ({} success, {} failure, refined {} times)\n",
                        stats.total_uses, stats.successes, stats.failures, stats.times_refined
                    ));
                }
            }
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
    let usage_store = SkillUsageStore::new(database_path);
    format_skills_for_prompt_with_stats(&skills, Some(&usage_store))
}

/// Load skills with context-aware filtering: all skills get a brief listing,
/// but only skills matching the user's prompt get full instructions injected.
///
/// When there are fewer than 10 skills total, all get full instructions
/// (no filtering needed). Above that threshold, we use trigger-based matching
/// to avoid wasting context on irrelevant skill instructions.
pub fn load_skills_prompt_for_prompt(
    database_path: &std::path::Path,
    user_prompt: &str,
) -> Option<String> {
    let store = SkillStore::new(database_path);
    let all_skills = store.list_all().ok()?;
    if all_skills.is_empty() {
        return None;
    }
    let usage_store = SkillUsageStore::new(database_path);

    // When few skills, load all with full instructions.
    if all_skills.len() <= 10 {
        return format_skills_for_prompt_with_stats(&all_skills, Some(&usage_store));
    }

    // Many skills: brief listing of all + full instructions for matched ones.
    let matched = store.find_matching(user_prompt, 5).ok()?;
    let matched_names: std::collections::HashSet<&str> =
        matched.iter().map(|s| s.name.as_str()).collect();

    let mut section = String::from("## Available Skills\n\n");
    section.push_str("You have the following learned skills. ");
    section.push_str("Use `skill_get` to load full instructions for any skill not shown below.\n\n");
    section.push_str("**Self-improvement:** After using a skill, call `skill_record_usage` with the outcome.\n\n");

    // Brief listing
    section.push_str("### All Skills\n\n");
    for skill in &all_skills {
        let marker = if matched_names.contains(skill.name.as_str()) {
            " ← auto-loaded"
        } else {
            ""
        };
        section.push_str(&format!(
            "- **{}** (v{}): {}{}\n",
            skill.name, skill.version, skill.description, marker
        ));
    }
    section.push('\n');

    // Full instructions for matched skills
    if !matched.is_empty() {
        section.push_str("### Auto-Loaded Skill Instructions\n\n");
        section.push_str("The following skills were automatically loaded because they match your request:\n\n");
        for skill in &matched {
            section.push_str(&format!("#### {} (v{})\n", skill.name, skill.version));
            if let Some(trigger) = &skill.trigger_hint {
                section.push_str(&format!("**Trigger:** {trigger}\n"));
            }
            if !skill.tags.is_empty() {
                section.push_str(&format!("**Tags:** {}\n", skill.tags.join(", ")));
            }
            if let Ok(stats) = usage_store.stats(&skill.name) {
                if stats.total_uses > 0 {
                    section.push_str(&format!(
                        "**Usage:** {} uses ({} success, {} failure)\n",
                        stats.total_uses, stats.successes, stats.failures
                    ));
                }
            }
            section.push_str(&format!("**Instructions:**\n{}\n\n", skill.instructions));
        }
    }

    Some(section)
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
        assert!(prompt.contains("### code_review (v1)"));
        assert!(prompt.contains("code_review description"));
        assert!(prompt.contains("Step 1: do code_review"));
        assert!(prompt.contains("review code"));
        assert!(prompt.contains("skill_record_usage"));
    }

    #[test]
    fn format_skills_includes_multiple_skills() {
        let skills = vec![
            sample_skill("deploy", Some("deploy to production")),
            sample_skill("test", Some("run tests")),
        ];
        let prompt = format_skills_for_prompt(&skills).expect("should have content");
        assert!(prompt.contains("### deploy (v1)"));
        assert!(prompt.contains("### test (v1)"));
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

    #[test]
    fn format_skills_with_stats_includes_usage_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let skill_store = SkillStore::new(&db_path);
        skill_store
            .upsert("deploy", "Deploy app", "Run deploy", None, &[])
            .expect("upsert");

        let usage_store = SkillUsageStore::new(&db_path);
        usage_store.record_usage("deploy", None, "success", None).unwrap();
        usage_store.record_usage("deploy", None, "failure", Some("Timed out")).unwrap();

        let skills = skill_store.list_all().unwrap();
        let prompt = format_skills_for_prompt_with_stats(&skills, Some(&usage_store))
            .expect("should have prompt");

        assert!(prompt.contains("2 uses"));
        assert!(prompt.contains("1 success"));
        assert!(prompt.contains("1 failure"));
    }

    #[test]
    fn format_skills_with_stats_omits_zero_usage() {
        let skills = vec![sample_skill("new_skill", None)];
        // Pass None for usage store — no stats available
        let prompt = format_skills_for_prompt_with_stats(&skills, None)
            .expect("should have prompt");
        assert!(!prompt.contains("Usage:"));
    }

    #[test]
    fn load_skills_prompt_for_prompt_loads_all_when_few() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let store = SkillStore::new(&db_path);
        store
            .upsert("deploy", "Deploy app", "Run deploy", Some("deploy production"), &["ops"])
            .expect("upsert");
        store
            .upsert("review", "Review code", "Check bugs", Some("review code"), &["dev"])
            .expect("upsert");

        let prompt = load_skills_prompt_for_prompt(&db_path, "deploy this app")
            .expect("should have prompt");
        // With ≤10 skills, all get full instructions
        assert!(prompt.contains("### deploy"));
        assert!(prompt.contains("### review"));
        assert!(prompt.contains("Run deploy"));
        assert!(prompt.contains("Check bugs"));
    }

    #[test]
    fn load_skills_prompt_for_prompt_filters_when_many() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let store = SkillStore::new(&db_path);
        // Create 12 skills (above the 10 threshold)
        for i in 0..12 {
            let name = format!("skill_{i}");
            let desc = format!("Description for skill {i}");
            let instructions = format!("Instructions for skill {i}");
            let trigger = format!("trigger for skill {i}");
            store.upsert(&name, &desc, &instructions, Some(&trigger), &[]).unwrap();
        }
        // Create a skill that will match
        store
            .upsert("deploy_app", "Deploy application", "Run kubectl apply", Some("deploy production app"), &["ops", "deploy"])
            .unwrap();

        let prompt = load_skills_prompt_for_prompt(&db_path, "please deploy my app to production")
            .expect("should have prompt");

        // Should contain brief listing of all skills
        assert!(prompt.contains("All Skills"));
        assert!(prompt.contains("skill_0"));

        // Should have auto-loaded section with matching skill
        assert!(prompt.contains("Auto-Loaded"));
        assert!(prompt.contains("deploy_app"));
        assert!(prompt.contains("Run kubectl apply"));
    }

    #[test]
    fn load_skills_prompt_for_prompt_returns_none_for_no_skills() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap");

        let result = load_skills_prompt_for_prompt(&db_path, "hello");
        assert!(result.is_none());
    }
}
