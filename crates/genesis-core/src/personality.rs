#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Personality {
    pub name: &'static str,
    pub description: &'static str,
    pub system_prompt_prefix: &'static str,
}

pub fn list_personalities() -> Vec<Personality> {
    genesis_lua::bundled_personalities()
        .iter()
        .map(|personality| Personality {
            name: personality.name,
            description: personality.description,
            system_prompt_prefix: personality.system_prompt,
        })
        .collect()
}

pub fn get_personality(name: &str) -> Option<Personality> {
    let needle = name.trim().to_ascii_lowercase();
    genesis_lua::bundled_personalities()
        .iter()
        .find(|personality| personality.name == needle)
        .map(|personality| Personality {
            name: personality.name,
            description: personality.description,
            system_prompt_prefix: personality.system_prompt,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_personalities_returns_all_personalities() {
        let personalities = list_personalities();
        assert_eq!(personalities.len(), 13);
        assert_eq!(
            personalities.first().map(|p| p.name),
            Some("default"),
            "personalities should retain declared order"
        );
    }

    #[test]
    fn list_personalities_contains_unique_names() {
        let names: Vec<&str> = list_personalities().iter().map(|p| p.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
        assert!(names.contains(&"kawaii"));
        assert!(names.contains(&"hacker"));
    }

    #[test]
    fn get_personality_matches_exact_name() {
        let personality = get_personality("detective").expect("detective personality should exist");
        assert_eq!(personality.name, "detective");
        assert!(
            personality.system_prompt_prefix.contains("detective"),
            "detective prefix should mention detective style"
        );
    }

    #[test]
    fn get_personality_is_case_and_whitespace_insensitive() {
        let lower = get_personality("  PIrate ").expect("pirate should normalize");
        let mixed = get_personality("NoIr").expect("noir should normalize");
        assert_eq!(lower.name, "pirate");
        assert_eq!(mixed.name, "noir");
    }

    #[test]
    fn get_personality_returns_none_for_unknown_name() {
        assert!(get_personality("bardic").is_none());
    }

    #[test]
    fn default_personality_includes_stability_prompts() {
        let personality = get_personality("default").expect("default should exist");
        assert_eq!(personality.name, "default");
        assert!(!personality.description.is_empty());
        assert!(!personality.system_prompt_prefix.is_empty());
    }

    #[test]
    fn personalities_are_sourced_from_bundled_lua_defaults() {
        let listed = list_personalities();
        let bundled = genesis_lua::bundled_personalities();

        assert_eq!(listed.len(), bundled.len());
        for (listed, bundled) in listed.iter().zip(bundled.iter()) {
            assert_eq!(listed.name, bundled.name);
            assert_eq!(listed.description, bundled.description);
            assert_eq!(listed.system_prompt_prefix, bundled.system_prompt);
        }
    }
}
