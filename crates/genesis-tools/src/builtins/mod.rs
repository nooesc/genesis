pub mod browse;
pub mod browser;
pub mod channel_directory;
pub mod clarify;
pub mod code_execution;
pub mod docker;
pub mod export;
pub mod fs;
pub mod git;
pub mod glob;
pub mod homeassistant;
pub mod image_gen;
pub mod memory;
pub mod mixture;
pub mod patch;
pub mod process;
pub mod process_registry;
pub mod reason;
pub mod schedule;
pub mod search;
pub mod send_message;
pub mod session;
pub mod shell;
pub mod skill;
pub mod skill_file;
pub mod ssh;
pub mod subagent;
pub mod think;
pub mod todo;
pub mod trajectory;
pub mod transcribe;
pub mod tree;
pub mod tts;
pub mod user_model;
pub mod vision;
pub mod web;
pub mod web_search;

/// Combine stdout and stderr from a command execution into a single string.
/// If both are empty, returns a message with the exit code.
pub(crate) fn combine_command_output(stdout: &str, stderr: &str, exit_code: i32) -> String {
    let mut content = String::new();
    if !stdout.is_empty() {
        content.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str("[stderr]\n");
        content.push_str(stderr);
    }
    if content.is_empty() {
        format!("(no output, exit code {exit_code})")
    } else {
        content
    }
}
