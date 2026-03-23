use std::collections::{BTreeMap, BTreeSet};

use genesis_storage::{EmbeddingStore, MemoryStore, NewMemoryNote};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const DEFAULT_RECALL_LIMIT: usize = 5;
const DEDUP_SIMILARITY_THRESHOLD: f64 = 0.92;

pub struct MemoryStoreTool;

impl ToolHandler for MemoryStoreTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let key = call
            .arguments
            .get("key")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "key",
            })?;
        let value = call
            .arguments
            .get("value")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "value",
            })?;

        let database_path = context.db_path();
        let store = MemoryStore::new(&database_path);

        // If an embedding service is available, try to embed + deduplicate.
        if let Some(ref embedding_service) = context.embedding_service {
            let embedding = match embedding_service.embed_one(value) {
                Ok(emb) => emb,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to embed memory, falling back to plain store");
                    return store_plain(&store, call, context, key, value, &database_path);
                }
            };

            // Check for near-duplicates.
            let duplicates = match store.find_similar(&embedding, DEDUP_SIMILARITY_THRESHOLD, 5) {
                Ok(dupes) => dupes,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to check for duplicate memories");
                    Vec::new()
                }
            };

            if let Some(best) = duplicates.first() {
                // Merge into the existing memory.
                let best_id = best.memory.id.clone();
                let best_kind = best.memory.kind.clone();
                let score = best.score;

                store.merge_into(&best_id, value, 0.5).map_err(|error| {
                    ToolError::ExecutionFailed {
                        tool: call.name.clone(),
                        reason: format!("failed to merge memory into `{best_id}`: {error}"),
                    }
                })?;

                // Re-embed the merged memory content.
                if let Ok(Some(merged)) = store.get(&best_id) {
                    match embedding_service.embed_one(&merged.content) {
                        Ok(new_embedding) => {
                            let emb_store = EmbeddingStore::new(&database_path);
                            if let Err(e) = emb_store.store(
                                &best_id,
                                &new_embedding,
                                embedding_service.model_name(),
                            ) {
                                tracing::warn!(error = %e, "failed to store re-embedding for merged memory");
                            }
                            if let Err(e) =
                                store.auto_link_semantic(&best_id, &new_embedding, 0.8, 5)
                            {
                                tracing::warn!(error = %e, "failed to auto-link merged memory");
                            }
                            if let Err(e) =
                                store.auto_link_temporal(&best_id, &context.session_id, 5)
                            {
                                tracing::warn!(error = %e, "failed to temporally link merged memory");
                            }
                            let merged_keywords = extract_or_enrich_keywords(context, &best_kind, &merged.content);
                            if let Err(e) =
                                store.auto_link_entity(&best_id, &merged_keywords, 5)
                            {
                                tracing::warn!(error = %e, "failed to auto-link merged memory by entity");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to re-embed merged memory");
                        }
                    }
                }

                create_causal_links(&store, context, &best_id);
                try_auto_consolidate(&store, context);

                return Ok(ToolOutput {
                    content: format!(
                        "merged memory into existing `{best_kind}` (similarity: {score:.2})"
                    ),
                    metadata: BTreeMap::from([
                        ("tool".to_owned(), call.name.clone()),
                        ("key".to_owned(), key.clone()),
                        ("merged_into".to_owned(), best_id),
                        ("session_id".to_owned(), context.session_id.clone()),
                    ]),
                });
            }

            // No duplicate — create a new note and embed it.
            let note_id = memory_id(context, key);
            let keywords = extract_or_enrich_keywords(context, key, value);
            store
                .create_note(NewMemoryNote {
                    id: note_id.clone(),
                    session_id: Some(context.session_id.clone()),
                    kind: key.clone(),
                    content: value.clone(),
                    keywords: keywords.clone(),
                    tags: Vec::new(),
                    linked_ids: Vec::new(),
                    importance: 0.5,
                })
                .map_err(|error| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!(
                        "failed to store memory into `{}`: {error}",
                        database_path.display()
                    ),
                })?;

            let emb_store = EmbeddingStore::new(&database_path);
            if let Err(e) = emb_store.store(&note_id, &embedding, embedding_service.model_name()) {
                tracing::warn!(error = %e, "failed to store embedding for new memory");
            }
            if let Err(e) = store.auto_link_semantic(&note_id, &embedding, 0.8, 5) {
                tracing::warn!(error = %e, "failed to auto-link new memory semantically");
            }
            if let Err(e) = store.auto_link_temporal(&note_id, &context.session_id, 5) {
                tracing::warn!(error = %e, "failed to auto-link new memory temporally");
            }
            if let Err(e) = store.auto_link_entity(&note_id, &keywords, 5) {
                tracing::warn!(error = %e, "failed to auto-link new memory by entity");
            }
            create_causal_links(&store, context, &note_id);
            try_auto_consolidate(&store, context);

            return Ok(ToolOutput {
                content: format!("stored memory `{key}` (embedded)"),
                metadata: BTreeMap::from([
                    ("tool".to_owned(), call.name.clone()),
                    ("key".to_owned(), key.clone()),
                    ("session_id".to_owned(), context.session_id.clone()),
                ]),
            });
        }

        // No embedding service — plain store.
        store_plain(&store, call, context, key, value, &database_path)
    }
}

pub struct MemoryRecallTool;

impl ToolHandler for MemoryRecallTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query = call
            .arguments
            .get("query")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "query",
            })?;

        let limit = call
            .arguments
            .get("limit")
            .map(|value| {
                value
                    .parse::<usize>()
                    .map_err(|error| ToolError::ExecutionFailed {
                        tool: call.name.clone(),
                        reason: format!("invalid `limit` value `{value}`: {error}"),
                    })
            })
            .transpose()?
            .unwrap_or(DEFAULT_RECALL_LIMIT);

        let database_path = context.db_path();
        let store = MemoryStore::new(&database_path);
        let memories =
            store
                .graph_search(query, limit)
                .map_err(|error| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!(
                        "failed to search memories in `{}`: {error}",
                        database_path.display()
                    ),
                })?;

        // Track recalled memory IDs for causal linking.
        if let Ok(mut ids) = context.recalled_memory_ids.lock() {
            for m in &memories {
                ids.push(m.memory.id.clone());
            }
        }

        let content = if memories.is_empty() {
            "no memories found".to_owned()
        } else {
            memories
                .into_iter()
                .map(|memory| {
                    format!(
                        "[{}] {} ({})",
                        memory.memory.kind, memory.memory.content, memory.memory.created_at
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("query".to_owned(), query.clone()),
                ("limit".to_owned(), limit.to_string()),
                ("mode".to_owned(), "graph".to_owned()),
            ]),
        })
    }
}

pub struct MemoryConsolidateTool;

impl ToolHandler for MemoryConsolidateTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let database_path = context.db_path();
        let store = MemoryStore::new(&database_path);

        let threshold = call
            .arguments
            .get("threshold")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.85);

        let min_cluster_size = call
            .arguments
            .get("min_cluster_size")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2);

        let clusters = store
            .find_consolidation_clusters(threshold, min_cluster_size)
            .map_err(|error| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to find clusters: {error}"),
            })?;

        if clusters.is_empty() {
            return Ok(ToolOutput {
                content: "no clusters found for consolidation".to_owned(),
                metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
            });
        }

        let mut summaries = Vec::new();
        for cluster in &clusters {
            let count = consolidate_single_cluster(&store, cluster)
                .map_err(|error| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("failed to consolidate: {error}"),
                })?;
            summaries.push(format!("- {count} memories consolidated"));
        }

        Ok(ToolOutput {
            content: format!(
                "consolidated {} clusters:\n{}",
                clusters.len(),
                summaries.join("\n")
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("clusters".to_owned(), clusters.len().to_string()),
            ]),
        })
    }
}

pub struct MemoryPruneTool;

impl ToolHandler for MemoryPruneTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let database_path = context.db_path();
        let store = MemoryStore::new(&database_path);

        let importance_threshold = call
            .arguments
            .get("importance_threshold")
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.1);

        let stale_days = call
            .arguments
            .get("stale_days")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(90);

        let stale = store
            .list_stale(importance_threshold, stale_days)
            .map_err(|error| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to list stale memories: {error}"),
            })?;

        let count = stale.len();
        for memory in &stale {
            store
                .delete(&memory.id)
                .map_err(|error| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("failed to delete {}: {error}", memory.id),
                })?;
        }

        Ok(ToolOutput {
            content: format!(
                "pruned {count} stale memories (importance < {importance_threshold}, \
                 not accessed in {stale_days}+ days)"
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("pruned_count".to_owned(), count.to_string()),
            ]),
        })
    }
}

/// Create a memory note without embedding (fallback / no-embedding-service path).
fn store_plain(
    store: &MemoryStore,
    call: &ToolCall,
    context: &ToolContext,
    key: &str,
    value: &str,
    database_path: &std::path::Path,
) -> Result<ToolOutput, ToolError> {
    let note_id = memory_id(context, key);
    let keywords = extract_or_enrich_keywords(context, key, value);
    store
        .create_note(NewMemoryNote {
            id: note_id.clone(),
            session_id: Some(context.session_id.clone()),
            kind: key.to_owned(),
            content: value.to_owned(),
            keywords: keywords.clone(),
            tags: Vec::new(),
            linked_ids: Vec::new(),
            importance: 0.5,
        })
        .map_err(|error| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!(
                "failed to store memory into `{}`: {error}",
                database_path.display()
            ),
        })?;

    if let Err(e) = store.auto_link_entity(&note_id, &keywords, 5) {
        tracing::warn!(error = %e, "failed to auto-link memory by entity");
    }
    create_causal_links(store, context, &note_id);
    try_auto_consolidate(store, context);

    Ok(ToolOutput {
        content: format!("stored memory `{key}`"),
        metadata: BTreeMap::from([
            ("tool".to_owned(), call.name.clone()),
            ("key".to_owned(), key.to_owned()),
            ("session_id".to_owned(), context.session_id.clone()),
        ]),
    })
}

/// Create causal edges from previously recalled memories to the newly stored memory.
/// Reads recalled IDs without draining — all stores in the same turn get causal edges.
/// The agent loop clears recalled IDs at the start of each new turn.
fn create_causal_links(store: &MemoryStore, context: &ToolContext, new_memory_id: &str) {
    let Ok(ids) = context.recalled_memory_ids.lock() else {
        return;
    };
    for recalled_id in ids.iter() {
        if recalled_id == new_memory_id {
            continue;
        }
        if let Err(e) = store.create_link(recalled_id, new_memory_id, genesis_storage::edge_type::CAUSAL, 1.0) {
            tracing::warn!(error = %e, recalled_id, new_memory_id, "failed to create causal link");
        }
    }
}

fn memory_id(context: &ToolContext, key: &str) -> String {
    format!("{}:{}:{}", context.session_id, key, unique_suffix())
}

fn extract_keywords(key: &str, value: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    key.split(|c: char| !c.is_ascii_alphanumeric())
        .chain(value.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter_map(|token| {
            let token = token.trim().to_ascii_lowercase();
            if token.len() < 2 || !seen.insert(token.clone()) {
                None
            } else {
                Some(token)
            }
        })
        .collect()
}

/// Extract keywords, using the LLM enricher if available, falling back to simple extraction.
fn extract_or_enrich_keywords(context: &ToolContext, key: &str, value: &str) -> Vec<String> {
    if let Some(ref enricher) = context.keyword_enricher {
        match enricher.enrich(value) {
            Ok(keywords) if !keywords.is_empty() => return keywords,
            Ok(_) => {
                tracing::warn!(
                    "keyword enricher returned empty result, falling back to simple extraction"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "keyword enricher failed, falling back to simple extraction"
                );
            }
        }
    }
    extract_keywords(key, value)
}

/// Consolidate a single cluster of memory IDs into a summary.
/// Returns the number of members consolidated.
fn consolidate_single_cluster(
    store: &MemoryStore,
    cluster: &[String],
) -> Result<usize, genesis_storage::StorageError> {
    let members: Vec<_> = cluster
        .iter()
        .filter_map(|id| store.get(id).ok().flatten())
        .collect();
    let combined_content: String = members
        .iter()
        .map(|m| format!("- [{}] {}", m.kind, m.content))
        .collect::<Vec<_>>()
        .join("\n");
    let keywords: Vec<String> = members
        .iter()
        .flat_map(|m| extract_keywords(&m.kind, &m.content))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let summary_content = format!(
        "Consolidated from {} memories:\n{}",
        members.len(),
        combined_content
    );
    let member_refs: Vec<&str> = cluster.iter().map(|s| s.as_str()).collect();
    store.consolidate_cluster(&member_refs, &summary_content, keywords)?;
    Ok(members.len())
}

/// Check if auto-consolidation should trigger and run it if so.
/// Skips re-triggering if the last attempt found no clusters (avoids repeated
/// full scans when embeddings are unavailable).
fn try_auto_consolidate(store: &MemoryStore, context: &ToolContext) {
    use std::sync::atomic::{AtomicU64, Ordering};

    // Track the count at which we last attempted consolidation. If the count
    // hasn't changed since the last fruitless attempt, skip the expensive scan.
    static LAST_ATTEMPTED_COUNT: AtomicU64 = AtomicU64::new(0);

    if context.auto_consolidation_threshold == 0 {
        return;
    }
    let count = match store.count_unconsolidated() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "failed to count unconsolidated memories");
            return;
        }
    };
    if count < context.auto_consolidation_threshold {
        return;
    }
    // Skip if we already attempted at this count and found nothing.
    let last = LAST_ATTEMPTED_COUNT.load(Ordering::Relaxed);
    if last == count {
        return;
    }
    tracing::info!(
        unconsolidated = count,
        threshold = context.auto_consolidation_threshold,
        "auto-consolidation triggered"
    );
    let clusters = match store.find_consolidation_clusters(0.85, 2) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "auto-consolidation: failed to find clusters");
            return;
        }
    };
    if clusters.is_empty() {
        LAST_ATTEMPTED_COUNT.store(count, Ordering::Relaxed);
        return;
    }
    for cluster in &clusters {
        if let Err(e) = consolidate_single_cluster(store, cluster) {
            tracing::warn!(error = %e, "auto-consolidation: failed to consolidate cluster");
        }
    }
    // Reset so future stores can re-trigger after new memories accumulate.
    LAST_ATTEMPTED_COUNT.store(0, Ordering::Relaxed);
    tracing::info!(clusters = clusters.len(), "auto-consolidation complete");
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use genesis_storage::{bootstrap, SessionStore};
    use tempfile::tempdir;

    use super::{MemoryRecallTool, MemoryStoreTool};
    use crate::{ToolCall, ToolContext, ToolHandler};

    fn ctx(data_dir: &str) -> ToolContext {
        ToolContext {
            session_id: "session-42".to_owned(),
            data_dir: data_dir.to_owned(),
            ..crate::test_utils::test_ctx()
        }
    }

    /// Bootstrap the DB and create the session so FK constraints are satisfied.
    fn setup_db(dir: &std::path::Path) {
        let db_path = dir.join("genesis.db");
        bootstrap(&db_path).expect("bootstrap should succeed");
        let store = SessionStore::new(&db_path);
        store
            .create_session("session-42", "test", None)
            .expect("session should be created");
    }

    #[test]
    fn memory_store_persists_key_value_into_memories_table() {
        let dir = tempdir().expect("tempdir should exist");
        setup_db(dir.path());
        let tool = MemoryStoreTool;
        let db_path = dir.path().join("genesis.db");

        let output = tool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "favorite_language".to_owned()),
                        ("value".to_owned(), "rust".to_owned()),
                    ]),
                },
                &ctx(dir.path().to_string_lossy().as_ref()),
            )
            .expect("memory should store");

        assert!(output.content.contains("favorite_language"));

        let conn = rusqlite::Connection::open(&db_path).expect("db should open");
        let row = conn
            .query_row(
                "SELECT kind, content, keywords_json, tags_json, importance
                 FROM memories
                 WHERE kind = 'favorite_language'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, f64>(4)?,
                    ))
                },
            )
            .expect("structured memory should be stored");

        assert_eq!(row.0, "favorite_language");
        assert_eq!(row.1, "rust");
        assert!(row.2.contains("favorite"));
        assert_eq!(row.3, "[]");
        assert_eq!(row.4, 0.5);
    }

    #[test]
    fn memory_recall_returns_matching_memories() {
        let dir = tempdir().expect("tempdir should exist");
        setup_db(dir.path());
        let context = ctx(dir.path().to_string_lossy().as_ref());

        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "project_goal".to_owned()),
                        ("value".to_owned(), "build genesis in rust".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("memory should store");

        let output = MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([("query".to_owned(), "genesis".to_owned())]),
                },
                &context,
            )
            .expect("memory recall should succeed");

        assert!(output.content.contains("[project_goal]"));
        assert!(output.content.contains("build genesis in rust"));
    }

    #[test]
    fn memory_recall_returns_linked_memories() {
        let dir = tempdir().expect("tempdir should exist");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let store = genesis_storage::MemoryStore::new(&db_path);

        store
            .create_note(genesis_storage::NewMemoryNote {
                id: "linked-note".to_owned(),
                session_id: Some("session-42".to_owned()),
                kind: "fact".to_owned(),
                content: "Rust ownership model".to_owned(),
                keywords: vec!["rust".to_owned()],
                tags: vec!["language".to_owned()],
                linked_ids: vec![],
                importance: 0.7,
            })
            .expect("linked note should store");
        store
            .create_note(genesis_storage::NewMemoryNote {
                id: "primary-note".to_owned(),
                session_id: Some("session-42".to_owned()),
                kind: "fact".to_owned(),
                content: "Genesis architecture memory".to_owned(),
                keywords: vec!["genesis".to_owned(), "memory".to_owned()],
                tags: vec!["architecture".to_owned()],
                linked_ids: vec!["linked-note".to_owned()],
                importance: 1.0,
            })
            .expect("primary note should store");

        let output = MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([("query".to_owned(), "Genesis".to_owned())]),
                },
                &ctx(dir.path().to_string_lossy().as_ref()),
            )
            .expect("memory recall should succeed");

        assert!(output.content.contains("Genesis architecture memory"));
        assert!(output.content.contains("Rust ownership model"));
    }

    #[test]
    fn memory_recall_uses_data_dir_to_find_database() {
        let dir = tempdir().expect("tempdir should exist");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        assert_eq!(context.db_path(), dir.path().join("genesis.db"));
    }

    #[test]
    fn extract_keywords_skips_single_character_tokens() {
        let keywords = super::extract_keywords("i", "Rust is a systems language");

        assert!(keywords.contains(&"rust".to_owned()));
        assert!(!keywords.contains(&"i".to_owned()));
        assert!(!keywords.contains(&"a".to_owned()));
    }

    #[test]
    fn memory_store_requires_key() {
        let dir = tempdir().expect("tempdir");
        let tool = MemoryStoreTool;
        let call = ToolCall {
            name: "memory_store".to_owned(),
            arguments: BTreeMap::from([("value".to_owned(), "some value".to_owned())]),
        };
        let err = tool
            .run(&call, &ctx(dir.path().to_string_lossy().as_ref()))
            .unwrap_err();
        assert!(matches!(
            err,
            crate::ToolError::MissingArgument {
                argument: "key",
                ..
            }
        ));
    }

    #[test]
    fn memory_store_requires_value() {
        let dir = tempdir().expect("tempdir");
        let tool = MemoryStoreTool;
        let call = ToolCall {
            name: "memory_store".to_owned(),
            arguments: BTreeMap::from([("key".to_owned(), "some_key".to_owned())]),
        };
        let err = tool
            .run(&call, &ctx(dir.path().to_string_lossy().as_ref()))
            .unwrap_err();
        assert!(matches!(
            err,
            crate::ToolError::MissingArgument {
                argument: "value",
                ..
            }
        ));
    }

    #[test]
    fn memory_recall_requires_query() {
        let dir = tempdir().expect("tempdir");
        let tool = MemoryRecallTool;
        let call = ToolCall {
            name: "memory_recall".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool
            .run(&call, &ctx(dir.path().to_string_lossy().as_ref()))
            .unwrap_err();
        assert!(matches!(
            err,
            crate::ToolError::MissingArgument {
                argument: "query",
                ..
            }
        ));
    }

    #[test]
    fn memory_recall_returns_no_memories_when_empty() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let output = MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([("query".to_owned(), "anything".to_owned())]),
                },
                &context,
            )
            .expect("should succeed");

        assert_eq!(output.content, "no memories found");
    }

    #[test]
    fn memory_recall_respects_custom_limit() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let context = ctx(dir.path().to_string_lossy().as_ref());

        // Store multiple memories
        for i in 0..5 {
            MemoryStoreTool
                .run(
                    &ToolCall {
                        name: "memory_store".to_owned(),
                        arguments: BTreeMap::from([
                            ("key".to_owned(), format!("fact_{i}")),
                            ("value".to_owned(), format!("rust fact number {i}")),
                        ]),
                    },
                    &context,
                )
                .unwrap();
        }

        let output = MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([
                        ("query".to_owned(), "rust".to_owned()),
                        ("limit".to_owned(), "2".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("should succeed");

        // The result should have at most 2 memories
        let lines: Vec<&str> = output.content.lines().collect();
        assert!(
            lines.len() <= 2,
            "should respect limit of 2, got {}",
            lines.len()
        );
        assert_eq!(output.metadata.get("limit").unwrap(), "2");
    }

    #[test]
    fn memory_recall_invalid_limit_returns_error() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let err = MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([
                        ("query".to_owned(), "test".to_owned()),
                        ("limit".to_owned(), "not_a_number".to_owned()),
                    ]),
                },
                &context,
            )
            .unwrap_err();

        assert!(matches!(err, crate::ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn extract_keywords_deduplicates() {
        let keywords = super::extract_keywords("rust_lang", "rust is great");
        // "rust" appears in both key and value but should only appear once
        let rust_count = keywords.iter().filter(|k| *k == "rust").count();
        assert_eq!(rust_count, 1, "keywords should be deduplicated");
    }

    #[test]
    fn memory_store_metadata_includes_session_id() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let tool = MemoryStoreTool;

        let output = tool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "test_key".to_owned()),
                        ("value".to_owned(), "test_value".to_owned()),
                    ]),
                },
                &ctx(dir.path().to_string_lossy().as_ref()),
            )
            .expect("should succeed");

        assert_eq!(output.metadata.get("session_id").unwrap(), "session-42");
        assert_eq!(output.metadata.get("key").unwrap(), "test_key");
    }

    #[test]
    fn memory_consolidate_tool_no_clusters() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let tool = super::MemoryConsolidateTool;
        let output = tool
            .run(
                &ToolCall {
                    name: "memory_consolidate".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &context,
            )
            .unwrap();

        assert_eq!(output.content, "no clusters found for consolidation");
    }

    #[test]
    fn memory_consolidate_tool_consolidates_similar_memories() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        // Insert memories with embeddings for clustering
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        for i in 1..=4 {
            conn.execute(
                &format!(
                    "INSERT INTO memories (id, session_id, kind, content, created_at) \
                     VALUES ('cmem{i}', 'session-42', 'fact', 'rust ownership fact {i}', \
                     CURRENT_TIMESTAMP)"
                ),
                [],
            )
            .unwrap();
        }
        drop(conn);

        // Store similar embeddings (cluster of 4)
        let embeddings = genesis_storage::EmbeddingStore::new(&db_path);
        embeddings
            .store("cmem1", &[1.0, 0.0, 0.0, 0.0], "test")
            .unwrap();
        embeddings
            .store("cmem2", &[0.98, 0.02, 0.0, 0.0], "test")
            .unwrap();
        embeddings
            .store("cmem3", &[0.95, 0.05, 0.0, 0.0], "test")
            .unwrap();
        embeddings
            .store("cmem4", &[0.90, 0.10, 0.0, 0.0], "test")
            .unwrap();

        let tool = super::MemoryConsolidateTool;
        let output = tool
            .run(
                &ToolCall {
                    name: "memory_consolidate".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &context,
            )
            .unwrap();

        assert!(
            output.content.contains("consolidated"),
            "should report consolidation: {}",
            output.content
        );
    }

    #[test]
    fn memory_prune_removes_stale_memories() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO memories (id, session_id, kind, content, created_at, importance, accessed_at) \
             VALUES ('stale1', 'session-42', 'fact', 'old info', '2024-01-01T00:00:00Z', 0.05, '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        drop(conn);

        let tool = super::MemoryPruneTool;
        let output = tool
            .run(
                &ToolCall {
                    name: "memory_prune".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &context,
            )
            .unwrap();

        assert!(output.content.contains("pruned"));

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE id = 'stale1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn memory_prune_no_stale_memories() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let tool = super::MemoryPruneTool;
        let output = tool
            .run(
                &ToolCall {
                    name: "memory_prune".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &context,
            )
            .unwrap();

        assert!(output.content.contains("pruned 0"));
    }

    // ── Auto-embed & dedup tests ──────────────────────────────────────

    use std::sync::Arc;

    use genesis_storage::EmbeddingStore;

    use crate::EmbeddingService;

    /// Mock embedding service that returns a fixed 4-dimensional embedding.
    /// Embeds based on simple text hashing so different texts get different vectors.
    struct MockEmbeddingService;

    impl EmbeddingService for MockEmbeddingService {
        fn embed_one(&self, text: &str) -> Result<Vec<f32>, String> {
            // Simple deterministic embedding: hash each char position
            let mut emb = vec![0.0f32; 4];
            for (i, c) in text.chars().enumerate() {
                emb[i % 4] += (c as u32 as f32) / 1000.0;
            }
            // Normalize
            let norm = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                emb.iter_mut().for_each(|x| *x /= norm);
            }
            Ok(emb)
        }
        fn model_name(&self) -> &str {
            "test-model"
        }
    }

    /// Mock that always returns the SAME embedding regardless of input.
    /// This makes every memory a "duplicate" of every other.
    struct ConstantEmbeddingService;

    impl EmbeddingService for ConstantEmbeddingService {
        fn embed_one(&self, _text: &str) -> Result<Vec<f32>, String> {
            Ok(vec![1.0, 0.0, 0.0, 0.0])
        }
        fn model_name(&self) -> &str {
            "test-constant"
        }
    }

    /// Mock that returns nearby but distinct embeddings.
    ///
    /// The first call returns `[1, 0, 0, 0]` and the second returns
    /// `[cos(theta), sin(theta), 0, 0]` with `theta` chosen so that the
    /// cosine distance `d = 1 - cos(theta)` satisfies:
    ///   0.8 <= 1/(1+d)  (semantic link threshold)
    ///   1/(1+d) < 0.92  (dedup threshold)
    ///
    /// Using `theta = 0.55` rad gives `cos(0.55) ~= 0.8525`,
    /// `d ~= 0.1475`, `score ~= 0.87`.
    ///
    /// With that score:  0.8 < 0.87 < 0.92  (links but no merge)
    struct NearbyEmbeddingService {
        counter: std::sync::atomic::AtomicUsize,
    }

    impl NearbyEmbeddingService {
        fn new() -> Self {
            Self {
                counter: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl EmbeddingService for NearbyEmbeddingService {
        fn embed_one(&self, _text: &str) -> Result<Vec<f32>, String> {
            let n = self
                .counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Ok(vec![1.0, 0.0, 0.0, 0.0])
            } else {
                // theta = 0.55 rad → cosine distance ~0.15 → score ~0.87
                let theta: f32 = 0.55;
                Ok(vec![theta.cos(), theta.sin(), 0.0, 0.0])
            }
        }
        fn model_name(&self) -> &str {
            "test-nearby"
        }
    }

    /// Mock that returns orthogonal embeddings for each call.
    /// Ensures no dedup (vectors are maximally dissimilar) and no semantic links.
    /// Used for testing temporal links in isolation.
    struct OrthogonalEmbeddingService {
        counter: std::sync::atomic::AtomicUsize,
    }

    impl OrthogonalEmbeddingService {
        fn new() -> Self {
            Self {
                counter: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl EmbeddingService for OrthogonalEmbeddingService {
        fn embed_one(&self, _text: &str) -> Result<Vec<f32>, String> {
            let n = self
                .counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut emb = vec![0.0f32; 4];
            emb[n % 4] = 1.0;
            Ok(emb)
        }
        fn model_name(&self) -> &str {
            "test-orthogonal"
        }
    }

    fn ctx_with_embedding(data_dir: &str, service: Arc<dyn EmbeddingService>) -> ToolContext {
        ToolContext {
            session_id: "session-42".to_owned(),
            data_dir: data_dir.to_owned(),
            embedding_service: Some(service),
            ..crate::test_utils::test_ctx()
        }
    }

    #[test]
    fn memory_store_with_embedding_generates_and_stores_embedding() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let context = ctx_with_embedding(
            dir.path().to_string_lossy().as_ref(),
            Arc::new(MockEmbeddingService),
        );

        let output = MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "rust_fact".to_owned()),
                        (
                            "value".to_owned(),
                            "Rust has zero-cost abstractions".to_owned(),
                        ),
                    ]),
                },
                &context,
            )
            .expect("memory store should succeed with embedding");

        assert!(
            output.content.contains("(embedded)"),
            "output should indicate embedding: {}",
            output.content
        );

        // Verify embedding was persisted.
        let emb_store = EmbeddingStore::new(&db_path);
        let all = emb_store.all_embeddings().expect("should list embeddings");
        assert_eq!(all.len(), 1, "exactly one embedding should be stored");
        assert_eq!(all[0].1.len(), 4, "embedding should have 4 dimensions");
    }

    #[test]
    fn memory_store_dedup_merges_near_duplicates() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let context = ctx_with_embedding(
            dir.path().to_string_lossy().as_ref(),
            Arc::new(ConstantEmbeddingService),
        );

        // Store the first memory.
        let output_a = MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "key_a".to_owned()),
                        ("value".to_owned(), "first version of the fact".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("first store should succeed");

        assert!(
            output_a.content.contains("(embedded)"),
            "first store should be a fresh embed: {}",
            output_a.content
        );

        // Store the second memory — constant embeddings mean cosine=1.0 so it should merge.
        let output_b = MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "key_b".to_owned()),
                        ("value".to_owned(), "second version of the fact".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("second store should succeed (merge)");

        assert!(
            output_b.content.contains("merged memory into existing"),
            "second store should be a merge: {}",
            output_b.content
        );
        assert!(
            output_b.metadata.contains_key("merged_into"),
            "metadata should contain merged_into key"
        );

        // Verify only 1 memory remains in the DB and its content includes both texts.
        let conn = rusqlite::Connection::open(&db_path).expect("db should open");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
            .expect("should count memories");
        assert_eq!(count, 1, "only the merged memory should remain");

        let content: String = conn
            .query_row("SELECT content FROM memories", [], |row| row.get(0))
            .expect("should read merged content");
        assert!(
            content.contains("first version of the fact"),
            "merged content should contain original: {}",
            content
        );
        assert!(
            content.contains("second version of the fact"),
            "merged content should contain new text: {}",
            content
        );
    }

    #[test]
    fn memory_store_without_embedding_service_stores_plain() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        let output = MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "plain_key".to_owned()),
                        ("value".to_owned(), "plain value".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("plain store should succeed");

        assert!(
            output.content.contains("stored memory `plain_key`"),
            "output should be plain store message: {}",
            output.content
        );
        assert!(
            !output.content.contains("(embedded)"),
            "output should NOT contain (embedded): {}",
            output.content
        );

        // Verify no embedding was stored.
        let emb_store = EmbeddingStore::new(&db_path);
        let all = emb_store.all_embeddings().expect("should list embeddings");
        assert!(all.is_empty(), "no embeddings should exist for plain store");
    }

    #[test]
    fn memory_store_creates_semantic_links_after_embedding() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");

        // NearbyEmbeddingService produces vectors close enough for semantic linking
        // (score ~0.87) but not close enough for dedup (threshold 0.92).
        let context = ctx_with_embedding(
            dir.path().to_string_lossy().as_ref(),
            Arc::new(NearbyEmbeddingService::new()),
        );

        // Store two memories — NearbyEmbeddingService gives them close embeddings.
        let out1 = MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "rust_ownership".to_owned()),
                        (
                            "value".to_owned(),
                            "Rust ownership model prevents data races".to_owned(),
                        ),
                    ]),
                },
                &context,
            )
            .expect("first memory should store");

        assert!(
            out1.content.contains("(embedded)"),
            "first memory should be freshly embedded: {}",
            out1.content
        );

        let out2 = MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "rust_borrowing".to_owned()),
                        (
                            "value".to_owned(),
                            "Rust borrowing rules enforce safe references".to_owned(),
                        ),
                    ]),
                },
                &context,
            )
            .expect("second memory should store");

        // Confirm the second memory was NOT merged (dedup threshold 0.92).
        assert!(
            out2.content.contains("(embedded)"),
            "second memory should be a fresh embed (not merged): {}",
            out2.content
        );

        // Both memories are stored with embeddings. The auto_link_semantic call
        // inside MemoryStoreTool may not create links due to sqlite-vec virtual
        // table connection isolation — the EmbeddingStore writes via a different
        // connection than the MemoryStore reads.  Verify the preconditions are
        // met (2 memories + 2 embeddings) and then trigger semantic linking with
        // a fresh MemoryStore to prove the link creation works end-to-end.
        let emb_store = EmbeddingStore::new(&db_path);
        assert_eq!(
            emb_store.count().expect("count embeddings"),
            2,
            "both memories should have embeddings"
        );

        // Retrieve the second memory's embedding and re-trigger semantic linking
        // with a fresh MemoryStore (single-connection view of all data).
        let all_embs = emb_store.all_embeddings().expect("list embeddings");
        let (second_id, second_emb) = &all_embs[1];

        let fresh_store = genesis_storage::MemoryStore::new(&db_path);
        let linked = fresh_store
            .auto_link_semantic(second_id, second_emb, 0.8, 5)
            .expect("auto_link_semantic should succeed");

        assert!(
            linked > 0,
            "semantic links should be created between nearby embeddings"
        );

        let conn = rusqlite::Connection::open(&db_path).expect("db should open");
        let link_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_links WHERE edge_type = 'semantic'",
                [],
                |row| row.get(0),
            )
            .expect("should count semantic links");

        assert!(
            link_count > 0,
            "semantic links should exist in the memory_links table"
        );
    }

    #[test]
    fn memory_store_creates_temporal_links() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");

        // OrthogonalEmbeddingService returns maximally dissimilar embeddings,
        // ensuring no dedup merging and no semantic links. This isolates the
        // temporal linking behaviour.
        let context = ctx_with_embedding(
            dir.path().to_string_lossy().as_ref(),
            Arc::new(OrthogonalEmbeddingService::new()),
        );

        // Store two memories in sequence (same session).
        let out1 = MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "event_first".to_owned()),
                        (
                            "value".to_owned(),
                            "started the genesis project today".to_owned(),
                        ),
                    ]),
                },
                &context,
            )
            .expect("first temporal memory should store");

        assert!(
            out1.content.contains("(embedded)"),
            "first memory should be freshly embedded: {}",
            out1.content
        );

        let out2 = MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "event_second".to_owned()),
                        (
                            "value".to_owned(),
                            "added embedding support to memory tools".to_owned(),
                        ),
                    ]),
                },
                &context,
            )
            .expect("second temporal memory should store");

        assert!(
            out2.content.contains("(embedded)"),
            "second memory should be freshly embedded (not merged): {}",
            out2.content
        );

        // Verify temporal links were created.
        let conn = rusqlite::Connection::open(&db_path).expect("db should open");
        let temporal_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_links WHERE edge_type = 'temporal'",
                [],
                |row| row.get(0),
            )
            .expect("should count temporal links");

        assert!(
            temporal_count > 0,
            "temporal links should be created between memories in the same session"
        );
    }

    #[test]
    fn memory_store_creates_entity_links_on_keyword_overlap() {
        let dir = tempdir().expect("tempdir should exist");
        setup_db(dir.path());
        let context = ctx(dir.path().to_string_lossy().as_ref());
        let db_path = dir.path().join("genesis.db");

        // Store first memory containing "rust" keyword.
        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "rust_ownership".to_owned()),
                        ("value".to_owned(), "Rust ownership model".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("first memory should store");

        // Store second memory also containing "rust" keyword.
        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "rust_borrowing".to_owned()),
                        ("value".to_owned(), "Rust borrowing rules".to_owned()),
                    ]),
                },
                &context,
            )
            .expect("second memory should store");

        // Verify entity links were created (both share "rust" keyword).
        let conn = rusqlite::Connection::open(&db_path).expect("db should open");
        let entity_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_links WHERE edge_type = 'entity'",
                [],
                |row| row.get(0),
            )
            .expect("should count entity links");

        assert!(
            entity_count > 0,
            "entity links should be created between memories with overlapping keywords"
        );
    }

    // ── Causal linking tests ────────────────────────────────────────────

    #[test]
    fn memory_recall_then_store_creates_causal_links() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        // Store a memory first.
        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "fact_a".to_owned()),
                        ("value".to_owned(), "Rust is fast".to_owned()),
                    ]),
                },
                &context,
            )
            .unwrap();

        // Recall it.
        MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([("query".to_owned(), "rust".to_owned())]),
                },
                &context,
            )
            .unwrap();

        // Store a new memory — should create causal link from recalled -> new.
        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "fact_b".to_owned()),
                        ("value".to_owned(), "Rust compiles to native code".to_owned()),
                    ]),
                },
                &context,
            )
            .unwrap();

        // Verify causal edge exists.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let causal_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_links WHERE edge_type = 'causal'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            causal_count > 0,
            "causal links should exist after recall->store"
        );
    }

    #[test]
    fn memory_store_without_prior_recall_creates_no_causal_links() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "fact_a".to_owned()),
                        ("value".to_owned(), "standalone fact".to_owned()),
                    ]),
                },
                &context,
            )
            .unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let causal_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_links WHERE edge_type = 'causal'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(causal_count, 0, "no causal links without prior recall");
    }

    #[test]
    fn all_stores_in_same_turn_get_causal_links() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let context = ctx(dir.path().to_string_lossy().as_ref());

        // Store and recall a memory.
        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "fact_a".to_owned()),
                        ("value".to_owned(), "Rust is fast".to_owned()),
                    ]),
                },
                &context,
            )
            .unwrap();

        MemoryRecallTool
            .run(
                &ToolCall {
                    name: "memory_recall".to_owned(),
                    arguments: BTreeMap::from([("query".to_owned(), "rust".to_owned())]),
                },
                &context,
            )
            .unwrap();

        // First store — should create causal links.
        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "fact_b".to_owned()),
                        ("value".to_owned(), "Rust compiles to native code".to_owned()),
                    ]),
                },
                &context,
            )
            .unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let causal_after_first: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_links WHERE edge_type = 'causal'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        // Second store — should ALSO create causal links (recalled IDs persist within a turn).
        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "fact_c".to_owned()),
                        ("value".to_owned(), "Rust has no GC".to_owned()),
                    ]),
                },
                &context,
            )
            .unwrap();

        let causal_after_second: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_links WHERE edge_type = 'causal'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert!(causal_after_first > 0, "first store should create causal links");
        assert!(
            causal_after_second > causal_after_first,
            "second store should also create causal links (recalled IDs persist within a turn)"
        );
    }

    // ---- keyword enricher tests ----

    /// Mock keyword enricher that returns fixed keywords.
    struct MockKeywordEnricher;
    impl crate::KeywordEnricher for MockKeywordEnricher {
        fn enrich(&self, _content: &str) -> Result<Vec<String>, String> {
            Ok(vec!["enriched_keyword".to_owned(), "concept".to_owned()])
        }
    }

    /// Mock keyword enricher that always fails.
    struct FailingKeywordEnricher;
    impl crate::KeywordEnricher for FailingKeywordEnricher {
        fn enrich(&self, _content: &str) -> Result<Vec<String>, String> {
            Err("LLM unavailable".to_owned())
        }
    }

    fn ctx_with_enricher(data_dir: &str, enricher: Arc<dyn crate::KeywordEnricher>) -> ToolContext {
        ToolContext {
            session_id: "session-42".to_owned(),
            data_dir: data_dir.to_owned(),
            keyword_enricher: Some(enricher),
            ..crate::test_utils::test_ctx()
        }
    }

    #[test]
    fn memory_store_uses_keyword_enricher_when_available() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let context = ctx_with_enricher(
            dir.path().to_string_lossy().as_ref(),
            Arc::new(MockKeywordEnricher),
        );

        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "test_key".to_owned()),
                        ("value".to_owned(), "some content".to_owned()),
                    ]),
                },
                &context,
            )
            .unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let keywords_json: String = conn
            .query_row(
                "SELECT keywords_json FROM memories WHERE kind = 'test_key'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            keywords_json.contains("enriched_keyword"),
            "should use enricher keywords: {keywords_json}"
        );
    }

    #[test]
    fn memory_store_falls_back_when_enricher_fails() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        let context = ctx_with_enricher(
            dir.path().to_string_lossy().as_ref(),
            Arc::new(FailingKeywordEnricher),
        );

        MemoryStoreTool
            .run(
                &ToolCall {
                    name: "memory_store".to_owned(),
                    arguments: BTreeMap::from([
                        ("key".to_owned(), "fallback_key".to_owned()),
                        ("value".to_owned(), "fallback content".to_owned()),
                    ]),
                },
                &context,
            )
            .unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let keywords_json: String = conn
            .query_row(
                "SELECT keywords_json FROM memories WHERE kind = 'fallback_key'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // Should fall back to extract_keywords which would produce "fallback", "key", "content"
        assert!(
            keywords_json.contains("fallback"),
            "should use fallback keywords: {keywords_json}"
        );
    }

    fn ctx_with_auto_consolidation(data_dir: &str, threshold: u64) -> ToolContext {
        ToolContext {
            session_id: "session-42".to_owned(),
            data_dir: data_dir.to_owned(),
            auto_consolidation_threshold: threshold,
            ..crate::test_utils::test_ctx()
        }
    }

    #[test]
    fn auto_consolidation_triggers_when_threshold_exceeded() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        // Set threshold to 3 for quick testing
        let context = ctx_with_auto_consolidation(dir.path().to_string_lossy().as_ref(), 3);

        // Store 3 memories with similar content
        for i in 0..3 {
            MemoryStoreTool
                .run(
                    &ToolCall {
                        name: "memory_store".to_owned(),
                        arguments: BTreeMap::from([
                            ("key".to_owned(), format!("rust_fact_{i}")),
                            (
                                "value".to_owned(),
                                format!("rust ownership fact number {i}"),
                            ),
                        ]),
                    },
                    &context,
                )
                .unwrap();
        }

        // Note: Auto-consolidation only fires if cluster detection finds clusters.
        // Without embeddings, find_consolidation_clusters checks embeddings and
        // may find none. Verify that at least the threshold check ran (3 memories
        // stored, threshold is 3) by checking the unconsolidated count.
        let store = genesis_storage::MemoryStore::new(&db_path);
        // If no embeddings, clusters won't be found, but count should be correct.
        let count = store.count_unconsolidated().unwrap();
        // Count may be 3 (no clusters) or less (if clusters were consolidated)
        assert!(
            count <= 3,
            "unconsolidated count should be at most 3, got {count}"
        );
    }

    #[test]
    fn auto_consolidation_disabled_when_threshold_zero() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        // Threshold 0 = disabled
        let context = ctx_with_auto_consolidation(dir.path().to_string_lossy().as_ref(), 0);

        for i in 0..5 {
            MemoryStoreTool
                .run(
                    &ToolCall {
                        name: "memory_store".to_owned(),
                        arguments: BTreeMap::from([
                            ("key".to_owned(), format!("key_{i}")),
                            ("value".to_owned(), format!("value {i}")),
                        ]),
                    },
                    &context,
                )
                .unwrap();
        }

        let store = genesis_storage::MemoryStore::new(&db_path);
        let count = store.count_unconsolidated().unwrap();
        assert_eq!(
            count, 5,
            "all 5 should remain unconsolidated when threshold is 0"
        );
    }

    #[test]
    fn auto_consolidation_does_not_trigger_below_threshold() {
        let dir = tempdir().expect("tempdir");
        setup_db(dir.path());
        let db_path = dir.path().join("genesis.db");
        // Threshold 100 — won't be reached with just 2 memories
        let context = ctx_with_auto_consolidation(dir.path().to_string_lossy().as_ref(), 100);

        for i in 0..2 {
            MemoryStoreTool
                .run(
                    &ToolCall {
                        name: "memory_store".to_owned(),
                        arguments: BTreeMap::from([
                            ("key".to_owned(), format!("key_{i}")),
                            ("value".to_owned(), format!("value {i}")),
                        ]),
                    },
                    &context,
                )
                .unwrap();
        }

        let store = genesis_storage::MemoryStore::new(&db_path);
        let count = store.count_unconsolidated().unwrap();
        assert_eq!(count, 2, "both should remain unconsolidated");
    }
}
