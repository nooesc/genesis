# Lua (Luau) Plugin System Design

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:writing-plans to create the implementation plan.

**Goal:** Add a default-on Luau plugin runtime that lets Genesis load sandboxed plugins from disk, register Lua-defined tools, run in-process lifecycle hooks, and expose Lua-defined personalities without touching the TUI customization surface in this branch.

**Architecture:** Add a new `genesis-lua` crate that owns plugin discovery, manifest parsing, sandbox setup, runtime state, and the `genesis.*` API. Integrate it into `genesis-core` at session construction and the agent loop middleware points, bridge it to `genesis-tools` for builtin tool access and Lua tool registration, and extend `genesis-config` and `genesis-cli` for plugin paths and plugin/personality inspection. Keep the existing Rust tool ABI for native tools in this PR, but give Lua plugins structured JSON internally so the plugin API does not inherit the current flattened argument model.

**Tech Stack:** Rust workspace crates, `mlua` with Luau support, `serde`/`serde_json`, existing Genesis agent loop, tool runtime, config loader, and CLI command surfaces.

---

## Scope

This branch follows issue `#148` closely for the runtime, hooks, tools, and personalities:

1. plugin discovery and loading from `~/.genesis/plugins`
2. package manifests with permission declarations
3. sandboxed Luau runtime
4. in-process hook middleware
5. Lua-defined tools
6. Lua-defined personalities
7. bundled Lua personalities as the migration path away from hard-coded Rust personalities

This branch explicitly does **not** include:

1. TUI keybinding customization
2. status line customization
3. theme customization

Those surfaces are hard-coded in the current TUI and would make this PR much larger than the non-TUI implementation already requires.

## Current Codebase Constraints

The issue assumes several extension seams that do not exist yet:

1. Current shell hooks in `crates/genesis-core/src/hooks.rs` are observe-only.
2. `AgentHooks` in `crates/genesis-core/src/agent_loop.rs` are also observe-only.
3. Builtin tool execution in `crates/genesis-tools/src/lib.rs` is synchronous and uses flattened `BTreeMap<String, String>` arguments.
4. Personalities in `crates/genesis-core/src/personality.rs` are compile-time constants.
5. Prompt composition in `crates/genesis-core/src/prompt.rs` only supports a static personality prefix lookup.

That means this implementation is not a runtime swap. It is a new plugin runtime plus new middleware seams in `genesis-core`.

## Plugin Model

Plugin discovery follows the issue:

1. single file plugin: `~/.genesis/plugins/<name>.lua`
2. package plugin: `~/.genesis/plugins/<name>/init.lua` with `plugin.toml`
3. bundled plugin: shipped in the repo and loaded through the same runtime path

Manifest format:

```toml
[plugin]
name = "my-plugin"
version = "0.1.0"
description = "Does something useful"
author = "user"

[permissions]
tools = ["read_file", "write_file"]
hooks = ["PreToolCall", "PostToolCall"]
trusted = false

[genesis]
min_version = "0.1.0"
```

Trust and permissions:

1. single-file plugins are untrusted by default
2. package plugins may request explicit tool and hook permissions
3. bundled plugins are treated as trusted but still go through the same registration and runtime path
4. plugins never get raw filesystem, process, or network access from Lua itself
5. builtin tool access always goes through permission checks

## Runtime Model

Each Genesis session gets one plugin runtime manager. The runtime manager:

1. discovers plugins
2. validates manifests
3. creates per-plugin execution state
4. registers hook callbacks, tools, and personalities
5. exposes a shared session-scoped `genesis.*` API

The runtime is session-scoped rather than process-global so plugin state cannot bleed between sessions.

`genesis-lua` should be split roughly as:

1. `runtime.rs` for session runtime ownership
2. `discovery.rs` for plugin path scanning and source loading
3. `manifest.rs` for `plugin.toml` parsing and validation
4. `api.rs` for building the `genesis.*` table
5. `hooks.rs` for callback registration and event dispatch
6. `tools.rs` for Lua tool registration and builtin-tool bridging
7. `personality.rs` for Lua-defined personalities

## `genesis.*` API Surface

This PR includes the non-TUI portion of the API from the issue:

```lua
genesis.on(event, callback)
genesis.register_tool({...})
genesis.register_personality({...})

genesis.log(msg)
genesis.log_warn(msg)
genesis.log_error(msg)

genesis.tools.<tool_name>(args)

genesis.session.id
genesis.session.model
genesis.session.turn_count
genesis.session.total_tokens
genesis.session.platform
genesis.session.personality

genesis.config.get(key)
genesis.version
genesis.plugin_dir
```

Explicitly deferred:

```lua
genesis.keybinds({...})
genesis.status_line({...})
genesis.theme({...})
```

These stay out of scope for this branch.

## Hook and Middleware Semantics

The runtime hook surface should follow the issue with a smaller first cut:

1. `PreTurn`
2. `PostTurn`
3. `PreToolCall`
4. `PostToolCall`
5. `OnError`
6. `OnComplete`

Deferred from the issue:

1. `OnMessage`
2. `OnPluginLoad`

Event behavior:

1. `PreTurn` can veto the LLM turn or rewrite/inject messages
2. `PostTurn` can rewrite the response payload
3. `PreToolCall` can veto or rewrite tool arguments
4. `PostToolCall` can rewrite tool output
5. `OnError` is observe-only
6. `OnComplete` is observe-only

Safety rules:

1. every veto is logged with plugin name and reason
2. invalid tool-argument rewrites are rejected and the original arguments are preserved
3. hook errors never crash the session
4. repeated plugin failures auto-disable that plugin for the session
5. internal runtime/tool calls from plugin execution use a re-entry guard so hooks do not recursively trigger themselves

## Tool Model

Lua-defined tools should look like normal Genesis tools to the model:

```lua
genesis.register_tool({
    name = "word_count",
    description = "Count words in a file",
    parameters = {
        type = "object",
        properties = {
            path = { type = "string", description = "File path" }
        },
        required = { "path" }
    },
    run = function(args)
        local content = genesis.tools.read_file({ path = args.path })
        local count = select(2, content:gsub("%S+", ""))
        return { content = "Word count: " .. tostring(count) }
    end
})
```

Implementation rules:

1. Lua plugins work with structured JSON values internally
2. Rust builtin tools remain on the current flattened argument ABI in this PR
3. the bridge layer converts structured Lua arguments into the existing Rust call shape when invoking builtin tools
4. Lua tool outputs are normalized into the existing `ToolOutput` model
5. output truncation still uses the existing limits in `genesis-tools`

This is a compromise: it keeps the PR tractable without forcing Lua plugin authors into the current string-only internal ABI.

## Personality Model

Lua personalities should be first-class plugin registrations, not a separate subsystem:

```lua
genesis.register_personality({
    name = "pirate",
    description = "Responds like a sea captain",
    system_prompt = "Ye be a salty sea captain. Speak in pirate vernacular.",
    build_prompt = function(ctx)
        local base = "Ye be a salty sea captain."
        if ctx.platform == "telegram" then
            return base .. " Keep replies concise for mobile."
        end
        return base
    end
})
```

Rules:

1. Rust personalities stay available during the migration
2. Lua personalities are loaded alongside them
3. prompt resolution prefers an exact Lua personality match, then falls back to Rust
4. bundled Lua personalities become the migration path for replacing the current hard-coded defaults

`transform_response` from the issue should not be implemented as a special case in personality code. If added, it should ride the same post-turn middleware path as other plugin transforms.

## Crate and Integration Boundaries

### `genesis-lua`

Owns:

1. plugin discovery
2. manifest parsing
3. sandbox creation
4. plugin lifecycle state
5. hook registry and event dispatch
6. Lua tool registry
7. Lua personality registry

### `genesis-core`

Changes:

1. instantiate the runtime during session construction in `crates/genesis-core/src/execution.rs`
2. attach plugin middleware to the real agent-loop decision points in `crates/genesis-core/src/agent_loop.rs`
3. extend prompt assembly in `crates/genesis-core/src/prompt.rs`
4. extend personality lookup in `crates/genesis-core/src/personality.rs` or move lookup behind a provider abstraction

### `genesis-tools`

Changes:

1. expose a safe builtin-tool bridge for Lua
2. support including Lua-defined tool definitions in the runtime-visible tool list
3. keep native Rust tools unchanged where possible

### `genesis-config`

Changes:

1. extend `AppPaths` with a plugin directory path
2. add plugin runtime configuration for timeouts and auto-disable thresholds
3. include plugin paths in example config rendering if that improves discoverability

### `genesis-cli`

Changes:

1. add `genesis plugins list`
2. add `genesis plugins show <name>`
3. add `genesis plugins enable <name>` and `disable <name>` if the runtime needs persisted toggles
4. extend `genesis personality list/show` to include Lua personalities

## Safety Model

The issue’s safety expectations should hold:

1. sandbox before loading any plugin code
2. plugin crashes are non-fatal to the session
3. hook callbacks have shorter timeouts than tools
4. plugin failures are logged clearly
5. panics or irrecoverable runtime errors disable the offending plugin for the current session

This PR should also define:

1. default hook timeout
2. default tool timeout
3. default auto-disable threshold
4. the exact user-visible warning format when a plugin is disabled

## Testing Strategy

This feature is mostly about edge cases and failure handling. Tests should cover:

1. discovery of single-file, package, and bundled plugins
2. manifest validation and duplicate-name handling
3. permission enforcement for builtin tool access
4. hook veto behavior
5. hook transform behavior
6. invalid transform rollback
7. timeout handling
8. plugin auto-disable after repeated errors
9. re-entry guard behavior
10. Lua tool registration and execution
11. Lua personality registration and prompt resolution
12. agent-loop integration for pre/post tool hooks and pre/post turn hooks

The branch should keep verification realistic:

1. targeted crate tests for `genesis-lua`
2. targeted integration tests in `genesis-core`
3. relevant CLI tests for new plugin/personality commands

Note: current `main` is not test-clean in this worktree. `cargo test` already has a pre-existing failure in `crates/genesis-config/src/lib.rs` unrelated to this feature, so final verification must call that out unless it is fixed separately as part of the branch.

## Delivery Phases Inside One PR

To keep one branch/PR reviewable, the implementation should still be staged internally:

1. `genesis-lua` crate, manifests, discovery, sandbox, runtime skeleton
2. middleware integration in `genesis-core`
3. Lua tool registration and builtin-tool bridge
4. Lua personality loading and prompt integration
5. CLI/config/docs/polish

Each phase should be committed as a single story with conventional commit messages.
