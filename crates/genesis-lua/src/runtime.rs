use thiserror::Error;

#[derive(Debug, Clone, Default)]
pub struct LuaRuntimeConfig;

#[derive(Debug, Clone, Default)]
pub struct LuaRuntime;

#[derive(Debug, Clone, Default)]
pub struct LuaRuntimeBuilder {
    config: LuaRuntimeConfig,
}

#[derive(Debug, Error)]
pub enum LuaRuntimeError {
    #[error("lua runtime initialization is not implemented yet")]
    NotImplemented,
}

impl LuaRuntime {
    pub fn builder() -> LuaRuntimeBuilder {
        LuaRuntimeBuilder::default()
    }
}

impl LuaRuntimeBuilder {
    pub fn with_config(mut self, config: LuaRuntimeConfig) -> Self {
        self.config = config;
        self
    }

    pub fn build(self) -> Result<LuaRuntime, LuaRuntimeError> {
        let _ = self.config;
        Ok(LuaRuntime)
    }
}
