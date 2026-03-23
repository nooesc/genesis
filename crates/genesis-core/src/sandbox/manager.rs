// NOTE: SandboxManager is wired in via ToolContext::sandbox_manager.
// It provides caching, persistence, and lifecycle management for sandbox instances.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use genesis_storage::SandboxStore;

#[cfg(test)]
use super::BackendSpecific;
use super::{ExecResult, SandboxBackend, SandboxConfig, SandboxError, SandboxInstance};

pub struct SandboxManager {
    cache: Mutex<HashMap<String, SandboxInstance>>,
    store: SandboxStore,
    lifetime_seconds: u64,
}

impl SandboxManager {
    pub fn new(store: SandboxStore, lifetime_seconds: u64) -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
            store,
            lifetime_seconds,
        }
    }

    pub async fn execute(
        &self,
        backend: Arc<dyn SandboxBackend>,
        config: &SandboxConfig,
        command: &str,
        working_dir: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<ExecResult, SandboxError> {
        // Step 1 & 2: Check cache for existing instance.
        let instance = {
            let cache = self.cache.lock().await;
            cache.get(&config.task_id).cloned()
        };

        let instance = if let Some(existing) = instance {
            debug!(task_id = %config.task_id, "reusing cached sandbox instance");
            existing
        } else {
            // Step 3: Check store for a saved snapshot.
            let saved = self
                .store
                .find_by_task(backend.backend_type(), &config.task_id)
                .map_err(|e| SandboxError::StorageError(e.to_string()))?;

            let mut effective_config = config.clone();
            if let Some(row) = saved {
                debug!(
                    task_id = %config.task_id,
                    id = %row.id,
                    "resuming sandbox from stored snapshot"
                );
                // Step 4: Restore snapshot data into config so the backend can resume.
                effective_config.snapshot_data = row.snapshot_data;
            }

            // Step 4: Create the sandbox (fresh or from snapshot).
            let new_instance = backend.create(&effective_config).await?;
            info!(
                task_id = %config.task_id,
                id = %new_instance.id,
                backend = backend.backend_type(),
                "sandbox created"
            );

            // Persist record to store.
            self.store
                .upsert(
                    &new_instance.id,
                    backend.backend_type(),
                    &config.task_id,
                    new_instance.snapshot_data.as_deref(),
                )
                .map_err(|e| SandboxError::StorageError(e.to_string()))?;

            // Insert into cache.
            {
                let mut cache = self.cache.lock().await;
                cache.insert(config.task_id.clone(), new_instance.clone());
            }

            new_instance
        };

        // Step 5: Execute the command.
        let result = backend
            .execute(&instance, command, working_dir, timeout)
            .await?;

        // Step 6: Update cache_instant and store last_active.
        {
            let mut cache = self.cache.lock().await;
            if let Some(cached) = cache.get_mut(&config.task_id) {
                cached.cache_instant = Instant::now();
                cached.last_active = SystemTime::now();
            }
        }
        if let Err(e) = self.store.update_activity(&instance.id) {
            warn!(id = %instance.id, error = %e, "failed to update sandbox activity");
        }

        Ok(result)
    }

    /// Snapshot a sandbox instance and then clean it up, logging any errors.
    async fn snapshot_and_cleanup(
        &self,
        backend: &dyn SandboxBackend,
        instance: &SandboxInstance,
        persistent: bool,
        context: &str,
    ) {
        match backend.snapshot(instance).await {
            Ok(Some(snap)) => {
                if let Err(e) = self.store.upsert(
                    &instance.id,
                    backend.backend_type(),
                    &instance.task_id,
                    Some(&snap),
                ) {
                    error!(id = %instance.id, error = %e, "failed to persist snapshot on {context}");
                }
            }
            Ok(None) => {
                debug!(id = %instance.id, "no snapshot data on {context}");
            }
            Err(e) => {
                error!(id = %instance.id, error = %e, "snapshot failed on {context}");
            }
        }

        if let Err(e) = backend.cleanup(instance, persistent).await {
            error!(id = %instance.id, error = %e, "cleanup failed on {context}");
        } else {
            debug!(id = %instance.id, "sandbox cleaned up on {context}");
        }
    }

    pub async fn shutdown(&self, backend: Arc<dyn SandboxBackend>) {
        let instances: Vec<SandboxInstance> = {
            let mut cache = self.cache.lock().await;
            cache.drain().map(|(_, v)| v).collect()
        };

        info!(
            count = instances.len(),
            "shutting down all sandbox instances"
        );

        for instance in instances {
            self.snapshot_and_cleanup(&*backend, &instance, true, "shutdown")
                .await;
        }
    }

    pub async fn cleanup_idle(&self, backend: Arc<dyn SandboxBackend>) {
        let idle_threshold = Duration::from_secs(self.lifetime_seconds);

        let idle_instances: Vec<SandboxInstance> = {
            let mut cache = self.cache.lock().await;
            let idle_keys: Vec<String> = cache
                .iter()
                .filter(|(_, v)| Instant::now().duration_since(v.cache_instant) > idle_threshold)
                .map(|(k, _)| k.clone())
                .collect();
            idle_keys
                .into_iter()
                .filter_map(|k| cache.remove(&k))
                .collect()
        };

        if idle_instances.is_empty() {
            return;
        }

        info!(
            count = idle_instances.len(),
            "cleaning up idle sandbox instances"
        );

        for instance in idle_instances {
            self.snapshot_and_cleanup(&*backend, &instance, instance.persistent, "idle cleanup")
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    struct MockBackend {
        create_count: AtomicUsize,
        cleanup_count: AtomicUsize,
    }

    impl MockBackend {
        fn new() -> Self {
            Self {
                create_count: AtomicUsize::new(0),
                cleanup_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl SandboxBackend for MockBackend {
        async fn create(&self, config: &SandboxConfig) -> Result<SandboxInstance, SandboxError> {
            self.create_count.fetch_add(1, Ordering::SeqCst);
            Ok(SandboxInstance {
                id: format!("mock-{}", config.task_id),
                backend_type: "mock".to_owned(),
                task_id: config.task_id.clone(),
                snapshot_data: config.working_dir.clone(), // reuse field for test snapshot passthrough
                persistent: config.persistent,
                created_at: SystemTime::now(),
                last_active: SystemTime::now(),
                cache_instant: Instant::now(),
            })
        }

        async fn execute(
            &self,
            _instance: &SandboxInstance,
            command: &str,
            _working_dir: Option<&str>,
            _timeout: Option<Duration>,
        ) -> Result<ExecResult, SandboxError> {
            Ok(ExecResult {
                output: format!("executed: {command}"),
                exit_code: 0,
            })
        }

        async fn snapshot(
            &self,
            _instance: &SandboxInstance,
        ) -> Result<Option<String>, SandboxError> {
            Ok(Some(r#"{"mock":"snapshot"}"#.to_owned()))
        }

        async fn cleanup(
            &self,
            _instance: &SandboxInstance,
            _persistent: bool,
        ) -> Result<(), SandboxError> {
            self.cleanup_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn backend_type(&self) -> &'static str {
            "mock"
        }
    }

    fn test_config(task_id: &str) -> SandboxConfig {
        SandboxConfig {
            task_id: task_id.to_owned(),
            image: "test:latest".to_owned(),
            cpu: 1.0,
            memory_mb: 5120,
            disk_mb: 51200,
            persistent: false,
            working_dir: None,
            snapshot_data: None,
            backend_specific: BackendSpecific::Singularity { bind: None },
        }
    }

    #[tokio::test]
    async fn manager_creates_sandbox_on_first_execute() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db).unwrap();
        let store = SandboxStore::new(&db);
        let backend = Arc::new(MockBackend::new());

        let manager = SandboxManager::new(store, 300);
        let config = test_config("session-1");
        let result = manager
            .execute(backend.clone(), &config, "echo hello", None, None)
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.output.contains("echo hello"));
        assert_eq!(backend.create_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn manager_reuses_cached_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db).unwrap();
        let store = SandboxStore::new(&db);
        let backend = Arc::new(MockBackend::new());

        let manager = SandboxManager::new(store, 300);
        let config = test_config("session-1");

        manager
            .execute(backend.clone(), &config, "first", None, None)
            .await
            .unwrap();
        manager
            .execute(backend.clone(), &config, "second", None, None)
            .await
            .unwrap();

        assert_eq!(backend.create_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn manager_shutdown_cleans_up_all_sandboxes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db).unwrap();
        let store = SandboxStore::new(&db);
        let backend = Arc::new(MockBackend::new());

        let manager = SandboxManager::new(store, 300);
        let config1 = test_config("session-1");
        let config2 = test_config("session-2");
        manager
            .execute(backend.clone(), &config1, "cmd1", None, None)
            .await
            .unwrap();
        manager
            .execute(backend.clone(), &config2, "cmd2", None, None)
            .await
            .unwrap();

        manager.shutdown(backend.clone()).await;
        assert_eq!(backend.cleanup_count.load(Ordering::SeqCst), 2);
    }
}
