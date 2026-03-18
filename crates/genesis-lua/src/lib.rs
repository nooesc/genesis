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
