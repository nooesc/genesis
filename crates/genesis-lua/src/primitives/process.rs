use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use genesis_tools::sandbox::PathValidator;
use mlua::{Lua, Table, Value};

/// Build the `genesis.process` bridge table.
///
/// Provides one method:
/// - `exec(command, opts?)` — execute a shell command, returning
///   `{ stdout, stderr, exit_code }`.
///
/// The optional `opts` table supports:
/// - `cwd` (string) — working directory override
/// - `timeout` (number) — not yet supported; specifying a timeout returns an error
/// - `env` (table) — extra environment variables merged into the current env
pub fn make_process_bridge(
    lua: &Lua,
    working_dir: Option<PathBuf>,
    path_validator: Option<Arc<PathValidator>>,
) -> mlua::Result<Table> {
    let table = lua.create_table()?;

    table.set(
        "exec",
        lua.create_function(move |lua, (command, opts): (String, Option<Table>)| {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", &command]);

            // Resolve working directory: opts.cwd > working_dir > unset.
            let cwd: Option<PathBuf> = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("cwd").ok().flatten())
                .map(|cwd_str| {
                    if let Some(ref validator) = path_validator {
                        validator
                            .validate(&cwd_str)
                            .map_err(|e| mlua::Error::external(e.to_string()))
                    } else {
                        Ok(PathBuf::from(cwd_str))
                    }
                })
                .transpose()?
                .or_else(|| working_dir.clone());
            if let Some(dir) = cwd {
                cmd.current_dir(dir);
            }

            // Merge extra environment variables from opts.env.
            if let Some(ref opts) = opts {
                if let Ok(Some(env_table)) = opts.get::<Option<Table>>("env") {
                    for pair in env_table.pairs::<String, String>() {
                        let (key, value) = pair?;
                        cmd.env(key, value);
                    }
                }
            }

            // Timeout is not yet implemented — return an explicit error so
            // callers don't silently assume their timeout is enforced.
            if let Some(timeout) = opts
                .as_ref()
                .and_then(|o| o.get::<Option<u64>>("timeout").ok().flatten())
            {
                return Err(mlua::Error::external(format!(
                    "process.exec timeout ({timeout}s) is not yet implemented — \
                     remove the timeout option or use shell_exec which has built-in timeout support"
                )));
            }

            let output = cmd
                .output()
                .map_err(|e| mlua::Error::external(format!("process.exec failed to spawn: {e}")))?;

            let result = lua.create_table()?;
            result.set(
                "stdout",
                String::from_utf8_lossy(&output.stdout).into_owned(),
            )?;
            result.set(
                "stderr",
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )?;
            result.set("exit_code", output.status.code().unwrap_or(-1))?;

            Ok(Value::Table(result))
        })?,
    )?;

    Ok(table)
}

#[cfg(test)]
mod tests {
    /// Create a bare Lua VM with `genesis.process` installed.
    fn test_lua_with_process(working_dir: Option<std::path::PathBuf>) -> mlua::Lua {
        let lua = mlua::Lua::new();
        let process_table = super::make_process_bridge(&lua, working_dir, None)
            .expect("make_process_bridge should succeed");
        let genesis = lua.create_table().expect("table should create");
        genesis
            .set("process", process_table)
            .expect("set process should work");
        lua.globals()
            .set("genesis", genesis)
            .expect("set genesis should work");
        lua
    }

    #[test]
    fn exec_echo_hello() {
        let lua = test_lua_with_process(None);
        let result: mlua::Table = lua
            .load("return genesis.process.exec('echo hello')")
            .eval()
            .expect("exec should succeed");
        let stdout: String = result.get("stdout").expect("stdout should exist");
        assert_eq!(stdout, "hello\n");
    }

    #[test]
    fn exec_returns_exit_code() {
        let lua = test_lua_with_process(None);
        let result: mlua::Table = lua
            .load("return genesis.process.exec('exit 1')")
            .eval()
            .expect("exec should succeed");
        let exit_code: i32 = result.get("exit_code").expect("exit_code should exist");
        assert_eq!(exit_code, 1);
    }

    #[test]
    fn exec_captures_stderr() {
        let lua = test_lua_with_process(None);
        let result: mlua::Table = lua
            .load("return genesis.process.exec('echo err >&2')")
            .eval()
            .expect("exec should succeed");
        let stderr: String = result.get("stderr").expect("stderr should exist");
        assert_eq!(stderr, "err\n");
        let stdout: String = result.get("stdout").expect("stdout should exist");
        assert_eq!(stdout, "");
    }

    #[test]
    fn exec_with_env() {
        let lua = test_lua_with_process(None);
        let result: mlua::Table = lua
            .load("return genesis.process.exec('echo $FOO', { env = { FOO = 'bar' } })")
            .eval()
            .expect("exec should succeed");
        let stdout: String = result.get("stdout").expect("stdout should exist");
        assert_eq!(stdout, "bar\n");
    }

    #[test]
    fn exec_with_cwd() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let dir_path = dir.path().to_string_lossy().into_owned();
        let lua = test_lua_with_process(None);
        let result: mlua::Table = lua
            .load(&format!(
                "return genesis.process.exec('pwd', {{ cwd = '{}' }})",
                dir_path
            ))
            .eval()
            .expect("exec should succeed");
        let stdout: String = result.get("stdout").expect("stdout should exist");
        // On macOS, /tmp is a symlink to /private/tmp, so canonicalize both.
        let expected = std::fs::canonicalize(dir.path())
            .expect("canonicalize dir")
            .to_string_lossy()
            .into_owned();
        let actual = std::fs::canonicalize(stdout.trim())
            .expect("canonicalize stdout")
            .to_string_lossy()
            .into_owned();
        assert_eq!(actual, expected);
    }
}
