use std::sync::{Arc, Mutex};

/// Maximum number of context entries across all plugins.
const MAX_CONTEXT_ENTRIES: usize = 100;
/// Maximum total content size across all entries (64 KB).
const MAX_CONTEXT_BYTES: usize = 64 * 1024;

/// A single context snippet contributed by a plugin.
#[derive(Debug, Clone)]
pub struct PluginContextEntry {
    pub plugin_name: String,
    pub content: String,
}

/// Thread-safe registry of plugin-contributed context snippets.
#[derive(Debug, Clone, Default)]
pub struct PluginContextRegistry {
    entries: Arc<Mutex<Vec<PluginContextEntry>>>,
}

impl PluginContextRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a context snippet. Silently drops entries that would exceed the
    /// entry-count or total-size cap.
    pub fn add(&self, plugin_name: &str, content: String) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if entries.len() >= MAX_CONTEXT_ENTRIES {
            tracing::warn!(
                plugin = plugin_name,
                "plugin context entry dropped — limit of {MAX_CONTEXT_ENTRIES} reached"
            );
            return;
        }
        let total_bytes: usize = entries.iter().map(|e| e.content.len()).sum();
        if total_bytes + content.len() > MAX_CONTEXT_BYTES {
            tracing::warn!(
                plugin = plugin_name,
                "plugin context entry dropped — total size would exceed {MAX_CONTEXT_BYTES} bytes"
            );
            return;
        }
        entries.push(PluginContextEntry {
            plugin_name: plugin_name.to_owned(),
            content,
        });
    }

    pub fn clear_for_plugin(&self, plugin_name: &str) {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|e| e.plugin_name != plugin_name);
    }

    pub fn entries(&self) -> Vec<PluginContextEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn build_section(&self) -> Option<String> {
        let entries = self.entries();
        if entries.is_empty() {
            return None;
        }
        let mut section = String::new();
        for entry in &entries {
            if !section.is_empty() {
                section.push_str("\n\n");
            }
            section.push_str(&format!(
                "### Plugin: {}\n{}",
                entry.plugin_name, entry.content
            ));
        }
        Some(section)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_inserts_entry() {
        let registry = PluginContextRegistry::new();
        registry.add("weather", "sunny 72F".to_owned());
        let entries = registry.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].plugin_name, "weather");
        assert_eq!(entries[0].content, "sunny 72F");
    }

    #[test]
    fn add_multiple_entries() {
        let registry = PluginContextRegistry::new();
        registry.add("weather", "sunny".to_owned());
        registry.add("stocks", "AAPL up 2%".to_owned());
        assert_eq!(registry.entries().len(), 2);
    }

    #[test]
    fn clear_for_plugin_removes_matching() {
        let registry = PluginContextRegistry::new();
        registry.add("weather", "sunny".to_owned());
        registry.add("stocks", "AAPL up".to_owned());
        registry.add("weather", "cloudy".to_owned());
        registry.clear_for_plugin("weather");
        let entries = registry.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].plugin_name, "stocks");
    }

    #[test]
    fn clear_for_plugin_noop_when_absent() {
        let registry = PluginContextRegistry::new();
        registry.add("weather", "sunny".to_owned());
        registry.clear_for_plugin("stocks");
        assert_eq!(registry.entries().len(), 1);
    }

    #[test]
    fn build_section_returns_none_when_empty() {
        let registry = PluginContextRegistry::new();
        assert!(registry.build_section().is_none());
    }

    #[test]
    fn build_section_single_entry() {
        let registry = PluginContextRegistry::new();
        registry.add("weather", "sunny 72F".to_owned());
        let section = registry.build_section().unwrap();
        assert_eq!(section, "### Plugin: weather\nsunny 72F");
    }

    #[test]
    fn build_section_multiple_entries() {
        let registry = PluginContextRegistry::new();
        registry.add("weather", "sunny".to_owned());
        registry.add("stocks", "AAPL up".to_owned());
        let section = registry.build_section().unwrap();
        assert!(section.contains("### Plugin: weather\nsunny"));
        assert!(section.contains("### Plugin: stocks\nAAPL up"));
        assert!(section.contains("\n\n"));
    }

    #[test]
    fn clone_shares_state() {
        let registry = PluginContextRegistry::new();
        let clone = registry.clone();
        registry.add("weather", "sunny".to_owned());
        assert_eq!(clone.entries().len(), 1);
    }
}
