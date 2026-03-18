pub mod api;
pub mod discovery;
pub mod hooks;
pub mod manifest;
pub mod personality;
pub mod runtime;
pub mod tools;

pub use discovery::{discover_plugins, DiscoveredPlugin, PluginKind};
pub use manifest::{PluginGenesis, PluginManifest, PluginMetadata, PluginPermissions};
pub use runtime::{LuaRuntime, LuaRuntimeBuilder, LuaRuntimeConfig, LuaRuntimeError};

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_builder_is_constructible() {
        let runtime = crate::LuaRuntime::builder().build();
        assert!(runtime.is_ok());
    }
}
