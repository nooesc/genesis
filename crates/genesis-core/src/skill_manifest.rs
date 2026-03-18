//! SKILL.md parser following the agentskills.io specification.
//!
//! Parses skill files with YAML frontmatter and Markdown instructions body.
//! Supports progressive disclosure: metadata-only loading for discovery,
//! full instructions loading for invocation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Parsed SKILL.md frontmatter metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillFrontmatter {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default = "default_true")]
    pub user_invocable: bool,
    #[serde(default)]
    pub disable_model_invocation: bool,
    /// Arbitrary extra fields from the frontmatter.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_yaml::Value>,
}

fn default_true() -> bool {
    true
}

/// A fully parsed SKILL.md file.
#[derive(Debug, Clone)]
pub struct ParsedSkill {
    pub frontmatter: SkillFrontmatter,
    pub instructions: String,
    pub source_path: Option<PathBuf>,
}

/// A lightweight skill entry for discovery (no instructions loaded).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub version: String,
    pub tags: Vec<String>,
    pub user_invocable: bool,
    pub path: PathBuf,
}

/// Parse a SKILL.md file from its text content.
///
/// The file must start with `---` YAML frontmatter delimiters.
/// Everything after the closing `---` is the instructions body.
pub fn parse_skill_md(content: &str) -> Result<ParsedSkill, SkillParseError> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Err(SkillParseError::MissingFrontmatter);
    }

    // Find the closing ---
    let after_open = &trimmed[3..];
    let close_pos = after_open
        .find("\n---")
        .ok_or(SkillParseError::UnclosedFrontmatter)?;

    let yaml_str = &after_open[..close_pos];
    let instructions_start = close_pos + 4; // skip "\n---"
    let instructions = after_open[instructions_start..].trim().to_owned();

    let frontmatter: SkillFrontmatter =
        serde_yaml::from_str(yaml_str).map_err(SkillParseError::InvalidYaml)?;

    if frontmatter.name.is_empty() {
        return Err(SkillParseError::MissingName);
    }

    Ok(ParsedSkill {
        frontmatter,
        instructions,
        source_path: None,
    })
}

/// Parse a SKILL.md file from a path on disk.
pub fn parse_skill_file(path: &Path) -> Result<ParsedSkill, SkillParseError> {
    let content = std::fs::read_to_string(path).map_err(SkillParseError::Io)?;
    let mut skill = parse_skill_md(&content)?;
    skill.source_path = Some(path.to_owned());
    Ok(skill)
}

/// Scan a directory for SKILL.md files (non-recursive).
///
/// Each subdirectory is expected to contain a SKILL.md file.
/// Returns lightweight entries for progressive disclosure.
pub fn scan_skills_dir(dir: &Path) -> Result<Vec<SkillEntry>, SkillParseError> {
    let mut entries = Vec::new();

    let read_dir = std::fs::read_dir(dir).map_err(SkillParseError::Io)?;
    for entry in read_dir {
        let entry = entry.map_err(SkillParseError::Io)?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let skill_file = path.join("SKILL.md");
        if !skill_file.exists() {
            continue;
        }

        match parse_skill_file(&skill_file) {
            Ok(skill) => {
                entries.push(SkillEntry {
                    name: skill.frontmatter.name,
                    description: skill.frontmatter.description,
                    version: skill.frontmatter.version,
                    tags: skill.frontmatter.tags,
                    user_invocable: skill.frontmatter.user_invocable,
                    path: skill_file,
                });
            }
            Err(_) => {
                // Skip malformed skill files during scan
                continue;
            }
        }
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// Search skill entries by name, description, or tags.
pub fn search_entries(entries: &[SkillEntry], query: &str) -> Vec<SkillEntry> {
    let lower = query.to_lowercase();
    entries
        .iter()
        .filter(|e| {
            e.name.to_lowercase().contains(&lower)
                || e.description.to_lowercase().contains(&lower)
                || e.tags.iter().any(|t| t.to_lowercase().contains(&lower))
        })
        .cloned()
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum SkillParseError {
    #[error("SKILL.md must start with --- frontmatter")]
    MissingFrontmatter,
    #[error("SKILL.md frontmatter missing closing ---")]
    UnclosedFrontmatter,
    #[error("invalid YAML frontmatter: {0}")]
    InvalidYaml(#[source] serde_yaml::Error),
    #[error("SKILL.md frontmatter missing 'name' field")]
    MissingName,
    #[error("IO error: {0}")]
    Io(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const SAMPLE_SKILL: &str = r#"---
name: code-review
description: Review code for bugs and style issues
version: "1.0"
author: Genesis Team
license: MIT
tags:
  - development
  - quality
allowed_tools:
  - read_file
  - shell_exec
user_invocable: true
disable_model_invocation: false
---

# Code Review

Review the provided code for:

1. Bug detection
2. Style consistency
3. Security vulnerabilities

## Steps

1. Read the target file
2. Analyze each function
3. Report findings in a structured format
"#;

    #[test]
    fn parse_valid_skill_md() {
        let skill = parse_skill_md(SAMPLE_SKILL).expect("should parse");
        assert_eq!(skill.frontmatter.name, "code-review");
        assert_eq!(
            skill.frontmatter.description,
            "Review code for bugs and style issues"
        );
        assert_eq!(skill.frontmatter.version, "1.0");
        assert_eq!(skill.frontmatter.author, "Genesis Team");
        assert_eq!(skill.frontmatter.license, "MIT");
        assert_eq!(skill.frontmatter.tags, vec!["development", "quality"]);
        assert_eq!(
            skill.frontmatter.allowed_tools,
            vec!["read_file", "shell_exec"]
        );
        assert!(skill.frontmatter.user_invocable);
        assert!(!skill.frontmatter.disable_model_invocation);
        assert!(skill.instructions.contains("# Code Review"));
        assert!(skill.instructions.contains("Bug detection"));
    }

    #[test]
    fn parse_minimal_frontmatter() {
        let content = "---\nname: minimal\n---\nDo the thing.";
        let skill = parse_skill_md(content).expect("should parse minimal");
        assert_eq!(skill.frontmatter.name, "minimal");
        assert!(skill.frontmatter.description.is_empty());
        assert!(skill.frontmatter.tags.is_empty());
        assert!(skill.frontmatter.user_invocable); // default true
        assert_eq!(skill.instructions, "Do the thing.");
    }

    #[test]
    fn parse_missing_frontmatter() {
        let content = "# Just markdown\nNo frontmatter here.";
        let err = parse_skill_md(content).unwrap_err();
        assert!(matches!(err, SkillParseError::MissingFrontmatter));
    }

    #[test]
    fn parse_unclosed_frontmatter() {
        let content = "---\nname: broken\nno closing delimiter";
        let err = parse_skill_md(content).unwrap_err();
        assert!(matches!(err, SkillParseError::UnclosedFrontmatter));
    }

    #[test]
    fn parse_missing_name_errors() {
        let content = "---\ndescription: no name field\n---\nInstructions here.";
        assert!(parse_skill_md(content).is_err());
    }

    #[test]
    fn parse_empty_name_errors() {
        let content = "---\nname: \"\"\n---\nInstructions here.";
        let err = parse_skill_md(content).unwrap_err();
        assert!(matches!(err, SkillParseError::MissingName));
    }

    #[test]
    fn parse_extra_fields_preserved() {
        let content = "---\nname: custom\ncustom_field: hello\npriority: 5\n---\nInstructions.";
        let skill = parse_skill_md(content).expect("should parse");
        assert_eq!(skill.frontmatter.name, "custom");
        assert!(skill.frontmatter.extra.contains_key("custom_field"));
    }

    #[test]
    fn parse_skill_file_from_disk() {
        let dir = tempdir().expect("tempdir");
        let skill_path = dir.path().join("SKILL.md");
        std::fs::write(&skill_path, SAMPLE_SKILL).expect("write");

        let skill = parse_skill_file(&skill_path).expect("should parse file");
        assert_eq!(skill.frontmatter.name, "code-review");
        assert_eq!(skill.source_path.as_deref(), Some(skill_path.as_path()));
    }

    #[test]
    fn scan_skills_directory() {
        let dir = tempdir().expect("tempdir");

        // Create two skill directories
        let skill1_dir = dir.path().join("code-review");
        std::fs::create_dir_all(&skill1_dir).expect("mkdir");
        std::fs::write(skill1_dir.join("SKILL.md"), SAMPLE_SKILL).expect("write");

        let skill2_dir = dir.path().join("deploy");
        std::fs::create_dir_all(&skill2_dir).expect("mkdir");
        std::fs::write(
            skill2_dir.join("SKILL.md"),
            "---\nname: deploy\ndescription: Deploy to production\nversion: \"2.0\"\ntags:\n  - ops\n---\nRun kubectl apply.",
        )
        .expect("write");

        // Create a non-skill directory (no SKILL.md)
        let other_dir = dir.path().join("random");
        std::fs::create_dir_all(&other_dir).expect("mkdir");
        std::fs::write(other_dir.join("README.md"), "not a skill").expect("write");

        let entries = scan_skills_dir(dir.path()).expect("should scan");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "code-review");
        assert_eq!(entries[1].name, "deploy");
        assert_eq!(entries[1].version, "2.0");
        assert_eq!(entries[1].tags, vec!["ops"]);
    }

    #[test]
    fn scan_skips_malformed_skills() {
        let dir = tempdir().expect("tempdir");

        let good_dir = dir.path().join("good");
        std::fs::create_dir_all(&good_dir).expect("mkdir");
        std::fs::write(
            good_dir.join("SKILL.md"),
            "---\nname: good\n---\nGood skill.",
        )
        .expect("write");

        let bad_dir = dir.path().join("bad");
        std::fs::create_dir_all(&bad_dir).expect("mkdir");
        std::fs::write(bad_dir.join("SKILL.md"), "no frontmatter here").expect("write");

        let entries = scan_skills_dir(dir.path()).expect("should scan");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "good");
    }

    #[test]
    fn search_entries_by_name() {
        let entries = vec![
            SkillEntry {
                name: "code-review".to_owned(),
                description: "Review code".to_owned(),
                version: "1.0".to_owned(),
                tags: vec!["dev".to_owned()],
                user_invocable: true,
                path: PathBuf::from("a/SKILL.md"),
            },
            SkillEntry {
                name: "deploy".to_owned(),
                description: "Deploy app".to_owned(),
                version: "1.0".to_owned(),
                tags: vec!["ops".to_owned()],
                user_invocable: true,
                path: PathBuf::from("b/SKILL.md"),
            },
        ];

        let results = search_entries(&entries, "review");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "code-review");
    }

    #[test]
    fn search_entries_by_tag() {
        let entries = vec![SkillEntry {
            name: "deploy".to_owned(),
            description: "Deploy app".to_owned(),
            version: "1.0".to_owned(),
            tags: vec!["operations".to_owned()],
            user_invocable: true,
            path: PathBuf::from("a/SKILL.md"),
        }];

        let results = search_entries(&entries, "operations");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_entries_case_insensitive() {
        let entries = vec![SkillEntry {
            name: "Deploy".to_owned(),
            description: "Deploy to Production".to_owned(),
            version: "1.0".to_owned(),
            tags: vec![],
            user_invocable: true,
            path: PathBuf::from("a/SKILL.md"),
        }];

        let results = search_entries(&entries, "deploy");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_entries_no_match() {
        let entries = vec![SkillEntry {
            name: "deploy".to_owned(),
            description: "Deploy app".to_owned(),
            version: "1.0".to_owned(),
            tags: vec![],
            user_invocable: true,
            path: PathBuf::from("a/SKILL.md"),
        }];

        let results = search_entries(&entries, "database");
        assert!(results.is_empty());
    }

    #[test]
    fn parse_allowed_tools_as_space_delimited() {
        // The spec also allows space-delimited tool lists
        let content = "---\nname: test\nallowed_tools:\n  - read_file\n  - write_file\n---\nDo it.";
        let skill = parse_skill_md(content).expect("should parse");
        assert_eq!(
            skill.frontmatter.allowed_tools,
            vec!["read_file", "write_file"]
        );
    }

    #[test]
    fn instructions_body_handles_empty() {
        let content = "---\nname: empty-body\n---\n";
        let skill = parse_skill_md(content).expect("should parse");
        assert!(skill.instructions.is_empty());
    }

    #[test]
    fn parse_user_invocable_defaults_true() {
        let content = "---\nname: defaults\n---\nInstructions.";
        let skill = parse_skill_md(content).expect("should parse");
        assert!(skill.frontmatter.user_invocable);
        assert!(!skill.frontmatter.disable_model_invocation);
    }
}
