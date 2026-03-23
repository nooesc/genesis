use crate::CliError;

pub(crate) fn run_compress(
    input: String,
    output: Option<String>,
    level: Option<String>,
    format: Option<String>,
    training: bool,
) -> Result<String, CliError> {
    let level = parse_compression_level(level.as_deref())?;
    let format = parse_compression_format(format.as_deref())?;

    let raw = std::fs::read_to_string(&input)
        .map_err(|e| CliError::Other(format!("failed to read {}: {e}", input)))?;
    let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
        .map_err(|e| CliError::Other(format!("invalid trajectory JSON in {}: {e}", input)))?;

    let compressed = if training {
        genesis_core::compress::TrajectoryCompressor::default().compress_for_training(&trajectory)
    } else {
        genesis_core::compress::compress(&trajectory, level)
    };
    let rendered = match format {
        CompressionFormat::Json => serde_json::to_string_pretty(&compressed)?,
        CompressionFormat::ShareGpt => {
            serde_json::to_string_pretty(&genesis_core::compress::to_sharegpt(&compressed))?
        }
        CompressionFormat::ChatMl => genesis_core::compress::to_chatml(&compressed),
    };

    match output {
        Some(path) => {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        CliError::Other(format!(
                            "failed to create parent directory for {}: {e}",
                            path
                        ))
                    })?;
                }
            }
            std::fs::write(&path, rendered)
                .map_err(|e| CliError::Other(format!("failed to write {}: {e}", path)))?;
            Ok(format!("wrote compressed trajectory to {path}"))
        }
        None => Ok(rendered),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompressionFormat {
    Json,
    ShareGpt,
    ChatMl,
}

pub(crate) fn parse_compression_level(
    raw: Option<&str>,
) -> Result<genesis_core::compress::CompressionLevel, CliError> {
    match raw.unwrap_or("medium").trim().to_ascii_lowercase().as_str() {
        "light" => Ok(genesis_core::compress::CompressionLevel::Light),
        "medium" => Ok(genesis_core::compress::CompressionLevel::Medium),
        "heavy" => Ok(genesis_core::compress::CompressionLevel::Heavy),
        other => Err(CliError::Other(format!(
            "unknown compression level '{other}', expected light, medium, or heavy"
        ))),
    }
}

pub(crate) fn parse_compression_format(raw: Option<&str>) -> Result<CompressionFormat, CliError> {
    match raw.unwrap_or("json").trim().to_ascii_lowercase().as_str() {
        "json" => Ok(CompressionFormat::Json),
        "sharegpt" => Ok(CompressionFormat::ShareGpt),
        "chatml" => Ok(CompressionFormat::ChatMl),
        other => Err(CliError::Other(format!(
            "unknown compression format '{other}', expected json, sharegpt, or chatml"
        ))),
    }
}
