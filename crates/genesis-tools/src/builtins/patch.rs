use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Minimum similarity ratio (0.0–1.0) for a fuzzy match to be accepted.
const FUZZY_THRESHOLD: f64 = 0.70;

/// Targeted find-and-replace tool with fuzzy fallback.
///
/// First attempts an exact text match. If that fails, falls back to
/// line-based fuzzy matching using sliding-window similarity scoring.
/// This handles cases where the model's view of the file is slightly
/// stale due to other edits or minor whitespace differences.
pub struct PatchTool;

impl ToolHandler for PatchTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = call
            .arguments
            .get("path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "path",
            })?;

        let old_text = call
            .arguments
            .get("old_text")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "old_text",
            })?;

        let new_text = call
            .arguments
            .get("new_text")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "new_text",
            })?;

        let file_path = Path::new(path);
        let content = fs::read_to_string(file_path).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to read `{path}`: {e}"),
        })?;

        let replace_all = call
            .arguments
            .get("replace_all")
            .map(|v| v == "true")
            .unwrap_or(false);

        // Try exact match first.
        let count = content.matches(old_text.as_str()).count();

        if count == 1 || (count > 1 && replace_all) {
            // Exact match path (unchanged from before).
            let updated = content.replace(old_text.as_str(), new_text.as_str());
            fs::write(file_path, &updated).map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to write `{path}`: {e}"),
            })?;

            let replacements = if replace_all { count } else { 1 };
            return Ok(ToolOutput {
                content: format!(
                    "patched `{path}`: {replacements} replacement(s) applied ({} bytes → {} bytes)",
                    content.len(),
                    updated.len()
                ),
                metadata: BTreeMap::from([
                    ("tool".to_owned(), call.name.clone()),
                    ("path".to_owned(), path.clone()),
                    ("replacements".to_owned(), replacements.to_string()),
                    ("match_type".to_owned(), "exact".to_owned()),
                ]),
            });
        }

        if count > 1 && !replace_all {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "old_text found {count} times in `{path}`. Provide more surrounding \
                     context to make the match unique, or set replace_all to \"true\" to \
                     replace every occurrence."
                ),
            });
        }

        // Exact match failed (count == 0). Try fuzzy matching.
        let fuzzy = fuzzy_find_best_match(&content, old_text);

        match fuzzy {
            Some(FuzzyMatch {
                start,
                end,
                score,
                matched_text,
            }) => {
                let mut updated = String::with_capacity(content.len());
                updated.push_str(&content[..start]);
                updated.push_str(new_text);
                updated.push_str(&content[end..]);

                fs::write(file_path, &updated).map_err(|e| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("failed to write `{path}`: {e}"),
                })?;

                // Build a diff hint showing what was actually matched vs what was requested.
                let similarity_pct = (score * 100.0).round() as u32;
                let diff_hint = build_diff_hint(old_text, &matched_text);

                Ok(ToolOutput {
                    content: format!(
                        "patched `{path}` via fuzzy match ({similarity_pct}% similarity, \
                         {} bytes → {} bytes).\n\n\
                         The exact old_text was not found, but a similar block was matched \
                         and replaced.\n{diff_hint}",
                        content.len(),
                        updated.len()
                    ),
                    metadata: BTreeMap::from([
                        ("tool".to_owned(), call.name.clone()),
                        ("path".to_owned(), path.clone()),
                        ("replacements".to_owned(), "1".to_owned()),
                        ("match_type".to_owned(), "fuzzy".to_owned()),
                        ("similarity".to_owned(), format!("{similarity_pct}")),
                    ]),
                })
            }
            None => Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "old_text not found in `{path}` (exact match failed, fuzzy match \
                     below {:.0}% threshold). Make sure the text is close to what actually \
                     appears in the file.",
                    FUZZY_THRESHOLD * 100.0
                ),
            }),
        }
    }
}

// ─── Fuzzy matching engine ──────────────────────────────────────────────────

struct FuzzyMatch {
    /// Byte offset of the match start in the file content.
    start: usize,
    /// Byte offset of the match end in the file content.
    end: usize,
    /// Similarity score (0.0–1.0).
    score: f64,
    /// The actual text that was matched.
    matched_text: String,
}

/// Find the best fuzzy match for `needle` within `haystack` using a
/// line-based sliding window approach.
fn fuzzy_find_best_match(haystack: &str, needle: &str) -> Option<FuzzyMatch> {
    let needle_lines: Vec<&str> = needle.lines().collect();
    if needle_lines.is_empty() {
        return None;
    }

    let haystack_lines: Vec<&str> = haystack.lines().collect();
    let window_size = needle_lines.len();

    let mut best_score = 0.0_f64;
    let mut best_start_line = 0usize;
    let mut best_ws = window_size;

    // Slide a window of `window_size` lines across the haystack.
    // Also check windows ±1/±2 lines to handle line additions/deletions.
    for extra in [0isize, -1, 1, -2, 2] {
        let ws = (window_size as isize + extra).max(1) as usize;
        if ws > haystack_lines.len() {
            continue;
        }
        for start in 0..=(haystack_lines.len() - ws) {
            let window = &haystack_lines[start..start + ws];
            let score = line_block_similarity(&needle_lines, window);
            if score > best_score {
                best_score = score;
                best_start_line = start;
                best_ws = ws;
            }
        }
    }

    if best_score < FUZZY_THRESHOLD {
        return None;
    }

    let final_ws = best_ws;

    // Compute byte offsets from line indices.
    let start_byte = line_start_offset(haystack, best_start_line);
    let end_byte = if best_start_line + final_ws >= haystack_lines.len() {
        haystack.len()
    } else {
        line_start_offset(haystack, best_start_line + final_ws)
    };

    // The matched text should not include the trailing newline of the last
    // matched line if the needle doesn't end with one, to maintain consistency.
    let mut matched_end = end_byte;
    if !needle.ends_with('\n') && matched_end > start_byte {
        // Trim trailing newline from matched region.
        let slice = &haystack[start_byte..matched_end];
        if slice.ends_with('\n') {
            matched_end -= 1;
            if matched_end > start_byte && haystack.as_bytes()[matched_end - 1] == b'\r' {
                matched_end -= 1;
            }
        }
    }

    let matched_text = haystack[start_byte..matched_end].to_string();

    Some(FuzzyMatch {
        start: start_byte,
        end: end_byte,
        score: best_score,
        matched_text,
    })
}

/// Compute the byte offset of the start of line `n` (0-indexed) in `text`.
fn line_start_offset(text: &str, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let mut count = 0;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            count += 1;
            if count == n {
                return i + 1;
            }
        }
    }
    text.len()
}

/// Compute similarity between two blocks of lines.
///
/// Uses a combination of:
/// 1. Line-for-line similarity (for same-length blocks)
/// 2. Best-line matching via LCS-style scoring (for different-length blocks)
fn line_block_similarity(needle: &[&str], window: &[&str]) -> f64 {
    if needle.is_empty() && window.is_empty() {
        return 1.0;
    }
    if needle.is_empty() || window.is_empty() {
        return 0.0;
    }

    // If same number of lines, do direct line-by-line comparison.
    if needle.len() == window.len() {
        let mut total = 0.0;
        for (n, w) in needle.iter().zip(window.iter()) {
            total += line_similarity(n, w);
        }
        return total / needle.len() as f64;
    }

    // Different number of lines: use a greedy matching approach.
    // For each needle line, find the best matching window line (in order).
    let mut total = 0.0;
    let mut w_start = 0;
    for n_line in needle {
        if w_start >= window.len() {
            // No more window lines to match against; remaining needle lines
            // contribute 0 similarity.
            break;
        }
        let mut best_sim = 0.0;
        let mut best_idx = w_start;
        let search_end = (w_start + 3).min(window.len());
        for (wi, w_line) in window[w_start..search_end].iter().enumerate() {
            let sim = line_similarity(n_line, w_line);
            if sim > best_sim {
                best_sim = sim;
                best_idx = w_start + wi;
            }
        }
        total += best_sim;
        w_start = best_idx + 1;
    }

    // Normalize by the larger block to penalize size mismatches.
    total / needle.len().max(window.len()) as f64
}

/// Compute similarity between two individual lines (0.0–1.0).
///
/// Uses normalized Levenshtein distance on trimmed lines, with a bonus
/// for matching indentation.
fn line_similarity(a: &str, b: &str) -> f64 {
    let a_trimmed = a.trim();
    let b_trimmed = b.trim();

    // Empty lines match each other perfectly, but not non-empty lines.
    if a_trimmed.is_empty() && b_trimmed.is_empty() {
        return 1.0;
    }
    if a_trimmed.is_empty() || b_trimmed.is_empty() {
        return 0.0;
    }

    // If trimmed content is identical, score based on indent similarity.
    if a_trimmed == b_trimmed {
        let indent_a = a.len() - a.trim_start().len();
        let indent_b = b.len() - b.trim_start().len();
        let indent_diff = (indent_a as f64 - indent_b as f64).abs();
        // Small indent differences are fine (might be tab vs spaces).
        let indent_penalty = (indent_diff / 8.0).min(0.1);
        return 1.0 - indent_penalty;
    }

    // Fall back to Levenshtein-based similarity on trimmed content.
    let dist = levenshtein(a_trimmed, b_trimmed);
    let max_len = a_trimmed.len().max(b_trimmed.len());
    if max_len == 0 {
        return 1.0;
    }

    let content_sim = 1.0 - (dist as f64 / max_len as f64);

    // Weight content similarity more than indentation.
    content_sim * 0.95
}

/// Classic Levenshtein edit distance, O(n*m) time and O(min(n,m)) space.
///
/// When both inputs are pure ASCII, delegates to [`levenshtein_bytes`] which
/// operates directly on byte slices, avoiding the two `Vec<char>` heap
/// allocations that the general path requires.
fn levenshtein(a: &str, b: &str) -> usize {
    // Fast path: skip Vec<char> allocations for ASCII-only strings.
    if a.is_ascii() && b.is_ascii() {
        return levenshtein_bytes(a.as_bytes(), b.as_bytes());
    }

    // General path for non-ASCII (multi-byte chars need char-level indexing).
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();

    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    // Use single-row optimization.
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0; m + 1];

    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1) // deletion
                .min(curr[j - 1] + 1) // insertion
                .min(prev[j - 1] + cost); // substitution
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[m]
}

/// Levenshtein edit distance on raw byte slices (ASCII fast path).
///
/// Identical algorithm to the char-based version but avoids heap-allocating
/// `Vec<char>` since each byte maps 1:1 to a character for ASCII input.
fn levenshtein_bytes(a: &[u8], b: &[u8]) -> usize {
    let m = a.len();
    let n = b.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

/// Build a compact diff hint showing differences between expected and actual text.
fn build_diff_hint(expected: &str, actual: &str) -> String {
    let exp_lines: Vec<&str> = expected.lines().collect();
    let act_lines: Vec<&str> = actual.lines().collect();

    let mut diffs = Vec::new();
    let max_lines = exp_lines.len().max(act_lines.len());
    let show_limit = 5; // Show at most 5 differences.

    for i in 0..max_lines {
        let exp = exp_lines.get(i).copied().unwrap_or("");
        let act = act_lines.get(i).copied().unwrap_or("");
        if exp != act {
            diffs.push(format!("  line {}: expected `{}`, found `{}`", i + 1, exp, act));
            if diffs.len() >= show_limit {
                diffs.push("  ... and more differences".to_string());
                break;
            }
        }
    }

    if diffs.is_empty() {
        String::new()
    } else {
        format!("\nDifferences between expected and matched text:\n{}", diffs.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
    use tempfile::tempdir;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: true,
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
        }
    }

    // ── Exact match tests (preserved from original) ──

    #[test]
    fn patch_replaces_unique_text() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world\ngoodbye world\n").unwrap();

        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                ("old_text".to_owned(), "hello world".to_owned()),
                ("new_text".to_owned(), "hi world".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("1 replacement(s)"));
        assert_eq!(output.metadata["match_type"], "exact");
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "hi world\ngoodbye world\n"
        );
    }

    #[test]
    fn patch_errors_on_ambiguous_match() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello\nhello\nhello\n").unwrap();

        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                ("old_text".to_owned(), "hello".to_owned()),
                ("new_text".to_owned(), "hi".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("3 times"));
            }
            _ => panic!("expected ExecutionFailed"),
        }
    }

    #[test]
    fn patch_replace_all_replaces_every_occurrence() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello\nhello\nhello\n").unwrap();

        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                ("old_text".to_owned(), "hello".to_owned()),
                ("new_text".to_owned(), "hi".to_owned()),
                ("replace_all".to_owned(), "true".to_owned()),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("3 replacement(s)"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "hi\nhi\nhi\n");
    }

    #[test]
    fn patch_errors_on_missing_file() {
        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), "/nonexistent/file.txt".to_owned()),
                ("old_text".to_owned(), "hello".to_owned()),
                ("new_text".to_owned(), "hi".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn patch_preserves_indentation() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("code.rs");
        fs::write(
            &file,
            "fn main() {\n    let x = 1;\n    let y = 2;\n}\n",
        )
        .unwrap();

        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                ("old_text".to_owned(), "    let x = 1;".to_owned()),
                (
                    "new_text".to_owned(),
                    "    let x = 42;\n    let z = 100;".to_owned(),
                ),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("1 replacement(s)"));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "fn main() {\n    let x = 42;\n    let z = 100;\n    let y = 2;\n}\n"
        );
    }

    // ── Fuzzy match tests ──

    #[test]
    fn fuzzy_matches_with_minor_whitespace_difference() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("code.py");
        // File has 4-space indent
        fs::write(
            &file,
            "def hello():\n    print('hello')\n    print('world')\n",
        )
        .unwrap();

        let tool = PatchTool;
        // Agent thinks it's 2-space indent
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                (
                    "old_text".to_owned(),
                    "def hello():\n  print('hello')\n  print('world')".to_owned(),
                ),
                (
                    "new_text".to_owned(),
                    "def hello():\n    print('hi')\n    print('world')".to_owned(),
                ),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should fuzzy match");
        assert!(output.content.contains("fuzzy match"));
        assert!(output.metadata["match_type"] == "fuzzy");
        let result = fs::read_to_string(&file).unwrap();
        assert!(result.contains("print('hi')"));
    }

    #[test]
    fn fuzzy_matches_with_minor_content_change() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("code.rs");
        // File was edited since the model last saw it
        fs::write(
            &file,
            "fn greet(name: &str) {\n    println!(\"Hello, {}!\", name);\n    log::info!(\"greeted\");\n}\n",
        )
        .unwrap();

        let tool = PatchTool;
        // Model's stale view: missing the log line
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                (
                    "old_text".to_owned(),
                    "fn greet(name: &str) {\n    println!(\"Hello, {}!\", name);\n}".to_owned(),
                ),
                (
                    "new_text".to_owned(),
                    "fn greet(name: &str) {\n    println!(\"Hi, {}!\", name);\n}".to_owned(),
                ),
            ]),
        };

        let output = tool.run(&call, &ctx()).expect("should fuzzy match");
        assert!(output.content.contains("fuzzy match"));
        let result = fs::read_to_string(&file).unwrap();
        // The replacement was applied to the matched region.
        assert!(result.contains("Hi, {}!"));
    }

    #[test]
    fn fuzzy_rejects_completely_different_text() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "alpha\nbeta\ngamma\ndelta\n").unwrap();

        let tool = PatchTool;
        let call = ToolCall {
            name: "patch".to_owned(),
            arguments: BTreeMap::from([
                ("path".to_owned(), file.to_string_lossy().into_owned()),
                (
                    "old_text".to_owned(),
                    "completely\ndifferent\ntext\nhere".to_owned(),
                ),
                ("new_text".to_owned(), "replacement".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("fuzzy match"));
                assert!(reason.contains("threshold"));
            }
            _ => panic!("expected ExecutionFailed"),
        }
    }

    // ── Unit tests for fuzzy internals ──

    #[test]
    fn levenshtein_basic_cases() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("saturday", "sunday"), 3);
    }

    #[test]
    fn line_similarity_identical_lines() {
        assert!((line_similarity("let x = 1;", "let x = 1;") - 1.0).abs() < 0.01);
    }

    #[test]
    fn line_similarity_different_indent() {
        let sim = line_similarity("    let x = 1;", "  let x = 1;");
        assert!(sim >= 0.9, "same content different indent: {sim}");
    }

    #[test]
    fn line_similarity_minor_edit() {
        let sim = line_similarity("let x = 1;", "let x = 2;");
        assert!(sim > 0.8, "minor edit should score high: {sim}");
    }

    #[test]
    fn line_similarity_completely_different() {
        let sim = line_similarity("fn main() {", "import os");
        assert!(sim < 0.5, "different lines should score low: {sim}");
    }

    #[test]
    fn line_block_similarity_identical_blocks() {
        let a = vec!["fn main() {", "    println!(\"hi\");", "}"];
        let b = vec!["fn main() {", "    println!(\"hi\");", "}"];
        let sim = line_block_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 0.01, "identical blocks: {sim}");
    }

    #[test]
    fn line_block_similarity_with_extra_line() {
        let needle = vec!["fn main() {", "    println!(\"hi\");", "}"];
        let window = vec![
            "fn main() {",
            "    println!(\"hi\");",
            "    // extra line",
            "}",
        ];
        let sim = line_block_similarity(&needle, &window);
        assert!(sim > 0.6, "extra line should still be reasonable: {sim}");
    }

    #[test]
    fn fuzzy_find_best_match_returns_none_for_empty_needle() {
        assert!(fuzzy_find_best_match("some content", "").is_none());
    }

    #[test]
    fn fuzzy_find_best_match_finds_similar_block() {
        let haystack = "line1\nfn foo() {\n    let x = 1;\n}\nline5\n";
        let needle = "fn foo() {\n    let x = 2;\n}";

        let m = fuzzy_find_best_match(haystack, needle).expect("should find fuzzy match");
        assert!(m.score >= FUZZY_THRESHOLD);
        assert!(m.matched_text.contains("fn foo()"));
    }

    #[test]
    fn line_start_offset_computes_correctly() {
        let text = "line0\nline1\nline2\n";
        assert_eq!(line_start_offset(text, 0), 0);
        assert_eq!(line_start_offset(text, 1), 6);
        assert_eq!(line_start_offset(text, 2), 12);
    }

    #[test]
    fn build_diff_hint_shows_differences() {
        let hint = build_diff_hint("a\nb\nc", "a\nB\nc");
        assert!(hint.contains("line 2"));
        assert!(hint.contains("expected `b`"));
        assert!(hint.contains("found `B`"));
    }

    #[test]
    fn build_diff_hint_empty_when_identical() {
        let hint = build_diff_hint("same\nlines", "same\nlines");
        assert!(hint.is_empty());
    }
}
