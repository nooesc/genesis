use crate::{CliError, PersonalityCommand};

pub(crate) fn run_personality(command: PersonalityCommand, json: bool) -> Result<String, CliError> {
    use genesis_core::personality;

    match command {
        PersonalityCommand::List => {
            let personalities = personality::list_personalities();
            if json {
                let items: Vec<serde_json::Value> = personalities
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "description": p.description,
                        })
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&items).unwrap())
            } else {
                let mut lines = vec![format!("{:<16} {:<10} {}", "NAME", "SOURCE", "DESCRIPTION")];
                for p in &personalities {
                    lines.push(format!(
                        "{:<16} {:<10} {}",
                        p.name, "bundled", p.description
                    ));
                }
                Ok(lines.join("\n"))
            }
        }
        PersonalityCommand::Show { name } => {
            let p = personality::get_personality(&name).ok_or_else(|| {
                let available: Vec<String> = personality::list_personalities()
                    .iter()
                    .map(|p| p.name.to_owned())
                    .collect();
                CliError::Other(format!(
                    "unknown personality '{name}'. Available: {}",
                    available.join(", ")
                ))
            })?;

            if json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "name": p.name,
                    "source": "bundled",
                    "description": p.description,
                    "system_prompt_prefix": p.system_prompt_prefix,
                }))
                .unwrap())
            } else {
                Ok(format!(
                    "Personality: {}\nSource: bundled\nDescription: {}\n\nSystem prompt prefix:\n{}",
                    p.name, p.description, p.system_prompt_prefix
                ))
            }
        }
    }
}
