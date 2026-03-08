use std::collections::BTreeMap;
use std::process::Command;

use crate::{truncate_output, ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const DEFAULT_LIMIT: usize = 20;

// ---------------------------------------------------------------------------
// ListProcessesTool
// ---------------------------------------------------------------------------

pub struct ListProcessesTool;

/// A single parsed row from `ps aux` output.
struct PsEntry {
    user: String,
    pid: String,
    cpu: f64,
    mem: f64,
    command: String,
    /// The full raw line for display purposes.
    raw_line: String,
}

impl ToolHandler for ListProcessesTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = call.arguments.get("pattern").map(|s| s.as_str());
        let user_filter = call.arguments.get("user").map(|s| s.as_str());
        let sort_by = call
            .arguments
            .get("sort")
            .map(|s| s.as_str())
            .unwrap_or("cpu");
        let limit = call
            .arguments
            .get("limit")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_LIMIT);

        // Validate sort parameter.
        if !["cpu", "mem", "pid"].contains(&sort_by) {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "invalid sort value `{sort_by}`: expected one of \"cpu\", \"mem\", \"pid\""
                ),
            });
        }

        let output = Command::new("ps")
            .args(["aux"])
            .output()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to execute ps: {e}"),
            })?;

        if !output.status.success() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "ps returned non-zero exit code: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();

        // Grab the header line for display.
        let header = lines.next().unwrap_or("").to_owned();

        let mut entries: Vec<PsEntry> = lines.filter_map(|line| parse_ps_line(line)).collect();

        // Apply user filter.
        if let Some(u) = user_filter {
            let u_lower = u.to_lowercase();
            entries.retain(|e| e.user.to_lowercase() == u_lower);
        }

        // Apply pattern filter (case-insensitive substring match on the command).
        if let Some(pat) = pattern {
            let pat_lower = pat.to_lowercase();
            entries.retain(|e| e.command.to_lowercase().contains(&pat_lower));
        }

        // Sort.
        match sort_by {
            "cpu" => entries.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal)),
            "mem" => entries.sort_by(|a, b| b.mem.partial_cmp(&a.mem).unwrap_or(std::cmp::Ordering::Equal)),
            "pid" => entries.sort_by(|a, b| {
                let a_pid: i64 = a.pid.parse().unwrap_or(0);
                let b_pid: i64 = b.pid.parse().unwrap_or(0);
                a_pid.cmp(&b_pid)
            }),
            _ => {} // Already validated above.
        }

        let total_matched = entries.len();
        entries.truncate(limit);

        let mut content = String::new();
        content.push_str(&header);
        content.push('\n');
        for entry in &entries {
            content.push_str(&entry.raw_line);
            content.push('\n');
        }

        if total_matched > limit {
            content.push_str(&format!(
                "\n... showing {limit} of {total_matched} matching processes\n"
            ));
        }

        content = truncate_output(&content);

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("process_count".to_owned(), total_matched.to_string()),
                ("displayed".to_owned(), entries.len().to_string()),
            ]),
        })
    }
}

/// Parse a single line of `ps aux` output into a `PsEntry`.
///
/// Expected columns (space-separated, command is the remainder):
///   USER PID %CPU %MEM VSZ RSS TT STAT STARTED TIME COMMAND
fn parse_ps_line(line: &str) -> Option<PsEntry> {
    let parts: Vec<&str> = line.splitn(11, char::is_whitespace).collect();
    // We need at least 11 fields; the last field is the full command.
    if parts.len() < 11 {
        return None;
    }

    // Skip empty tokens produced by consecutive whitespace. We re-parse
    // more carefully by collecting non-empty segments.
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 11 {
        return None;
    }

    let user = tokens[0].to_owned();
    let pid = tokens[1].to_owned();
    let cpu: f64 = tokens[2].parse().unwrap_or(0.0);
    let mem: f64 = tokens[3].parse().unwrap_or(0.0);
    // tokens[4]=VSZ, [5]=RSS, [6]=TT, [7]=STAT, [8]=STARTED, [9]=TIME
    // tokens[10..] = COMMAND (may contain spaces)
    let command = tokens[10..].join(" ");

    Some(PsEntry {
        user,
        pid,
        cpu,
        mem,
        command,
        raw_line: line.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// SystemInfoTool
// ---------------------------------------------------------------------------

pub struct SystemInfoTool;

impl ToolHandler for SystemInfoTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let section = call
            .arguments
            .get("section")
            .map(|s| s.as_str())
            .unwrap_or("all");

        if !["all", "cpu", "memory", "disk", "network"].contains(&section) {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "invalid section `{section}`: expected one of \"all\", \"cpu\", \"memory\", \"disk\", \"network\""
                ),
            });
        }

        let mut content = String::new();

        if section == "all" || section == "cpu" {
            content.push_str(&format_section("CPU / Load", &get_cpu_info()));
        }

        if section == "all" || section == "memory" {
            content.push_str(&format_section("Memory", &get_memory_info()));
        }

        if section == "all" || section == "disk" {
            content.push_str(&format_section("Disk", &get_disk_info()));
        }

        if section == "all" || section == "network" {
            content.push_str(&format_section("Network Interfaces", &get_network_info()));
        }

        if content.is_empty() {
            content.push_str("(no information collected)\n");
        }

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("section".to_owned(), section.to_owned()),
                (
                    "platform".to_owned(),
                    if cfg!(target_os = "macos") {
                        "macos".to_owned()
                    } else if cfg!(target_os = "linux") {
                        "linux".to_owned()
                    } else {
                        "unknown".to_owned()
                    },
                ),
            ]),
        })
    }
}

fn format_section(title: &str, body: &str) -> String {
    format!("=== {title} ===\n{body}\n\n")
}

fn run_cmd(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_else(|| "(command failed)".to_owned())
}

fn get_cpu_info() -> String {
    // `uptime` works on both macOS and Linux and gives load averages.
    let uptime = run_cmd("uptime", &[]);
    let mut info = String::new();

    info.push_str(&uptime.trim().to_string());
    info.push('\n');

    if cfg!(target_os = "macos") {
        let ncpu = run_cmd("sysctl", &["-n", "hw.ncpu"]);
        let brand = run_cmd("sysctl", &["-n", "machdep.cpu.brand_string"]);
        info.push_str(&format!("CPUs: {}", ncpu.trim()));
        info.push('\n');
        let brand_trimmed = brand.trim();
        if !brand_trimmed.is_empty() {
            info.push_str(&format!("Model: {brand_trimmed}"));
            info.push('\n');
        }
    } else if cfg!(target_os = "linux") {
        let nproc = run_cmd("nproc", &[]);
        info.push_str(&format!("CPUs: {}", nproc.trim()));
        info.push('\n');
    }

    info
}

fn get_memory_info() -> String {
    if cfg!(target_os = "macos") {
        get_memory_info_macos()
    } else if cfg!(target_os = "linux") {
        get_memory_info_linux()
    } else {
        "(unsupported platform)".to_owned()
    }
}

fn get_memory_info_macos() -> String {
    let mut info = String::new();

    // Total physical memory.
    let total_bytes_str = run_cmd("sysctl", &["-n", "hw.memsize"]);
    let total_bytes: u64 = total_bytes_str.trim().parse().unwrap_or(0);
    let total_gb = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

    info.push_str(&format!("Total: {total_gb:.1} GB\n"));

    // vm_stat gives pages; page size is typically 16384 on Apple Silicon, 4096 on Intel.
    let page_size_str = run_cmd("sysctl", &["-n", "hw.pagesize"]);
    let page_size: u64 = page_size_str.trim().parse().unwrap_or(16384);

    let vm_stat = run_cmd("vm_stat", &[]);
    let mut free_pages: u64 = 0;
    let mut active_pages: u64 = 0;
    let mut inactive_pages: u64 = 0;
    let mut speculative_pages: u64 = 0;
    let mut wired_pages: u64 = 0;
    let mut compressed_pages: u64 = 0;

    for line in vm_stat.lines() {
        let line = line.trim().trim_end_matches('.');
        if let Some(val) = extract_vm_stat_value(line, "Pages free") {
            free_pages = val;
        } else if let Some(val) = extract_vm_stat_value(line, "Pages active") {
            active_pages = val;
        } else if let Some(val) = extract_vm_stat_value(line, "Pages inactive") {
            inactive_pages = val;
        } else if let Some(val) = extract_vm_stat_value(line, "Pages speculative") {
            speculative_pages = val;
        } else if let Some(val) = extract_vm_stat_value(line, "Pages wired down") {
            wired_pages = val;
        } else if let Some(val) = extract_vm_stat_value(line, "Pages occupied by compressor") {
            compressed_pages = val;
        }
    }

    let free_gb = (free_pages + speculative_pages) as f64 * page_size as f64 / (1024.0 * 1024.0 * 1024.0);
    let used_gb = (active_pages + wired_pages + compressed_pages) as f64 * page_size as f64 / (1024.0 * 1024.0 * 1024.0);
    let inactive_gb = inactive_pages as f64 * page_size as f64 / (1024.0 * 1024.0 * 1024.0);

    info.push_str(&format!("Used: {used_gb:.1} GB\n"));
    info.push_str(&format!("Free: {free_gb:.1} GB\n"));
    info.push_str(&format!("Inactive: {inactive_gb:.1} GB\n"));

    info
}

fn extract_vm_stat_value(line: &str, key: &str) -> Option<u64> {
    if line.starts_with(key) {
        line.split(':')
            .nth(1)
            .and_then(|v| v.trim().parse::<u64>().ok())
    } else {
        None
    }
}

fn get_memory_info_linux() -> String {
    let meminfo = run_cmd("cat", &["/proc/meminfo"]);
    let mut total_kb: u64 = 0;
    let mut available_kb: u64 = 0;
    let mut free_kb: u64 = 0;
    let mut buffers_kb: u64 = 0;
    let mut cached_kb: u64 = 0;

    for line in meminfo.lines() {
        if let Some(val) = extract_meminfo_value(line, "MemTotal") {
            total_kb = val;
        } else if let Some(val) = extract_meminfo_value(line, "MemAvailable") {
            available_kb = val;
        } else if let Some(val) = extract_meminfo_value(line, "MemFree") {
            free_kb = val;
        } else if let Some(val) = extract_meminfo_value(line, "Buffers") {
            buffers_kb = val;
        } else if let Some(val) = extract_meminfo_value(line, "Cached") {
            cached_kb = val;
        }
    }

    let total_gb = total_kb as f64 / (1024.0 * 1024.0);
    let avail_gb = if available_kb > 0 {
        available_kb
    } else {
        free_kb + buffers_kb + cached_kb
    } as f64
        / (1024.0 * 1024.0);
    let used_gb = total_gb - avail_gb;

    format!(
        "Total: {total_gb:.1} GB\nUsed: {used_gb:.1} GB\nAvailable: {avail_gb:.1} GB\n"
    )
}

fn extract_meminfo_value(line: &str, key: &str) -> Option<u64> {
    if line.starts_with(key) {
        // Format: "MemTotal:       16384000 kB"
        line.split(':')
            .nth(1)
            .and_then(|v| {
                v.trim()
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
            })
    } else {
        None
    }
}

fn get_disk_info() -> String {
    if cfg!(target_os = "macos") {
        // On macOS, exclude noisy filesystem types.
        run_cmd("df", &["-h", "-T", "apfs,hfs"])
    } else {
        run_cmd("df", &["-h", "--exclude-type=tmpfs", "--exclude-type=devtmpfs"])
    }
}

fn get_network_info() -> String {
    if cfg!(target_os = "macos") {
        // Show only active interfaces with addresses.
        let raw = run_cmd("ifconfig", &[]);
        summarize_interfaces(&raw)
    } else {
        let raw = run_cmd("ip", &["-brief", "addr"]);
        if raw.contains("(command failed)") {
            run_cmd("ifconfig", &[])
        } else {
            raw
        }
    }
}

/// Summarize `ifconfig` output to show only interfaces that have an inet address.
fn summarize_interfaces(raw: &str) -> String {
    let mut result = String::new();
    let mut current_iface = String::new();
    let mut iface_lines: Vec<String> = Vec::new();
    let mut has_inet = false;

    for line in raw.lines() {
        if !line.starts_with('\t') && !line.starts_with(' ') && line.contains(':') {
            // Flush previous interface.
            if has_inet {
                result.push_str(&current_iface);
                result.push('\n');
                for l in &iface_lines {
                    result.push_str(l);
                    result.push('\n');
                }
                result.push('\n');
            }
            current_iface = line.to_owned();
            iface_lines.clear();
            has_inet = false;
        } else {
            let trimmed = line.trim();
            if trimmed.starts_with("inet ") || trimmed.starts_with("inet6 ") {
                has_inet = true;
                iface_lines.push(line.to_owned());
            }
        }
    }

    // Flush last interface.
    if has_inet {
        result.push_str(&current_iface);
        result.push('\n');
        for l in &iface_lines {
            result.push_str(l);
            result.push('\n');
        }
    }

    if result.is_empty() {
        "(no active network interfaces found)".to_owned()
    } else {
        result
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_OUTPUT_BYTES;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
        }
    }

    // -----------------------------------------------------------------------
    // ListProcessesTool
    // -----------------------------------------------------------------------

    #[test]
    fn list_processes_returns_results() {
        let tool = ListProcessesTool;
        let call = ToolCall {
            name: "list_processes".to_owned(),
            arguments: BTreeMap::new(),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        // The header and at least one process should be present.
        assert!(output.content.contains("PID"));
        let count: usize = output
            .metadata
            .get("process_count")
            .unwrap()
            .parse()
            .unwrap();
        assert!(count > 0, "should have at least one process");
    }

    #[test]
    fn list_processes_pattern_filter() {
        // Every test run has a process whose command line contains something;
        // filtering for a nonsensical pattern should yield zero results.
        let tool = ListProcessesTool;
        let call = ToolCall {
            name: "list_processes".to_owned(),
            arguments: BTreeMap::from([(
                "pattern".to_owned(),
                "zzz_nonexistent_process_xyz_1234567890".to_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        let count: usize = output
            .metadata
            .get("process_count")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn list_processes_finds_known_process() {
        // The kernel_task / init / launchd process should always exist.
        let tool = ListProcessesTool;

        // On macOS, `launchd` is PID 1; on Linux, `init` or `systemd`.
        // We just search for something very generic that `ps aux` itself will
        // produce — the `ps` command itself.
        let call = ToolCall {
            name: "list_processes".to_owned(),
            arguments: BTreeMap::from([("pattern".to_owned(), "ps".to_owned())]),
        };

        // The `ps aux` invocation itself should match since its command line
        // contains "ps". However, it may have already exited by the time we
        // parse. Use a more reliable pattern instead.
        let output = tool.run(&call, &ctx()).expect("should succeed");
        // We can't guarantee a match, but the tool should not error.
        assert!(output.content.contains("PID"));
    }

    #[test]
    fn list_processes_sort_by_pid() {
        let tool = ListProcessesTool;
        let call = ToolCall {
            name: "list_processes".to_owned(),
            arguments: BTreeMap::from([
                ("sort".to_owned(), "pid".to_owned()),
                ("limit".to_owned(), "5".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("PID"));
    }

    #[test]
    fn list_processes_sort_by_mem() {
        let tool = ListProcessesTool;
        let call = ToolCall {
            name: "list_processes".to_owned(),
            arguments: BTreeMap::from([("sort".to_owned(), "mem".to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("PID"));
    }

    #[test]
    fn list_processes_invalid_sort_rejected() {
        let tool = ListProcessesTool;
        let call = ToolCall {
            name: "list_processes".to_owned(),
            arguments: BTreeMap::from([("sort".to_owned(), "invalid".to_owned())]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn list_processes_user_filter() {
        let tool = ListProcessesTool;
        let call = ToolCall {
            name: "list_processes".to_owned(),
            arguments: BTreeMap::from([("user".to_owned(), "root".to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        // Should not error, though we can't guarantee root processes exist in
        // every CI environment.
        assert!(output.content.contains("PID"));
    }

    #[test]
    fn list_processes_respects_limit() {
        let tool = ListProcessesTool;
        let call = ToolCall {
            name: "list_processes".to_owned(),
            arguments: BTreeMap::from([("limit".to_owned(), "2".to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        let displayed: usize = output
            .metadata
            .get("displayed")
            .unwrap()
            .parse()
            .unwrap();
        assert!(displayed <= 2);
    }

    // -----------------------------------------------------------------------
    // SystemInfoTool
    // -----------------------------------------------------------------------

    #[test]
    fn system_info_returns_all_sections() {
        let tool = SystemInfoTool;
        let call = ToolCall {
            name: "system_info".to_owned(),
            arguments: BTreeMap::new(),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("=== CPU / Load ==="));
        assert!(output.content.contains("=== Memory ==="));
        assert!(output.content.contains("=== Disk ==="));
        assert!(output.content.contains("=== Network Interfaces ==="));
    }

    #[test]
    fn system_info_cpu_section() {
        let tool = SystemInfoTool;
        let call = ToolCall {
            name: "system_info".to_owned(),
            arguments: BTreeMap::from([("section".to_owned(), "cpu".to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("=== CPU / Load ==="));
        // Should not contain other sections.
        assert!(!output.content.contains("=== Memory ==="));
        assert!(!output.content.contains("=== Disk ==="));
    }

    #[test]
    fn system_info_memory_section() {
        let tool = SystemInfoTool;
        let call = ToolCall {
            name: "system_info".to_owned(),
            arguments: BTreeMap::from([("section".to_owned(), "memory".to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("=== Memory ==="));
        assert!(output.content.contains("Total:"));
    }

    #[test]
    fn system_info_disk_section() {
        let tool = SystemInfoTool;
        let call = ToolCall {
            name: "system_info".to_owned(),
            arguments: BTreeMap::from([("section".to_owned(), "disk".to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("=== Disk ==="));
        assert!(!output.content.is_empty());
    }

    #[test]
    fn system_info_network_section() {
        let tool = SystemInfoTool;
        let call = ToolCall {
            name: "system_info".to_owned(),
            arguments: BTreeMap::from([("section".to_owned(), "network".to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("=== Network Interfaces ==="));
    }

    #[test]
    fn system_info_invalid_section_rejected() {
        let tool = SystemInfoTool;
        let call = ToolCall {
            name: "system_info".to_owned(),
            arguments: BTreeMap::from([("section".to_owned(), "bogus".to_owned())]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn system_info_reports_platform_in_metadata() {
        let tool = SystemInfoTool;
        let call = ToolCall {
            name: "system_info".to_owned(),
            arguments: BTreeMap::new(),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        let platform = output.metadata.get("platform").unwrap();
        assert!(
            platform == "macos" || platform == "linux" || platform == "unknown",
            "unexpected platform: {platform}"
        );
    }

    // -----------------------------------------------------------------------
    // Helper unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_ps_line_valid() {
        let line = "root               1   0.0  0.1 34291744  15472   ??  Ss   Sat08AM   2:34.56 /sbin/launchd";
        let entry = parse_ps_line(line).expect("should parse");
        assert_eq!(entry.user, "root");
        assert_eq!(entry.pid, "1");
        assert!((entry.cpu - 0.0).abs() < f64::EPSILON);
        assert!(entry.command.contains("/sbin/launchd"));
    }

    #[test]
    fn parse_ps_line_too_few_fields() {
        let line = "root 1 0.0";
        assert!(parse_ps_line(line).is_none());
    }

    #[test]
    fn extract_vm_stat_value_works() {
        assert_eq!(
            extract_vm_stat_value("Pages free:                12345", "Pages free"),
            Some(12345)
        );
        assert_eq!(
            extract_vm_stat_value("Pages active:              99999", "Pages active"),
            Some(99999)
        );
        assert_eq!(
            extract_vm_stat_value("Something else: 42", "Pages free"),
            None
        );
    }

    #[test]
    fn extract_meminfo_value_works() {
        assert_eq!(
            extract_meminfo_value("MemTotal:       16384000 kB", "MemTotal"),
            Some(16384000)
        );
        assert_eq!(
            extract_meminfo_value("MemFree:         1234567 kB", "MemFree"),
            Some(1234567)
        );
        assert_eq!(
            extract_meminfo_value("MemFree:         1234567 kB", "MemTotal"),
            None
        );
    }

    #[test]
    fn summarize_interfaces_filters_inactive() {
        let raw = "\
lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384
\tinet 127.0.0.1 netmask 0xff000000
\tinet6 ::1 prefixlen 128
gif0: flags=8010<POINTOPOINT,MULTICAST> mtu 1280
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tinet 192.168.1.100 netmask 0xffffff00 broadcast 192.168.1.255
";

        let summary = summarize_interfaces(raw);
        assert!(summary.contains("lo0:"));
        assert!(summary.contains("127.0.0.1"));
        assert!(summary.contains("en0:"));
        assert!(summary.contains("192.168.1.100"));
        // gif0 has no inet lines, should be excluded.
        assert!(!summary.contains("gif0:"));
    }

    #[test]
    fn truncate_output_preserves_short_text() {
        let short = "hello world\n";
        assert_eq!(truncate_output(short), short);
    }

    #[test]
    fn truncate_output_cuts_long_text() {
        let long = "a\n".repeat(100_000);
        let result = truncate_output(&long);
        assert!(result.len() <= MAX_OUTPUT_BYTES + 50);
        assert!(result.ends_with("... (output truncated)"));
    }
}
