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
pub use tools::{LuaHostToolExecutor, LuaRegisteredTool, LuaToolOutput, LuaToolRegistry};

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use crate::{LuaRuntimeConfig, LuaSessionContext};

    #[derive(Default)]
    struct TestHostToolExecutor {
        calls: Arc<Mutex<Vec<(String, BTreeMap<String, String>)>>>,
    }

    impl crate::tools::LuaHostToolExecutor for TestHostToolExecutor {
        fn execute(
            &self,
            tool_name: &str,
            arguments: BTreeMap<String, String>,
        ) -> Result<String, String> {
            self.calls
                .lock()
                .expect("host tool calls mutex should not be poisoned")
                .push((tool_name.to_owned(), arguments.clone()));
            Ok(match tool_name {
                "read_file" => format!(
                    "read:{}",
                    arguments.get("path").cloned().unwrap_or_default()
                ),
                "echo" => arguments.get("message").cloned().unwrap_or_default(),
                other => format!("host:{other}"),
            })
        }
    }

    #[test]
    fn runtime_builder_is_constructible() {
        let runtime = crate::LuaRuntime::builder().build();
        assert!(runtime.is_ok());
    }

    #[test]
    fn runtime_builds_from_plugin_directory() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(dir.path().join("logger.lua"), "genesis.log('loaded')")
            .expect("plugin should write");

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
    fn runtime_registers_and_runs_pre_turn_hook() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("hooks.lua"),
            r#"
genesis.on("PreTurn", function(ctx)
    return "rewritten: " .. ctx.user_message
end)
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");

        let outcome = runtime
            .run_pre_turn("hello")
            .expect("pre turn hook should run");

        assert_eq!(
            outcome,
            crate::hooks::PreHookOutcome::Allow("rewritten: hello".to_owned())
        );
    }

    #[test]
    fn runtime_vetoes_pre_tool_call() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("hooks.lua"),
            r#"
genesis.on("PreToolCall", function(ctx)
    return false, "blocked"
end)
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");

        let outcome = runtime
            .run_pre_tool_call("shell_exec", r#"{"command":"rm -rf /"}"#)
            .expect("pre tool hook should run");

        assert_eq!(
            outcome,
            crate::hooks::PreHookOutcome::Veto {
                reason: Some("blocked".to_owned())
            }
        );
    }

    #[test]
    fn runtime_rewrites_post_tool_call_output() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("hooks.lua"),
            r#"
genesis.on("PostToolCall", function(ctx)
    return ctx.output .. " [rewritten]"
end)
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");

        let output = runtime
            .run_post_tool_call("echo", "tool output")
            .expect("post tool hook should run");

        assert_eq!(
            output,
            crate::hooks::PostHookOutcome::Rewrite("tool output [rewritten]".to_owned())
        );
    }

    #[test]
    fn runtime_logs_hook_errors_and_keeps_original_value() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("hooks.lua"),
            r#"
genesis.on("PreTurn", function(_)
    error("boom")
end)
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");

        let outcome = runtime
            .run_pre_turn("hello")
            .expect("pre turn hook should not crash");

        assert_eq!(
            outcome,
            crate::hooks::PreHookOutcome::Allow("hello".to_owned())
        );
        assert!(
            runtime.logs().iter().any(|entry| entry.contains("boom")),
            "hook failure should be recorded in runtime logs: {:?}",
            runtime.logs()
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
    fn runtime_registers_lua_tools_with_owner_and_permissions() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let package_dir = dir.path().join("word-tools");
        fs::create_dir(&package_dir).expect("package dir should exist");
        fs::write(
            package_dir.join("plugin.toml"),
            r#"
[plugin]
name = "word-tools"
version = "0.1.0"

[permissions]
tools = ["read_file"]
"#,
        )
        .expect("plugin manifest should write");
        fs::write(
            package_dir.join("init.lua"),
            r#"
genesis.register_tool({
    name = "word_count",
    description = "Count words in a path",
    parameters = {
        path = {
            type = "string",
            description = "Path to inspect",
            required = true,
        },
    },
    run = function(args)
        return args.path
    end,
})
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");
        let tools = runtime.registered_tools();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].definition.name, "word_count");
        assert_eq!(tools[0].definition.description, "Count words in a path");
        assert_eq!(tools[0].plugin_name, "word-tools");
        assert_eq!(tools[0].permissions.tools, vec!["read_file"]);
        assert_eq!(
            tools[0].definition.parameters,
            Some(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to inspect",
                    }
                },
                "required": ["path"]
            }))
        );
    }

    #[test]
    fn runtime_rejects_duplicate_tool_names_without_overwriting_first_registration() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("first.lua"),
            r#"
genesis.register_tool({
    name = "echoer",
    description = "First echoer",
    run = function(args)
        return "first:" .. args.message
    end,
})
"#,
        )
        .expect("plugin should write");
        fs::write(
            dir.path().join("second.lua"),
            r#"
genesis.register_tool({
    name = "echoer",
    description = "Second echoer",
    run = function(args)
        return "second:" .. args.message
    end,
})
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");

        assert_eq!(runtime.registered_tools().len(), 1);
        assert_eq!(runtime.registered_tools()[0].plugin_name, "first");
        assert!(
            runtime
                .plugin_errors()
                .iter()
                .any(|entry| entry.contains("duplicate lua tool name `echoer`")),
            "duplicate tool registration should be recorded: {:?}",
            runtime.plugin_errors()
        );
    }

    #[test]
    fn runtime_rolls_back_tools_from_failed_plugins() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("broken.lua"),
            r#"
genesis.register_tool({
    name = "broken_tool",
    description = "Should not survive load failure",
    run = function(_)
        return "nope"
    end,
})
error("boom")
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");

        assert!(
            runtime.registered_tools().is_empty(),
            "failed plugins must not leave registered tools behind"
        );
        assert!(
            runtime
                .plugin_errors()
                .iter()
                .any(|entry| entry.contains("boom")),
            "plugin failure should still be recorded: {:?}",
            runtime.plugin_errors()
        );
    }

    #[test]
    fn runtime_rejects_tool_registration_after_plugin_load() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("late.lua"),
            r#"
local late_register = genesis.register_tool

genesis.on("PreTurn", function(ctx)
    late_register({
        name = "late_tool",
        description = "Should not be allowed after load",
        run = function(_)
            return "late"
        end,
    })
    return ctx.user_message
end)
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");
        assert!(
            runtime.registered_tools().is_empty(),
            "plugin load should not eagerly register the late tool"
        );

        let outcome = runtime
            .run_pre_turn("hello")
            .expect("hook execution should not crash runtime");

        assert_eq!(
            outcome,
            crate::hooks::PreHookOutcome::Allow("hello".to_owned())
        );
        assert!(
            runtime.registered_tools().is_empty(),
            "tool registration should stay closed after plugin load"
        );
        assert!(
            runtime
                .logs()
                .iter()
                .any(|entry| entry.contains("only available during plugin load")),
            "late registration failure should be logged: {:?}",
            runtime.logs()
        );
    }

    #[test]
    fn runtime_invokes_lua_tool_with_string_arguments() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("echoer.lua"),
            r#"
genesis.register_tool({
    name = "echoer",
    description = "Echo a string argument",
    run = function(args)
        return args.message .. ":" .. args.count
    end,
})
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");
        let output = runtime
            .invoke_tool(
                "echoer",
                BTreeMap::from([
                    ("message".to_owned(), "hello".to_owned()),
                    ("count".to_owned(), "2".to_owned()),
                ]),
            )
            .expect("tool should run");

        assert_eq!(
            output,
            crate::tools::LuaToolOutput::Text("hello:2".to_owned())
        );
    }

    #[test]
    fn runtime_returns_structured_json_from_lua_tool() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("structured.lua"),
            r#"
genesis.register_tool({
    name = "structured",
    description = "Return structured data",
    run = function(args)
        return {
            ok = true,
            echoed = args.message,
        }
    end,
})
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");
        let output = runtime
            .invoke_tool(
                "structured",
                BTreeMap::from([("message".to_owned(), "hello".to_owned())]),
            )
            .expect("tool should run");

        assert_eq!(
            output,
            crate::tools::LuaToolOutput::Json(json!({
                "ok": true,
                "echoed": "hello",
            }))
        );
    }

    #[test]
    fn runtime_lua_tool_can_call_permitted_host_tool() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let package_dir = dir.path().join("reader");
        fs::create_dir(&package_dir).expect("package dir should exist");
        fs::write(
            package_dir.join("plugin.toml"),
            r#"
[plugin]
name = "reader"
version = "0.1.0"

[permissions]
tools = ["read_file"]
"#,
        )
        .expect("manifest should write");
        fs::write(
            package_dir.join("init.lua"),
            r#"
genesis.register_tool({
    name = "read_path",
    description = "Read a path through the host bridge",
    run = function(args)
        return genesis.tools.read_file({ path = args.path })
    end,
})
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");
        let executor = Arc::new(TestHostToolExecutor::default());
        runtime.set_host_tool_executor(executor.clone());

        let output = runtime
            .invoke_tool(
                "read_path",
                BTreeMap::from([("path".to_owned(), "/tmp/demo.txt".to_owned())]),
            )
            .expect("tool should run");

        assert_eq!(
            output,
            crate::tools::LuaToolOutput::Text("read:/tmp/demo.txt".to_owned())
        );
        assert_eq!(
            executor
                .calls
                .lock()
                .expect("host tool calls mutex should not be poisoned")
                .as_slice(),
            &[(
                "read_file".to_owned(),
                BTreeMap::from([("path".to_owned(), "/tmp/demo.txt".to_owned())]),
            )]
        );
    }

    #[test]
    fn runtime_blocks_unpermitted_host_tool_access() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        fs::write(
            dir.path().join("blocked.lua"),
            r#"
genesis.register_tool({
    name = "blocked_reader",
    description = "Should not be able to reach host tools",
    run = function(args)
        return genesis.tools.read_file({ path = args.path })
    end,
})
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");
        runtime.set_host_tool_executor(Arc::new(TestHostToolExecutor::default()));

        let err = runtime
            .invoke_tool(
                "blocked_reader",
                BTreeMap::from([("path".to_owned(), "/tmp/demo.txt".to_owned())]),
            )
            .expect_err("unpermitted host tool access should fail");

        assert!(
            err.to_string()
                .contains("plugin `blocked` is not permitted to call host tool `read_file`"),
            "unexpected permission error: {err}"
        );
    }

    #[test]
    fn runtime_hook_can_call_permitted_host_tool() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let package_dir = dir.path().join("rewriter");
        fs::create_dir(&package_dir).expect("package dir should exist");
        fs::write(
            package_dir.join("plugin.toml"),
            r#"
[plugin]
name = "rewriter"
version = "0.1.0"

[permissions]
tools = ["echo"]
"#,
        )
        .expect("manifest should write");
        fs::write(
            package_dir.join("init.lua"),
            r#"
genesis.on("PreTurn", function(ctx)
    return genesis.tools.echo({ message = "hook:" .. ctx.user_message })
end)
"#,
        )
        .expect("plugin should write");

        let runtime = test_runtime(dir.path(), BTreeMap::new()).expect("runtime should build");
        runtime.set_host_tool_executor(Arc::new(TestHostToolExecutor::default()));

        let outcome = runtime
            .run_pre_turn("hello")
            .expect("hook should run");

        assert_eq!(
            outcome,
            crate::hooks::PreHookOutcome::Allow("hook:hello".to_owned())
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

        let runtime =
            test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

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

        let runtime =
            test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

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

        let runtime =
            test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

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
        fs::write(dir.path().join("first.lua"), "shared_value = 'leaked'")
            .expect("plugin should write");
        fs::write(
            dir.path().join("second.lua"),
            "assert(shared_value == nil, 'global leaked across plugins')\ngenesis.log('second loaded')",
        )
        .expect("plugin should write");

        let runtime =
            test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

        assert_eq!(
            runtime.plugin_names(),
            vec!["first".to_owned(), "second".to_owned()]
        );
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

        let runtime =
            test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

        assert_eq!(
            runtime.plugin_names(),
            vec!["first".to_owned(), "second".to_owned()]
        );
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

        let runtime =
            test_runtime(dir.path(), BTreeMap::new()).expect("runtime should still build");

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
