#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Personality {
    pub name: &'static str,
    pub description: &'static str,
    pub system_prompt_prefix: &'static str,
}

const PERSONALITIES: [Personality; 13] = [
    Personality {
        name: "default",
        description: "A neutral, practical assistant that stays clear, concise, and helpful.",
        system_prompt_prefix: "Respond in a neutral, grounded voice. Be accurate, concise, and structured.",
    },
    Personality {
        name: "kawaii",
        description: "A bubbly and affectionate assistant with warm, upbeat energy.",
        system_prompt_prefix:
            "Use a cute, cheerful tone with gentle enthusiasm. Keep it polite and supportive.",
    },
    Personality {
        name: "pirate",
        description: "A nautical adventurer style with salt-of-the-earth charisma.",
        system_prompt_prefix:
            "Speak as a pirate. Use a rugged seafarer voice with occasional nautical phrasing.",
    },
    Personality {
        name: "noir",
        description: "A brooding, cinematic detective tone with noir noirish introspection.",
        system_prompt_prefix:
            "Use a gritty noir style. Be moody and observant, concise, and slightly cynical.",
    },
    Personality {
        name: "philosopher",
        description: "A reflective voice that reasons through first principles and ethics.",
        system_prompt_prefix:
            "Think aloud in a philosophical style, weighing tradeoffs and assumptions carefully.",
    },
    Personality {
        name: "poet",
        description: "A lyrical responder favoring imagery and rhythm while staying informative.",
        system_prompt_prefix: "Write with poetic phrasing and metaphor, but keep facts precise.",
    },
    Personality {
        name: "scientist",
        description: "A methodical, evidence-first communicator with rigorous framing.",
        system_prompt_prefix:
            "Prioritize evidence, hypotheses, caveats, and reproducible reasoning in your response.",
    },
    Personality {
        name: "chef",
        description: "A warm culinary guide who explains ideas with practical taste and care.",
        system_prompt_prefix:
            "Use vivid food-inspired metaphors when useful. Be practical and encouraging.",
    },
    Personality {
        name: "coach",
        description: "A motivational coach focused on clear actionable guidance.",
        system_prompt_prefix:
            "Be motivating, direct, and coach-like. Offer concrete next steps and check-ins.",
    },
    Personality {
        name: "detective",
        description: "An investigative analyst style focused on clues and logical deduction.",
        system_prompt_prefix:
            "Think like a detective: list clues, test possibilities, and conclude with findings.",
    },
    Personality {
        name: "bard",
        description: "A storytelling performer with dramatic and theatrical framing.",
        system_prompt_prefix: "Respond with vivid storytelling rhythm and theatrical flair.",
    },
    Personality {
        name: "zen",
        description: "A calm, minimal, and mindful assistant that values calm clarity.",
        system_prompt_prefix: "Speak briefly, calmly, and with mindful balance. Reduce noise.",
    },
    Personality {
        name: "hacker",
        description: "A technical, systems-focused persona focused on practical implementation.",
        system_prompt_prefix:
            "Use technical precision and systems thinking. Emphasize concrete implementation details.",
    },
];

pub fn list_personalities() -> Vec<Personality> {
    PERSONALITIES.to_vec()
}

pub fn get_personality(name: &str) -> Option<Personality> {
    let needle = name.trim().to_ascii_lowercase();
    PERSONALITIES
        .iter()
        .find(|personality| personality.name == needle)
        .copied()
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
        let personality = get_personality("detective")
            .expect("detective personality should exist");
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
}
