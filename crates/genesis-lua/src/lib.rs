pub mod api;
pub mod discovery;
pub mod hooks;
pub mod manifest;
pub mod personality;
pub mod runtime;
pub mod tools;

pub use discovery::{discover_plugins, DiscoveredPlugin, PluginKind};
pub use manifest::{PluginGenesis, PluginManifest, PluginMetadata, PluginPermissions};
pub use runtime::{
    LuaRuntime, LuaRuntimeBuilder, LuaRuntimeConfig, LuaRuntimeError, LuaSessionContext,
};

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use serde_json::json;

    use crate::{LuaRuntimeConfig, LuaSessionContext};

    #[test]
    fn runtime_builder_is_constructible() {
        let runtime = crate::LuaRuntime::builder().build();
        assert!(runtime.is_ok());
    }

    #[test]
    fn runtime_builds_from_plugin_directory() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(dir.path().join("logger.lua"), "genesis.log('loaded')").expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");

        assert_eq!(runtime.plugin_names(), vec!["logger".to_owned()]);
    }

    #[test]
    fn runtime_exposes_genesis_version() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");

        let value = runtime
            .eval_string("return genesis.version")
            .expect("version should evaluate");
        assert_eq!(value, json!(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn runtime_exposes_session_metadata() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");

        let value = runtime
            .eval_string("return genesis.session.id")
            .expect("session id should evaluate");
        assert_eq!(value, json!("sess-1"));
    }

    #[test]
    fn runtime_exposes_config_get() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let runtime = test_runtime(
            dir.path(),
            BTreeMap::from([("profile".to_owned(), "default".to_owned())]),
        )
        .expect("runtime should build");

        let value = runtime
            .eval_string("return genesis.config.get('profile')")
            .expect("config lookup should evaluate");
        assert_eq!(value, json!("default"));
    }

    #[test]
    fn runtime_rejects_config_mutation() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");

        let err = runtime
            .eval_string("genesis.config.extra = 'boom'; return genesis.config.extra")
            .expect_err("config view should be read-only");
        assert!(
            err.to_string().contains("userdata")
                || err.to_string().contains("read-only")
                || err.to_string().contains("index"),
            "unexpected mutation error: {err}"
        );
    }

    #[test]
    fn runtime_loads_plugin_that_calls_genesis_log() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("logger.lua"),
            "genesis.log('plugin booted')\nreturn genesis.plugin_dir",
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");

        assert_eq!(runtime.plugin_names(), vec!["logger".to_owned()]);
        assert_eq!(
            runtime.logs(),
            vec!["plugin booted".to_owned()],
            "plugin should be able to call genesis.log during load"
        );
    }

    #[test]
    fn runtime_treats_missing_plugin_directory_as_empty() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let missing = dir.path().join("missing-plugins");

        let runtime = test_runtime(&missing, BTreeMap::new()).expect("runtime should still build");

        assert!(runtime.plugin_names().is_empty());
        assert!(runtime.plugin_errors().is_empty());
    }

    #[test]
    fn runtime_skips_broken_plugins_and_records_errors() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(dir.path().join("good.lua"), "genesis.log('good loaded')")
            .expect("good plugin should write");
        fs::write(dir.path().join("broken.lua"), "this is not valid lua(")
            .expect("broken plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

        assert_eq!(runtime.plugin_names(), vec!["good".to_owned()]);
        assert_eq!(runtime.logs(), vec!["good loaded".to_owned()]);
        assert_eq!(runtime.plugin_errors().len(), 1);
        assert!(
            runtime.plugin_errors()[0].contains("broken"),
            "broken plugin should be identified in recorded errors: {:?}",
            runtime.plugin_errors()
        );
    }

    #[test]
    fn runtime_skips_bad_manifest_and_loads_healthy_siblings() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(dir.path().join("good.lua"), "genesis.log('good loaded')")
            .expect("good plugin should write");

        let broken_dir = dir.path().join("broken-package");
        fs::create_dir(&broken_dir).expect("broken package dir should exist");
        fs::write(broken_dir.join("init.lua"), "return true").expect("init.lua should write");
        fs::write(
            broken_dir.join("plugin.toml"),
            r#"
[plugin]
name = "broken"
"#,
        )
        .expect("plugin.toml should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

        assert_eq!(runtime.plugin_names(), vec!["good".to_owned()]);
        assert_eq!(runtime.logs(), vec!["good loaded".to_owned()]);
        assert_eq!(runtime.plugin_errors().len(), 1);
        assert!(
            runtime.plugin_errors()[0].contains("plugin.toml"),
            "broken manifest should be identified in recorded errors: {:?}",
            runtime.plugin_errors()
        );
    }

    #[test]
    fn runtime_skips_duplicate_named_plugin_and_loads_unique_plugins() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(dir.path().join("alpha.lua"), "genesis.log('alpha loaded')")
            .expect("alpha plugin should write");
        fs::write(dir.path().join("gamma.lua"), "genesis.log('gamma loaded')")
            .expect("gamma plugin should write");

        let dup_dir = dir.path().join("beta-package");
        fs::create_dir(&dup_dir).expect("duplicate package dir should exist");
        fs::write(dup_dir.join("init.lua"), "return true").expect("init.lua should write");
        fs::write(
            dup_dir.join("plugin.toml"),
            r#"
[plugin]
name = "alpha"
version = "0.1.0"
"#,
        )
        .expect("plugin.toml should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

        assert_eq!(
            runtime.plugin_names(),
            vec!["alpha".to_owned(), "gamma".to_owned()],
        );
        assert_eq!(
            runtime.logs(),
            vec!["alpha loaded".to_owned(), "gamma loaded".to_owned()],
        );
        assert_eq!(runtime.plugin_errors().len(), 1);
        assert!(
            runtime.plugin_errors()[0].contains("duplicate plugin name `alpha`"),
            "duplicate plugin error should be recorded: {:?}",
            runtime.plugin_errors()
        );
    }

    #[test]
    fn runtime_isolates_plugin_globals_between_siblings() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(dir.path().join("first.lua"), "shared_value = 'leaked'").expect("plugin should write");
        fs::write(
            dir.path().join("second.lua"),
            "assert(shared_value == nil, 'global leaked across plugins')\ngenesis.log('second loaded')",
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

        assert_eq!(runtime.plugin_names(), vec!["first".to_owned(), "second".to_owned()]);
        assert_eq!(runtime.logs(), vec!["second loaded".to_owned()]);
        assert!(
            runtime.plugin_errors().is_empty(),
            "no plugin should fail when globals are isolated: {:?}",
            runtime.plugin_errors()
        );
    }

    #[test]
    fn runtime_isolates_shared_library_table_mutation_between_plugins() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("first.lua"),
            "string.plugin_marker = 'leaked'",
        )
        .expect("plugin should write");
        fs::write(
            dir.path().join("second.lua"),
            "assert(string.plugin_marker == nil, 'string table mutation leaked across plugins')\ngenesis.log('second loaded')",
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

        assert_eq!(runtime.plugin_names(), vec!["first".to_owned(), "second".to_owned()]);
        assert_eq!(runtime.logs(), vec!["second loaded".to_owned()]);
        assert!(
            runtime.plugin_errors().is_empty(),
            "shared library mutations should not leak across plugins: {:?}",
            runtime.plugin_errors()
        );
    }

    #[test]
    fn runtime_protects_genesis_root_from_plugin_mutation() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("mutator.lua"),
            "genesis.log = function(_) end",
        )
        .expect("plugin should write");
        fs::write(
            dir.path().join("observer.lua"),
            "genesis.log('observer loaded')",
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

        assert_eq!(runtime.plugin_names(), vec!["observer".to_owned()]);
        assert_eq!(runtime.logs(), vec!["observer loaded".to_owned()]);
        assert_eq!(runtime.plugin_errors().len(), 1);
        assert!(
            runtime.plugin_errors()[0].contains("read-only")
                || runtime.plugin_errors()[0].contains("protected")
                || runtime.plugin_errors()[0].contains("newindex")
                || runtime.plugin_errors()[0].contains("GenesisApi")
                || runtime.plugin_errors()[0].contains("userdata"),
            "mutation error should be recorded: {:?}",
            runtime.plugin_errors()
        );
    }

    fn test_runtime(
        plugin_dir: &std::path::Path,
        config_values: BTreeMap<String, String>,
    ) -> Result<crate::LuaRuntime, crate::LuaRuntimeError> {
        crate::LuaRuntime::builder()
            .with_config(LuaRuntimeConfig {
                plugin_dir: plugin_dir.to_path_buf(),
                session: LuaSessionContext {
                    id: "sess-1".to_owned(),
                    model: "gpt-5.4".to_owned(),
                    turn_count: 2,
                    total_tokens: 42,
                    platform: "cli".to_owned(),
                    personality: Some("default".to_owned()),
                },
                config_values,
            })
            .build()
    }
}
