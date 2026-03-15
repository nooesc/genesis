//! Tool call parsers for models that embed tool calls in text content
//! rather than using the native OpenAI tool_calls field.
//!
//! Each parser extracts tool calls from model-specific markup formats
//! (e.g. `<tool_call>...</tool_call>` for Hermes models) and converts
//! them to standard [`ToolCallEntry`] objects.

use regex::Regex;
use std::sync::LazyLock;
use tracing::warn;
use uuid::Uuid;

use crate::api_types::{FunctionCall, ToolCallEntry};

/// Result of parsing tool calls from text content.
#[derive(Debug, Clone)]
pub struct ParseResult {
    /// Text content before the tool call markup (the model's "thinking" or response).
    /// `None` if the entire output was tool calls with no preceding text.
    pub content: Option<String>,
    /// Extracted tool calls, or empty if none found.
    pub tool_calls: Vec<ToolCallEntry>,
}

/// A parser that can extract tool calls from model-generated text.
pub trait ToolCallParser: Send + Sync {
    /// Attempt to parse tool calls from the given text.
    /// Returns `None` if no tool calls were found in the text.
    fn parse(&self, text: &str) -> Option<ParseResult>;

    /// The name of this parser (for logging/config).
    fn name(&self) -> &'static str;
}

/// Generate a unique tool call ID.
fn gen_call_id() -> String {
    let mut buf = [0u8; 32];
    Uuid::new_v4().simple().encode_lower(&mut buf);
    format!("call_{}", std::str::from_utf8(&buf[..8]).unwrap())
}

/// Build a [`ToolCallEntry`] from name and arguments JSON string.
fn make_tool_call(name: String, arguments: String) -> ToolCallEntry {
    ToolCallEntry {
        id: gen_call_id(),
        call_type: "function".to_owned(),
        function: FunctionCall { name, arguments },
    }
}

// ---------------------------------------------------------------------------
// Hermes parser: <tool_call>{"name": "...", "arguments": {...}}</tool_call>
// Also used by Qwen 2.5.
// ---------------------------------------------------------------------------

static HERMES_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<tool_call>\s*(.*?)\s*</tool_call>|<tool_call>\s*(.*)").unwrap()
});

pub struct HermesParser;

impl ToolCallParser for HermesParser {
    fn name(&self) -> &'static str {
        "hermes"
    }

    fn parse(&self, text: &str) -> Option<ParseResult> {
        parse_xml_tag_format(text, &HERMES_RE, "<tool_call>")
    }
}

/// Shared logic for Hermes-style XML tag parsers (Hermes, Longcat).
/// The JSON inside the tags must have `"name"` and `"arguments"` keys.
fn parse_xml_tag_format(text: &str, re: &Regex, start_tag: &str) -> Option<ParseResult> {
    // Fast exit: skip regex when the start tag is absent
    if !text.contains(start_tag) {
        return None;
    }

    let content = text
        .find(start_tag)
        .map(|pos| text[..pos].trim().to_owned())
        .filter(|s| !s.is_empty());

    let mut tool_calls = Vec::new();
    for cap in re.captures_iter(text) {
        let json_str = cap
            .get(1)
            .or_else(|| cap.get(2))
            .map(|m| m.as_str().trim())
            .unwrap_or("");

        if json_str.is_empty() {
            continue;
        }

        match serde_json::from_str::<serde_json::Value>(json_str) {
            Ok(val) => {
                let name = val
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned();
                let arguments = val
                    .get("arguments")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".to_owned());

                if !name.is_empty() {
                    tool_calls.push(make_tool_call(name, arguments));
                }
            }
            Err(e) => {
                warn!(json = json_str, error = %e, "failed to parse tool call JSON");
            }
        }
    }

    if tool_calls.is_empty() {
        return None;
    }

    Some(ParseResult {
        content,
        tool_calls,
    })
}

// ---------------------------------------------------------------------------
// Longcat parser: <longcat_tool_call>...</longcat_tool_call>
// ---------------------------------------------------------------------------

static LONGCAT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<longcat_tool_call>\s*(.*?)\s*</longcat_tool_call>|<longcat_tool_call>\s*(.*)")
        .unwrap()
});

pub struct LongcatParser;

impl ToolCallParser for LongcatParser {
    fn name(&self) -> &'static str {
        "longcat"
    }

    fn parse(&self, text: &str) -> Option<ParseResult> {
        parse_xml_tag_format(text, &LONGCAT_RE, "<longcat_tool_call>")
    }
}

// ---------------------------------------------------------------------------
// Mistral parser: [TOOL_CALLS] with two sub-formats
// Pre-v11: JSON array after [TOOL_CALLS]
// v11+: tool_name{"arg": "val"} segments
// ---------------------------------------------------------------------------

static MISTRAL_PRE_V11_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[?\s*(\{.*?\})\s*\]?").unwrap()
});

pub struct MistralParser;

impl ToolCallParser for MistralParser {
    fn name(&self) -> &'static str {
        "mistral"
    }

    fn parse(&self, text: &str) -> Option<ParseResult> {
        let split_pos = text.find("[TOOL_CALLS]")?;
        let content = {
            let before = text[..split_pos].trim();
            if before.is_empty() {
                None
            } else {
                Some(before.to_owned())
            }
        };
        let after = text[split_pos + "[TOOL_CALLS]".len()..].trim();

        if after.is_empty() {
            return None;
        }

        let mut tool_calls = Vec::new();

        // Detect format: if starts with [ or { it's pre-v11 JSON
        if after.starts_with('[') || after.starts_with('{') {
            // Pre-v11: try to parse as JSON array first
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(after) {
                for val in arr {
                    if let Some(tc) = extract_mistral_tool_call(&val) {
                        tool_calls.push(tc);
                    }
                }
            } else {
                // Fallback: extract individual JSON objects with regex
                for cap in MISTRAL_PRE_V11_RE.captures_iter(after) {
                    if let Some(json_str) = cap.get(1) {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str.as_str())
                        {
                            if let Some(tc) = extract_mistral_tool_call(&val) {
                                tool_calls.push(tc);
                            }
                        }
                    }
                }
            }
        } else {
            // v11+: segments separated by [TOOL_CALLS]
            // Each segment: tool_name{"arg": "val"}
            let segments: Vec<&str> = after.split("[TOOL_CALLS]").collect();
            for segment in segments {
                let segment = segment.trim();
                if segment.is_empty() {
                    continue;
                }
                if let Some(brace_pos) = segment.find('{') {
                    let name = segment[..brace_pos].trim().to_owned();
                    let args = segment[brace_pos..].trim().to_owned();
                    if !name.is_empty() {
                        tool_calls.push(make_tool_call(name, args));
                    }
                }
            }
        }

        if tool_calls.is_empty() {
            return None;
        }

        Some(ParseResult {
            content,
            tool_calls,
        })
    }
}

fn extract_mistral_tool_call(val: &serde_json::Value) -> Option<ToolCallEntry> {
    let name = val.get("name").and_then(|v| v.as_str())?.to_owned();
    let arguments = val
        .get("arguments")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "{}".to_owned());
    Some(make_tool_call(name, arguments))
}

// ---------------------------------------------------------------------------
// Llama parser: raw JSON objects or <|python_tag|> prefix
// Uses a JSON decoder walking approach.
// ---------------------------------------------------------------------------

pub struct LlamaParser;

impl ToolCallParser for LlamaParser {
    fn name(&self) -> &'static str {
        "llama"
    }

    fn parse(&self, text: &str) -> Option<ParseResult> {
        // Strip <|python_tag|> prefix if present
        let clean = text
            .strip_prefix("<|python_tag|>")
            .unwrap_or(text)
            .trim();

        let mut tool_calls = Vec::new();
        let mut first_match_pos: Option<usize> = None;
        let mut skip_until = 0usize;

        for (i, _) in clean.char_indices().filter(|(_, c)| *c == '{') {
            if i < skip_until {
                continue;
            }

            if let Ok((val, end_idx)) = parse_json_at(clean, i) {
                // A valid tool call must have "name" and either "arguments" or "parameters"
                let has_name = val.get("name").and_then(|v| v.as_str()).is_some();
                let has_args = val.get("arguments").is_some() || val.get("parameters").is_some();

                if has_name && has_args {
                    if first_match_pos.is_none() {
                        first_match_pos = Some(i);
                    }

                    let name = val["name"].as_str().unwrap().to_owned();
                    let arguments = val
                        .get("arguments")
                        .or_else(|| val.get("parameters"))
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "{}".to_owned());

                    tool_calls.push(make_tool_call(name, arguments));
                    skip_until = end_idx;
                }
            }
        }

        if tool_calls.is_empty() {
            return None;
        }

        // Content is everything before the first JSON match, or before <|python_tag|>
        let content = if text.starts_with("<|python_tag|>") {
            None
        } else {
            first_match_pos
                .map(|pos| clean[..pos].trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_owned())
        };

        Some(ParseResult {
            content,
            tool_calls,
        })
    }
}

/// Try to parse a JSON value starting at `offset` in `text`.
/// Returns (parsed_value, end_index) on success.
fn parse_json_at(text: &str, offset: usize) -> Result<(serde_json::Value, usize), ()> {
    let slice = &text[offset..];
    let mut de = serde_json::Deserializer::from_str(slice).into_iter::<serde_json::Value>();
    match de.next() {
        Some(Ok(val)) => {
            let consumed = de.byte_offset();
            Ok((val, offset + consumed))
        }
        _ => Err(()),
    }
}

// ---------------------------------------------------------------------------
// DeepSeek V3 parser: fullwidth Unicode tokens + markdown code blocks
// <｜tool▁calls▁begin｜>
// <｜tool▁call▁begin｜>type<｜tool▁sep｜>function_name
// ```json
// {"arg": "value"}
// ```
// <｜tool▁call▁end｜>
// <｜tool▁calls▁end｜>
// ---------------------------------------------------------------------------

static DEEPSEEK_V3_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)<｜tool▁call▁begin｜>(?P<type>.*)<｜tool▁sep｜>(?P<name>.*)\n```json\n(?P<args>.*)\n```<｜tool▁call▁end｜>",
    )
    .unwrap()
});

/// Shared logic for DeepSeek-style parsers (V3 and V3.1).
/// Both use fullwidth Unicode token delimiters; only the inner capture format differs.
fn parse_deepseek_format(text: &str, re: &Regex) -> Option<ParseResult> {
    if !text.contains("<｜tool▁calls▁begin｜>") {
        return None;
    }

    let content = text
        .find("<｜tool▁calls▁begin｜>")
        .map(|pos| text[..pos].trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    let tool_calls: Vec<_> = re
        .captures_iter(text)
        .filter_map(|cap| {
            let name = cap.name("name")?.as_str().trim();
            let args = cap.name("args").map(|m| m.as_str().trim()).unwrap_or("{}");
            (!name.is_empty()).then(|| make_tool_call(name.to_owned(), args.to_owned()))
        })
        .collect();

    if tool_calls.is_empty() {
        return None;
    }

    Some(ParseResult {
        content,
        tool_calls,
    })
}

pub struct DeepSeekV3Parser;

impl ToolCallParser for DeepSeekV3Parser {
    fn name(&self) -> &'static str {
        "deepseek_v3"
    }

    fn parse(&self, text: &str) -> Option<ParseResult> {
        parse_deepseek_format(text, &DEEPSEEK_V3_RE)
    }
}

// ---------------------------------------------------------------------------
// DeepSeek V3.1 parser: same tokens but no markdown code block
// <｜tool▁call▁begin｜>function_name<｜tool▁sep｜>{"arg": "val"}<｜tool▁call▁end｜>
// ---------------------------------------------------------------------------

static DEEPSEEK_V31_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)<｜tool▁call▁begin｜>(?P<name>.*?)<｜tool▁sep｜>(?P<args>.*?)<｜tool▁call▁end｜>",
    )
    .unwrap()
});

pub struct DeepSeekV31Parser;

impl ToolCallParser for DeepSeekV31Parser {
    fn name(&self) -> &'static str {
        "deepseek_v31"
    }

    fn parse(&self, text: &str) -> Option<ParseResult> {
        parse_deepseek_format(text, &DEEPSEEK_V31_RE)
    }
}

// ---------------------------------------------------------------------------
// Kimi K2 parser:
// <|tool_calls_section_begin|>
// <|tool_call_begin|>functions.get_weather:0<|tool_call_argument_begin|>{"location": "NYC"}<|tool_call_end|>
// <|tool_calls_section_end|>
// ---------------------------------------------------------------------------

pub struct KimiK2Parser;

impl ToolCallParser for KimiK2Parser {
    fn name(&self) -> &'static str {
        "kimi_k2"
    }

    fn parse(&self, text: &str) -> Option<ParseResult> {
        // Accept both singular and plural forms
        if !text.contains("<|tool_calls_section_begin|>")
            && !text.contains("<|tool_call_section_begin|>")
        {
            return None;
        }

        let content = text
            .find("<|tool_call")
            .map(|pos| text[..pos].trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_owned());

        // Parse by splitting on delimiters rather than using lookahead regex
        let mut tool_calls = Vec::new();
        let begin_token = "<|tool_call_begin|>";
        let arg_token = "<|tool_call_argument_begin|>";
        let end_token = "<|tool_call_end|>";

        let mut search_from = 0;
        while let Some(begin_pos) = text[search_from..].find(begin_token) {
            let begin_pos = search_from + begin_pos + begin_token.len();
            let remaining = &text[begin_pos..];

            let arg_pos = match remaining.find(arg_token) {
                Some(p) => p,
                None => break,
            };
            let function_id = remaining[..arg_pos].trim();
            let after_arg = &remaining[arg_pos + arg_token.len()..];

            let end_pos = match after_arg.find(end_token) {
                Some(p) => p,
                None => break,
            };
            let args = after_arg[..end_pos].trim();

            // Extract function name: functions.get_weather:0 -> get_weather
            let name = function_id
                .split(':')
                .next()
                .unwrap_or("")
                .split('.')
                .next_back()
                .unwrap_or("")
                .to_owned();

            if !name.is_empty() {
                let tc = ToolCallEntry {
                    id: function_id.to_owned(),
                    call_type: "function".to_owned(),
                    function: FunctionCall {
                        name,
                        arguments: args.to_owned(),
                    },
                };
                tool_calls.push(tc);
            }

            search_from = begin_pos + arg_pos + arg_token.len() + end_pos + end_token.len();
        }

        if tool_calls.is_empty() {
            return None;
        }

        Some(ParseResult {
            content,
            tool_calls,
        })
    }
}

// ---------------------------------------------------------------------------
// GLM 4.5 parser: XML-style arg_key/arg_value pairs
// <tool_call>function_name
// <arg_key>key</arg_key><arg_value>value</arg_value>
// </tool_call>
// ---------------------------------------------------------------------------

static GLM_TOOL_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<tool_call>.*?</tool_call>").unwrap()
});

static GLM_DETAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<tool_call>([^\n]*)\n(.*)</tool_call>").unwrap()
});

static GLM_ARG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<arg_key>(.*?)</arg_key>\s*<arg_value>(.*?)</arg_value>").unwrap()
});

/// Shared GLM parser logic — both 4.5 and 4.7 use `<tool_call>` blocks with
/// `<arg_key>`/`<arg_value>` pairs, differing only in their detail and arg regexes.
fn parse_glm_format(text: &str, detail_re: &Regex, arg_re: &Regex) -> Option<ParseResult> {
    if !text.contains("<tool_call>") || !text.contains("<arg_key>") {
        return None;
    }

    let content = text
        .find("<tool_call>")
        .map(|pos| text[..pos].trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    let mut tool_calls = Vec::new();
    for block_match in GLM_TOOL_CALL_RE.find_iter(text) {
        let block = block_match.as_str();
        if let Some(detail_cap) = detail_re.captures(block) {
            // Take first line only — harmless for 4.5 (single-line capture),
            // necessary for 4.7 (multi-line capture group).
            let name = detail_cap
                .get(1)
                .map(|m| m.as_str().trim())
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_owned();

            let body = detail_cap.get(2).map(|m| m.as_str()).unwrap_or("");

            let mut args = serde_json::Map::new();
            for arg_cap in arg_re.captures_iter(body) {
                let key = arg_cap
                    .get(1)
                    .map(|m| m.as_str().trim())
                    .unwrap_or("")
                    .to_owned();
                let val_str = arg_cap.get(2).map(|m| m.as_str().trim()).unwrap_or("");

                let val = serde_json::from_str::<serde_json::Value>(val_str)
                    .unwrap_or_else(|_| serde_json::Value::String(val_str.to_owned()));

                if !key.is_empty() {
                    args.insert(key, val);
                }
            }

            if !name.is_empty() {
                let arguments =
                    serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_owned());
                tool_calls.push(make_tool_call(name, arguments));
            }
        }
    }

    if tool_calls.is_empty() {
        return None;
    }

    Some(ParseResult {
        content,
        tool_calls,
    })
}

pub struct Glm45Parser;

impl ToolCallParser for Glm45Parser {
    fn name(&self) -> &'static str {
        "glm45"
    }

    fn parse(&self, text: &str) -> Option<ParseResult> {
        parse_glm_format(text, &GLM_DETAIL_RE, &GLM_ARG_RE)
    }
}

// ---------------------------------------------------------------------------
// GLM 4.7 parser: extends GLM 4.5 with more flexible whitespace handling
// ---------------------------------------------------------------------------

static GLM47_DETAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<tool_call>(.*?)(<arg_key>.*?)?</tool_call>").unwrap()
});

static GLM47_ARG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<arg_key>(.*?)</arg_key>(?:\\n|\s)*<arg_value>(.*?)</arg_value>").unwrap()
});

pub struct Glm47Parser;

impl ToolCallParser for Glm47Parser {
    fn name(&self) -> &'static str {
        "glm47"
    }

    fn parse(&self, text: &str) -> Option<ParseResult> {
        parse_glm_format(text, &GLM47_DETAIL_RE, &GLM47_ARG_RE)
    }
}

// ---------------------------------------------------------------------------
// Qwen3-Coder parser: nested XML with <function=name> and <parameter=name>
// <tool_call>
// <function=get_weather>
// <parameter=location>NYC</parameter>
// </function>
// </tool_call>
// ---------------------------------------------------------------------------

struct Qwen3Function {
    name: String,
    arguments: String,
}

/// Extract `<function=name>...<parameter=key>val</parameter>...</function>` blocks
/// from text, using string splitting to avoid regex lookaheads.
fn extract_qwen3_functions(text: &str) -> Vec<Qwen3Function> {
    let mut results = Vec::new();
    let mut search_from = 0;

    while let Some(fn_start) = text[search_from..].find("<function=") {
        let fn_start = search_from + fn_start + "<function=".len();
        let remaining = &text[fn_start..];

        // Function name ends at first >
        let gt_pos = match remaining.find('>') {
            Some(p) => p,
            None => {
                // Truncated — use rest as name with no args
                let name = remaining.trim().to_owned();
                if !name.is_empty() {
                    results.push(Qwen3Function {
                        name,
                        arguments: "{}".to_owned(),
                    });
                }
                break;
            }
        };

        let name = remaining[..gt_pos].trim().to_owned();
        let after_name = &remaining[gt_pos + 1..];

        // Find end of function block
        let fn_end = after_name
            .find("</function>")
            .unwrap_or(after_name.len());
        let fn_body = &after_name[..fn_end];

        // Extract parameters from the function body
        let mut args = serde_json::Map::new();
        let mut param_search = 0;
        while let Some(param_start) = fn_body[param_search..].find("<parameter=") {
            let param_start = param_search + param_start + "<parameter=".len();
            let param_remaining = &fn_body[param_start..];

            if let Some(param_gt) = param_remaining.find('>') {
                let key = param_remaining[..param_gt].trim().to_owned();
                let after_key = &param_remaining[param_gt + 1..];

                // Value ends at </parameter>, next <parameter=, or end of body
                let val_end = after_key
                    .find("</parameter>")
                    .or_else(|| after_key.find("<parameter="))
                    .unwrap_or(after_key.len());

                let val_str = after_key[..val_end].trim();
                let val = serde_json::from_str::<serde_json::Value>(val_str)
                    .unwrap_or_else(|_| serde_json::Value::String(val_str.to_owned()));

                if !key.is_empty() {
                    args.insert(key, val);
                }

                param_search = param_start + param_gt + 1 + val_end;
            } else {
                break;
            }
        }

        if !name.is_empty() {
            let arguments = serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_owned());
            results.push(Qwen3Function { name, arguments });
        }

        search_from = fn_start + gt_pos + 1 + fn_end;
    }

    results
}

pub struct Qwen3CoderParser;

impl ToolCallParser for Qwen3CoderParser {
    fn name(&self) -> &'static str {
        "qwen3_coder"
    }

    fn parse(&self, text: &str) -> Option<ParseResult> {
        // Distinguish from Hermes: Qwen3-Coder uses <function= prefix
        if !text.contains("<function=") {
            return None;
        }

        let content = text
            .find("<tool_call>")
            .or_else(|| text.find("<function="))
            .map(|pos| text[..pos].trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_owned());

        let mut tool_calls = Vec::new();

        // Extract function blocks using string splitting
        for func in extract_qwen3_functions(text) {
            tool_calls.push(make_tool_call(func.name, func.arguments));
        }

        if tool_calls.is_empty() {
            return None;
        }

        Some(ParseResult {
            content,
            tool_calls,
        })
    }
}

// ===========================================================================
// Parser registry and auto-detection
// ===========================================================================

/// Get a parser by explicit name.
pub fn get_parser(name: &str) -> Option<Box<dyn ToolCallParser>> {
    match name {
        "hermes" | "qwen" | "qwen25" => Some(Box::new(HermesParser)),
        "longcat" => Some(Box::new(LongcatParser)),
        "mistral" => Some(Box::new(MistralParser)),
        "llama" | "llama3" | "llama4" => Some(Box::new(LlamaParser)),
        "deepseek_v3" | "deepseek-v3" => Some(Box::new(DeepSeekV3Parser)),
        "deepseek_v31" | "deepseek-v31" | "deepseek_v3.1" | "deepseek-v3.1" => {
            Some(Box::new(DeepSeekV31Parser))
        }
        "kimi_k2" | "kimi-k2" => Some(Box::new(KimiK2Parser)),
        "glm45" | "glm-4.5" | "glm4.5" => Some(Box::new(Glm45Parser)),
        "glm47" | "glm-4.7" | "glm4.7" => Some(Box::new(Glm47Parser)),
        "qwen3_coder" | "qwen3-coder" => Some(Box::new(Qwen3CoderParser)),
        _ => None,
    }
}

/// Detect the appropriate parser for a model name.
/// Returns `None` for models that use native tool calling.
pub fn detect_parser(model: &str) -> Option<Box<dyn ToolCallParser>> {
    let model_lower = model.to_ascii_lowercase();

    // Qwen3-Coder must be checked before generic qwen
    if model_lower.contains("qwen3") && model_lower.contains("coder") {
        return Some(Box::new(Qwen3CoderParser));
    }

    // Hermes models (NousResearch/Hermes-*)
    if model_lower.contains("hermes") {
        return Some(Box::new(HermesParser));
    }

    // DeepSeek: v3.1 check first (more specific)
    if model_lower.contains("deepseek")
        && (model_lower.contains("v3.1") || model_lower.contains("v31"))
    {
        return Some(Box::new(DeepSeekV31Parser));
    }

    // DeepSeek V3
    if model_lower.contains("deepseek") && model_lower.contains("v3") {
        return Some(Box::new(DeepSeekV3Parser));
    }

    // Kimi K2
    if model_lower.contains("kimi") && model_lower.contains("k2") {
        return Some(Box::new(KimiK2Parser));
    }

    // GLM models
    if model_lower.contains("glm") {
        if model_lower.contains("4.7") || model_lower.contains("47") {
            return Some(Box::new(Glm47Parser));
        }
        if model_lower.contains("4.5") || model_lower.contains("45") {
            return Some(Box::new(Glm45Parser));
        }
    }

    // Longcat
    if model_lower.contains("longcat") {
        return Some(Box::new(LongcatParser));
    }

    // Mistral (only models that don't support native tool calling)
    // Most hosted Mistral models support native tool calling, so we don't
    // auto-detect. Users can explicitly set parser to "mistral".

    // Llama (only local/vLLM models — OpenRouter Llama uses native)
    // Same reasoning: don't auto-detect, user can set parser to "llama".

    // Qwen 2.5 uses Hermes format
    if model_lower.contains("qwen") && model_lower.contains("2.5") {
        return Some(Box::new(HermesParser));
    }

    None
}

/// Apply a parser to a [`ChatCompletionResponse`], modifying it in-place
/// to populate `tool_calls` from text content if the parser finds matches.
///
/// This is the main integration point — call after receiving the API response.
pub fn normalize_response(
    response: &mut crate::api_types::ChatCompletionResponse,
    parser: &dyn ToolCallParser,
) {
    for choice in &mut response.choices {
        // Skip if native tool calls are already present
        if choice
            .message
            .tool_calls
            .as_ref()
            .map(|tc| !tc.is_empty())
            .unwrap_or(false)
        {
            continue;
        }

        // Try to extract tool calls from text content
        let text = match choice.message.content_text() {
            Some(text) if !text.is_empty() => text.to_owned(),
            _ => continue,
        };

        if let Some(result) = parser.parse(&text) {
            use crate::api_types::MessageContent;

            choice.message.tool_calls = Some(result.tool_calls);
            choice.message.content = result.content.map(MessageContent::Text);
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Hermes --

    #[test]
    fn hermes_parses_single_tool_call() {
        let text = r#"Let me search for that.
<tool_call>{"name": "web_search", "arguments": {"query": "rust programming"}}</tool_call>"#;

        let parser = HermesParser;
        let result = parser.parse(text).expect("should parse");

        assert_eq!(result.content.as_deref(), Some("Let me search for that."));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].function.name, "web_search");
        assert!(result.tool_calls[0].function.arguments.contains("rust programming"));
    }

    #[test]
    fn hermes_parses_multiple_tool_calls() {
        let text = r#"<tool_call>{"name": "read_file", "arguments": {"path": "/a.txt"}}</tool_call>
<tool_call>{"name": "read_file", "arguments": {"path": "/b.txt"}}</tool_call>"#;

        let parser = HermesParser;
        let result = parser.parse(text).expect("should parse");

        assert!(result.content.is_none());
        assert_eq!(result.tool_calls.len(), 2);
    }

    #[test]
    fn hermes_handles_truncated_tag() {
        let text = r#"<tool_call>{"name": "search", "arguments": {"q": "test"}}"#;

        let parser = HermesParser;
        let result = parser.parse(text).expect("should parse truncated");

        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].function.name, "search");
    }

    #[test]
    fn hermes_returns_none_for_no_tool_calls() {
        let text = "Just a normal response with no tool calls.";
        assert!(HermesParser.parse(text).is_none());
    }

    // -- Longcat --

    #[test]
    fn longcat_parses_tool_call() {
        let text = r#"Thinking...
<longcat_tool_call>{"name": "search", "arguments": {"q": "hello"}}</longcat_tool_call>"#;

        let result = LongcatParser.parse(text).expect("should parse");
        assert_eq!(result.content.as_deref(), Some("Thinking..."));
        assert_eq!(result.tool_calls[0].function.name, "search");
    }

    // -- Mistral --

    #[test]
    fn mistral_pre_v11_json_array() {
        let text = r#"Here's what I found.
[TOOL_CALLS] [{"name": "get_weather", "arguments": {"city": "NYC"}}]"#;

        let result = MistralParser.parse(text).expect("should parse");
        assert_eq!(
            result.content.as_deref(),
            Some("Here's what I found.")
        );
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].function.name, "get_weather");
    }

    #[test]
    fn mistral_v11_format() {
        let text = r#"[TOOL_CALLS]get_weather{"city": "NYC"}"#;

        let result = MistralParser.parse(text).expect("should parse");
        assert!(result.content.is_none());
        assert_eq!(result.tool_calls[0].function.name, "get_weather");
    }

    #[test]
    fn mistral_returns_none_without_marker() {
        assert!(MistralParser.parse("normal text").is_none());
    }

    // -- Llama --

    #[test]
    fn llama_parses_json_object() {
        let text = r#"I'll look that up.
{"name": "search", "arguments": {"query": "rust"}}"#;

        let result = LlamaParser.parse(text).expect("should parse");
        assert_eq!(result.content.as_deref(), Some("I'll look that up."));
        assert_eq!(result.tool_calls[0].function.name, "search");
    }

    #[test]
    fn llama_parses_with_python_tag() {
        let text = r#"<|python_tag|>{"name": "run_code", "parameters": {"code": "print(1)"}}"#;

        let result = LlamaParser.parse(text).expect("should parse");
        assert_eq!(result.tool_calls[0].function.name, "run_code");
        assert!(result.tool_calls[0].function.arguments.contains("print(1)"));
    }

    #[test]
    fn llama_supports_parameters_key() {
        let text = r#"{"name": "func", "parameters": {"x": 1}}"#;
        let result = LlamaParser.parse(text).expect("should parse");
        assert_eq!(result.tool_calls[0].function.name, "func");
    }

    #[test]
    fn llama_ignores_non_tool_json() {
        let text = r#"The config is {"setting": true, "value": 42}."#;
        assert!(LlamaParser.parse(text).is_none());
    }

    // -- DeepSeek V3 --

    #[test]
    fn deepseek_v3_parses_tool_call() {
        let text = "Let me check.\n<｜tool▁calls▁begin｜>\n<｜tool▁call▁begin｜>function<｜tool▁sep｜>get_weather\n```json\n{\"city\": \"NYC\"}\n```<｜tool▁call▁end｜>\n<｜tool▁calls▁end｜>";

        let result = DeepSeekV3Parser.parse(text).expect("should parse");
        assert_eq!(result.content.as_deref(), Some("Let me check."));
        assert_eq!(result.tool_calls[0].function.name, "get_weather");
    }

    #[test]
    fn deepseek_v3_returns_none_without_markers() {
        assert!(DeepSeekV3Parser.parse("normal text").is_none());
    }

    // -- DeepSeek V3.1 --

    #[test]
    fn deepseek_v31_parses_tool_call() {
        let text = "<｜tool▁calls▁begin｜>\n<｜tool▁call▁begin｜>search<｜tool▁sep｜>{\"q\": \"test\"}<｜tool▁call▁end｜>\n<｜tool▁calls▁end｜>";

        let result = DeepSeekV31Parser.parse(text).expect("should parse");
        assert!(result.content.is_none());
        assert_eq!(result.tool_calls[0].function.name, "search");
    }

    // -- Kimi K2 --

    #[test]
    fn kimi_k2_parses_tool_call() {
        let text = "<|tool_calls_section_begin|>\n<|tool_call_begin|>functions.get_weather:0<|tool_call_argument_begin|>{\"location\": \"NYC\"}<|tool_call_end|>\n<|tool_calls_section_end|>";

        let result = KimiK2Parser.parse(text).expect("should parse");
        assert_eq!(result.tool_calls[0].function.name, "get_weather");
        assert_eq!(result.tool_calls[0].id, "functions.get_weather:0");
    }

    #[test]
    fn kimi_k2_extracts_name_from_dotted_id() {
        let text = "<|tool_calls_section_begin|>\n<|tool_call_begin|>tools.web.search:1<|tool_call_argument_begin|>{}<|tool_call_end|>\n<|tool_calls_section_end|>";

        let result = KimiK2Parser.parse(text).expect("should parse");
        assert_eq!(result.tool_calls[0].function.name, "search");
    }

    // -- GLM 4.5 --

    #[test]
    fn glm45_parses_tool_call() {
        let text = "Checking weather.\n<tool_call>get_weather\n<arg_key>location</arg_key><arg_value>NYC</arg_value>\n<arg_key>units</arg_key><arg_value>celsius</arg_value>\n</tool_call>";

        let result = Glm45Parser.parse(text).expect("should parse");
        assert_eq!(result.content.as_deref(), Some("Checking weather."));
        assert_eq!(result.tool_calls[0].function.name, "get_weather");
        let args: serde_json::Value =
            serde_json::from_str(&result.tool_calls[0].function.arguments).unwrap();
        assert_eq!(args["location"], "NYC");
        assert_eq!(args["units"], "celsius");
    }

    // -- Qwen3-Coder --

    #[test]
    fn qwen3_coder_parses_tool_call() {
        let text = "<tool_call>\n<function=get_weather>\n<parameter=location>NYC</parameter>\n<parameter=units>celsius</parameter>\n</function>\n</tool_call>";

        let result = Qwen3CoderParser.parse(text).expect("should parse");
        assert_eq!(result.tool_calls[0].function.name, "get_weather");
        let args: serde_json::Value =
            serde_json::from_str(&result.tool_calls[0].function.arguments).unwrap();
        assert_eq!(args["location"], "NYC");
        assert_eq!(args["units"], "celsius");
    }

    #[test]
    fn qwen3_coder_handles_truncated() {
        let text = "<tool_call>\n<function=search>\n<parameter=query>rust</parameter>";

        let result = Qwen3CoderParser.parse(text).expect("should parse truncated");
        assert_eq!(result.tool_calls[0].function.name, "search");
    }

    // -- Registry --

    #[test]
    fn get_parser_returns_known_parsers() {
        assert!(get_parser("hermes").is_some());
        assert!(get_parser("qwen").is_some());
        assert!(get_parser("mistral").is_some());
        assert!(get_parser("llama").is_some());
        assert!(get_parser("deepseek_v3").is_some());
        assert!(get_parser("deepseek_v31").is_some());
        assert!(get_parser("kimi_k2").is_some());
        assert!(get_parser("glm45").is_some());
        assert!(get_parser("glm47").is_some());
        assert!(get_parser("qwen3_coder").is_some());
    }

    #[test]
    fn get_parser_returns_none_for_unknown() {
        assert!(get_parser("nonexistent").is_none());
    }

    // -- Auto-detection --

    #[test]
    fn detect_hermes_model() {
        let p = detect_parser("NousResearch/Hermes-3-Llama-3.1-70B").unwrap();
        assert_eq!(p.name(), "hermes");
    }

    #[test]
    fn detect_deepseek_v3() {
        let p = detect_parser("deepseek-ai/DeepSeek-V3").unwrap();
        assert_eq!(p.name(), "deepseek_v3");
    }

    #[test]
    fn detect_deepseek_v31() {
        let p = detect_parser("deepseek-ai/DeepSeek-V3.1").unwrap();
        assert_eq!(p.name(), "deepseek_v31");
    }

    #[test]
    fn detect_kimi_k2() {
        let p = detect_parser("moonshotai/kimi-k2").unwrap();
        assert_eq!(p.name(), "kimi_k2");
    }

    #[test]
    fn detect_qwen3_coder() {
        let p = detect_parser("Qwen/Qwen3-Coder-235B").unwrap();
        assert_eq!(p.name(), "qwen3_coder");
    }

    #[test]
    fn detect_qwen25() {
        let p = detect_parser("Qwen/Qwen2.5-72B-Instruct").unwrap();
        assert_eq!(p.name(), "hermes");
    }

    #[test]
    fn no_parser_for_native_models() {
        assert!(detect_parser("gpt-4.1").is_none());
        assert!(detect_parser("claude-sonnet-4-20250514").is_none());
        assert!(detect_parser("gemini-2.5-pro").is_none());
    }

    // -- normalize_response --

    #[test]
    fn normalize_response_populates_tool_calls() {
        use crate::api_types::*;

        let mut response = ChatCompletionResponse {
            id: "test".to_owned(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_owned(),
                    content: Some(MessageContent::Text(
                        r#"Searching now.
<tool_call>{"name": "web_search", "arguments": {"query": "rust"}}</tool_call>"#
                            .to_owned(),
                    )),
                    thinking: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                finish_reason: Some("stop".to_owned()),
            }],
            usage: None,
        };

        normalize_response(&mut response, &HermesParser);

        let tc = response.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("tool_calls should be populated");
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "web_search");
        assert_eq!(
            response.choices[0].message.content_text(),
            Some("Searching now.")
        );
    }

    #[test]
    fn normalize_response_skips_when_native_tool_calls_present() {
        use crate::api_types::*;

        let existing_tc = vec![ToolCallEntry {
            id: "existing".to_owned(),
            call_type: "function".to_owned(),
            function: FunctionCall {
                name: "native_tool".to_owned(),
                arguments: "{}".to_owned(),
            },
        }];

        let mut response = ChatCompletionResponse {
            id: "test".to_owned(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_owned(),
                    content: Some(MessageContent::Text("some text".to_owned())),
                    thinking: None,
                    tool_calls: Some(existing_tc.clone()),
                    tool_call_id: None,
                    name: None,
                },
                finish_reason: Some("stop".to_owned()),
            }],
            usage: None,
        };

        normalize_response(&mut response, &HermesParser);

        // Should not be modified
        let tc = response.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "existing");
    }
}
