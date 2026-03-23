use genesis_core::prompt::load_context_file;

use crate::format::context_template;
use crate::{CliError, ContextCommand};

pub(crate) fn run_context(command: ContextCommand) -> Result<String, CliError> {
    let current_dir = std::env::current_dir()?;

    match command {
        ContextCommand::Show => Ok(
            match load_context_file(&current_dir, &genesis_config::ContextSecurityPolicy::Warn) {
                Some(contents) => contents,
                None => "no context file found in current directory".to_owned(),
            },
        ),
        ContextCommand::Init => {
            let context_dir = current_dir.join(".genesis");
            let context_path = context_dir.join("context.md");

            if context_path.exists() {
                return Ok(format!(
                    "context file already exists: {}",
                    context_path.display()
                ));
            }

            std::fs::create_dir_all(&context_dir)?;
            std::fs::write(&context_path, context_template())?;

            Ok(format!("created context file: {}", context_path.display()))
        }
        ContextCommand::Edit => {
            let context_dir = current_dir.join(".genesis");
            let context_path = context_dir.join("context.md");

            if !context_path.exists() {
                // Create with template if it doesn't exist
                std::fs::create_dir_all(&context_dir)?;
                std::fs::write(&context_path, context_template())?;
            }

            let editor = genesis_config::env::get_or(genesis_config::env::EDITOR, "vi");
            let path_str = context_path.display().to_string();
            let status = std::process::Command::new(&editor)
                .arg(&path_str)
                .status()
                .map_err(|e| CliError::Other(format!("failed to launch {editor}: {e}")))?;
            if status.success() {
                Ok(format!("context saved: {path_str}"))
            } else {
                Err(CliError::Other(format!(
                    "{editor} exited with status {status}"
                )))
            }
        }
    }
}
