use crate::{CliError, ToolsetCommand};

pub(crate) fn run_toolset(command: ToolsetCommand, json: bool) -> Result<String, CliError> {
    use genesis_core::toolset;
    use rand::SeedableRng;

    match command {
        ToolsetCommand::List => {
            let names = toolset::builtin_distribution_names();
            if json {
                let distributions: Vec<serde_json::Value> = names
                    .iter()
                    .filter_map(|name| {
                        toolset::builtin_distribution(name).map(|d| {
                            serde_json::json!({
                                "name": d.name,
                                "description": d.description,
                                "tool_count": d.possible_tools().len(),
                            })
                        })
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&distributions).unwrap())
            } else {
                let mut lines = vec![format!("{:<18} {:<5} {}", "NAME", "TOOLS", "DESCRIPTION")];
                for name in &names {
                    if let Some(d) = toolset::builtin_distribution(name) {
                        lines.push(format!(
                            "{:<18} {:<5} {}",
                            d.name,
                            d.possible_tools().len(),
                            d.description
                        ));
                    }
                }
                Ok(lines.join("\n"))
            }
        }
        ToolsetCommand::Show { name } => {
            let dist = toolset::builtin_distribution(&name).ok_or_else(|| {
                CliError::Other(format!(
                    "unknown distribution '{name}'. Available: {}",
                    toolset::builtin_distribution_names().join(", ")
                ))
            })?;

            if json {
                Ok(serde_json::to_string_pretty(&dist).unwrap())
            } else {
                let mut lines = vec![
                    format!("Distribution: {}", dist.name),
                    format!("Description:  {}", dist.description),
                    format!("Tools ({}):", dist.tools.len()),
                ];
                for (tool, prob) in &dist.tools {
                    lines.push(format!("  {:<30} {:.0}%", tool, prob * 100.0));
                }
                Ok(lines.join("\n"))
            }
        }
        ToolsetCommand::Sample { name, seed } => {
            let dist = toolset::builtin_distribution(&name).ok_or_else(|| {
                CliError::Other(format!(
                    "unknown distribution '{name}'. Available: {}",
                    toolset::builtin_distribution_names().join(", ")
                ))
            })?;

            let mut rng = match seed {
                Some(s) => rand::rngs::StdRng::seed_from_u64(s),
                None => rand::rngs::StdRng::from_os_rng(),
            };
            let selected = dist.sample(&mut rng);
            let mut tools: Vec<&str> = selected.iter().map(|s| s.as_str()).collect();
            tools.sort();

            if json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "distribution": dist.name,
                    "seed": seed,
                    "selected_count": tools.len(),
                    "possible_count": dist.possible_tools().len(),
                    "selected": tools,
                }))
                .unwrap())
            } else {
                let mut lines = vec![format!(
                    "Sampled {} tools from '{}' ({} possible):",
                    tools.len(),
                    dist.name,
                    dist.possible_tools().len()
                )];
                for tool in &tools {
                    lines.push(format!("  {tool}"));
                }
                Ok(lines.join("\n"))
            }
        }
    }
}
