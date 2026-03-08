//! Dataset manifest and metadata for trajectory collections.
//!
//! A dataset manifest tracks provenance, statistics, and configuration
//! for a directory of trajectory files used in training.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Describes a collection of trajectory files as a training dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub name: String,
    pub description: String,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
    pub file_count: usize,
    pub total_steps: usize,
    pub models: Vec<String>,
    pub tags: Vec<String>,
    pub statistics: DatasetStatistics,
    /// Provenance: how this dataset was created.
    pub provenance: DatasetProvenance,
}

/// Statistical summary of a dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetStatistics {
    pub avg_steps_per_trajectory: f64,
    pub min_steps: usize,
    pub max_steps: usize,
    pub outcome_counts: HashMap<String, usize>,
    pub tag_counts: HashMap<String, usize>,
    pub model_counts: HashMap<String, usize>,
    pub avg_quality_score: Option<f64>,
}

/// How the dataset was constructed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetProvenance {
    pub source: String,
    pub filters_applied: Vec<String>,
    pub quality_threshold: Option<f64>,
    pub toolset: Option<String>,
}

/// Build a manifest by scanning a directory of trajectory JSON files.
pub fn build_manifest(
    name: &str,
    description: &str,
    dir: &Path,
    recursive: bool,
) -> Result<DatasetManifest, DatasetError> {
    let files = collect_json_files(dir, recursive)?;
    if files.is_empty() {
        return Err(DatasetError::EmptyDirectory(dir.display().to_string()));
    }

    let mut total_steps = 0usize;
    let mut min_steps = usize::MAX;
    let mut max_steps = 0usize;
    let mut models: HashMap<String, usize> = HashMap::new();
    let mut tags: HashMap<String, usize> = HashMap::new();
    let mut outcomes: HashMap<String, usize> = HashMap::new();
    let mut quality_sum = 0.0f64;
    let mut quality_count = 0usize;

    for path in &files {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| DatasetError::Io(format!("{}: {e}", path.display())))?;
        let traj: crate::trajectory::Trajectory = serde_json::from_str(&raw)
            .map_err(|e| DatasetError::Parse(format!("{}: {e}", path.display())))?;

        let step_count = traj.steps.len();
        total_steps += step_count;
        min_steps = min_steps.min(step_count);
        max_steps = max_steps.max(step_count);

        *models.entry(traj.model.clone()).or_default() += 1;

        for tag in &traj.tags {
            *tags.entry(tag.clone()).or_default() += 1;
        }

        let outcome_str = match &traj.outcome {
            Some(crate::trajectory::TrajectoryOutcome::Success) => "success",
            Some(crate::trajectory::TrajectoryOutcome::Failure { .. }) => "failure",
            Some(crate::trajectory::TrajectoryOutcome::Abandoned) => "abandoned",
            None => "unknown",
        };
        *outcomes.entry(outcome_str.to_owned()).or_default() += 1;

        let score = crate::quality::score(&traj);
        quality_sum += score.overall;
        quality_count += 1;
    }

    let file_count = files.len();
    let avg_steps = if file_count > 0 {
        total_steps as f64 / file_count as f64
    } else {
        0.0
    };

    let avg_quality = if quality_count > 0 {
        Some(quality_sum / quality_count as f64)
    } else {
        None
    };

    let mut model_list: Vec<String> = models.keys().cloned().collect();
    model_list.sort();
    let mut tag_list: Vec<String> = tags.keys().cloned().collect();
    tag_list.sort();

    let now = chrono::Utc::now().to_rfc3339();

    Ok(DatasetManifest {
        name: name.to_owned(),
        description: description.to_owned(),
        version: "1.0".to_owned(),
        created_at: now.clone(),
        updated_at: now,
        file_count,
        total_steps,
        models: model_list,
        tags: tag_list,
        statistics: DatasetStatistics {
            avg_steps_per_trajectory: avg_steps,
            min_steps,
            max_steps,
            outcome_counts: outcomes,
            tag_counts: tags,
            model_counts: models,
            avg_quality_score: avg_quality,
        },
        provenance: DatasetProvenance {
            source: dir.display().to_string(),
            filters_applied: vec![],
            quality_threshold: None,
            toolset: None,
        },
    })
}

/// Save a manifest as `dataset.json` in the given directory.
pub fn save_manifest(manifest: &DatasetManifest, dir: &Path) -> Result<(), DatasetError> {
    let path = dir.join("dataset.json");
    let json = serde_json::to_string_pretty(manifest)
        .map_err(|e| DatasetError::Io(format!("failed to serialize manifest: {e}")))?;
    std::fs::write(&path, json)
        .map_err(|e| DatasetError::Io(format!("failed to write {}: {e}", path.display())))?;
    Ok(())
}

/// Load a manifest from `dataset.json` in the given directory.
pub fn load_manifest(dir: &Path) -> Result<DatasetManifest, DatasetError> {
    let path = dir.join("dataset.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| DatasetError::Io(format!("failed to read {}: {e}", path.display())))?;
    serde_json::from_str(&raw)
        .map_err(|e| DatasetError::Parse(format!("invalid manifest: {e}")))
}

fn collect_json_files(
    dir: &Path,
    recursive: bool,
) -> Result<Vec<std::path::PathBuf>, DatasetError> {
    let mut files = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| DatasetError::Io(format!("failed to read {}: {e}", dir.display())))?;

    for entry in entries {
        let entry = entry.map_err(|e| DatasetError::Io(e.to_string()))?;
        let path = entry.path();

        if path.is_dir() && recursive {
            files.extend(collect_json_files(&path, true)?);
        } else if path.extension().and_then(|e| e.to_str()) == Some("json")
            && path.file_name().and_then(|f| f.to_str()) != Some("dataset.json")
        {
            files.push(path);
        }
    }

    files.sort();
    Ok(files)
}

#[derive(Debug)]
pub enum DatasetError {
    EmptyDirectory(String),
    Io(String),
    Parse(String),
}

impl std::fmt::Display for DatasetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyDirectory(d) => write!(f, "no trajectory files in {d}"),
            Self::Io(msg) => write!(f, "{msg}"),
            Self::Parse(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for DatasetError {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_test_trajectory(dir: &Path, id: &str, model: &str, steps: usize) {
        let mut step_list = Vec::new();
        for i in 0..steps {
            step_list.push(serde_json::json!({
                "step_index": i,
                "timestamp": "2026-01-01T00:00:00Z",
                "action_type": if i == 0 { "user_message" } else { "assistant_message" },
                "content": format!("step {i}")
            }));
        }

        let traj = serde_json::json!({
            "session_id": id,
            "model": model,
            "system_prompt_hash": "h",
            "started_at": "2026-01-01T00:00:00Z",
            "completed_at": "2026-01-01T00:01:00Z",
            "steps": step_list,
            "outcome": {"type": "success"},
            "tags": ["test", model]
        });

        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::to_string(&traj).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn build_manifest_computes_statistics() {
        let dir = tempdir().expect("tempdir");
        write_test_trajectory(dir.path(), "s1", "gpt-4", 5);
        write_test_trajectory(dir.path(), "s2", "gpt-4", 3);
        write_test_trajectory(dir.path(), "s3", "claude", 7);

        let manifest =
            build_manifest("test-dataset", "A test dataset", dir.path(), false)
                .expect("should build manifest");

        assert_eq!(manifest.name, "test-dataset");
        assert_eq!(manifest.file_count, 3);
        assert_eq!(manifest.total_steps, 15);
        assert_eq!(manifest.statistics.min_steps, 3);
        assert_eq!(manifest.statistics.max_steps, 7);
        assert!((manifest.statistics.avg_steps_per_trajectory - 5.0).abs() < 0.01);
        assert!(manifest.models.contains(&"gpt-4".to_owned()));
        assert!(manifest.models.contains(&"claude".to_owned()));
        assert_eq!(manifest.statistics.model_counts["gpt-4"], 2);
        assert_eq!(manifest.statistics.model_counts["claude"], 1);
        assert!(manifest.statistics.avg_quality_score.is_some());
    }

    #[test]
    fn build_manifest_excludes_dataset_json() {
        let dir = tempdir().expect("tempdir");
        write_test_trajectory(dir.path(), "s1", "gpt-4", 3);
        // Write a dataset.json file that should be ignored
        std::fs::write(
            dir.path().join("dataset.json"),
            r#"{"name":"old"}"#,
        )
        .unwrap();

        let manifest =
            build_manifest("new", "fresh", dir.path(), false).expect("should build");
        assert_eq!(manifest.file_count, 1);
    }

    #[test]
    fn save_and_load_manifest_roundtrip() {
        let dir = tempdir().expect("tempdir");
        write_test_trajectory(dir.path(), "s1", "gpt-4", 4);

        let manifest =
            build_manifest("roundtrip", "test", dir.path(), false).expect("should build");
        save_manifest(&manifest, dir.path()).expect("should save");

        let loaded = load_manifest(dir.path()).expect("should load");
        assert_eq!(loaded.name, "roundtrip");
        assert_eq!(loaded.file_count, 1);
        assert_eq!(loaded.total_steps, 4);
    }

    #[test]
    fn build_manifest_empty_dir_errors() {
        let dir = tempdir().expect("tempdir");
        let result = build_manifest("empty", "no files", dir.path(), false);
        assert!(result.is_err());
    }

    #[test]
    fn build_manifest_tracks_tags() {
        let dir = tempdir().expect("tempdir");
        write_test_trajectory(dir.path(), "s1", "gpt-4", 2);
        write_test_trajectory(dir.path(), "s2", "claude", 2);

        let manifest = build_manifest("tags", "test", dir.path(), false).expect("should build");
        assert!(manifest.tags.contains(&"test".to_owned()));
        assert!(manifest.statistics.tag_counts.contains_key("test"));
    }

    #[test]
    fn build_manifest_tracks_outcomes() {
        let dir = tempdir().expect("tempdir");
        write_test_trajectory(dir.path(), "s1", "m", 2);

        let manifest =
            build_manifest("outcomes", "test", dir.path(), false).expect("should build");
        assert_eq!(manifest.statistics.outcome_counts["success"], 1);
    }
}
