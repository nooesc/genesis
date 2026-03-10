use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const TIMEOUT_SECS: u64 = 30;

/// Shared blocking HTTP client for browse requests.
fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        crate::http::build_blocking_client(Duration::from_secs(TIMEOUT_SECS), |b| {
            b.user_agent("Mozilla/5.0 (compatible; genesis-agent/0.1)")
                .redirect(reqwest::redirect::Policy::limited(5))
        })
    })
}

/// A tool that fetches a web page and extracts readable text content,
/// stripping HTML tags, scripts, styles, and navigation.
pub struct BrowseTool;

impl ToolHandler for BrowseTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let url = call
            .arguments
            .get("url")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "url",
            })?;

        // SSRF protection: validate URL before making request
        crate::url_safety::validate_url(url).map_err(|reason| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason,
        })?;

        let client = http_client();
        let response = client.get(url).send().map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to fetch URL: {e}"),
        })?;

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        let body = response.text().map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to read response body: {e}"),
        })?;

        // Extract readable text based on content type
        let text = if content_type.contains("text/html") || content_type.contains("application/xhtml") {
            let selector = call.arguments.get("selector").map(|s| s.as_str());
            extract_text_from_html(&body, selector)
        } else if content_type.contains("text/") || content_type.contains("application/json") {
            // Plain text or JSON — return as-is
            body
        } else {
            format!("(binary content: {content_type}, {} bytes)", body.len())
        };

        // Truncate at char boundary
        let truncated = if text.len() > MAX_RESPONSE_BYTES {
            let mut end = MAX_RESPONSE_BYTES;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            let mut t = text[..end].to_string();
            t.push_str("\n... (content truncated)");
            t
        } else {
            text
        };

        let mut content = format!("URL: {url}\nHTTP {status}\n\n{truncated}");
        // Collapse excessive blank lines for cleaner output
        while content.contains("\n\n\n") {
            content = content.replace("\n\n\n", "\n\n");
        }

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("status".to_owned(), status.to_string()),
                ("url".to_owned(), url.clone()),
            ]),
        })
    }
}

/// Extract readable text from HTML, stripping tags, scripts, styles.
/// If `selector` is provided, only extracts content from matching elements
/// (supports simple tag names like "article", "main", "p").
fn extract_text_from_html(html: &str, selector: Option<&str>) -> String {
    // If a selector is given, try to extract only content within those tags
    let working_html = if let Some(tag) = selector {
        extract_tag_contents(html, tag)
    } else {
        html.to_owned()
    };

    // Remove script, style, and navigation blocks entirely.
    // Pre-compute lowercase once and pass it through to avoid re-lowercasing on each call.
    let cleaned = remove_tag_blocks_multi(&working_html, &["script", "style", "nav", "header", "footer"]);

    // Strip all remaining HTML tags
    let text = strip_tags(&cleaned);

    // Decode common HTML entities
    let text = decode_entities(&text);

    // Normalize whitespace
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    lines.join("\n")
}

/// Remove all occurrences of multiple tag types in sequence, computing
/// lowercase only once.
fn remove_tag_blocks_multi(html: &str, tags: &[&str]) -> String {
    let mut current = html.to_owned();
    let mut lower = html.to_lowercase();
    let last = tags.len().saturating_sub(1);
    for (i, tag) in tags.iter().enumerate() {
        let result = remove_tag_blocks_with_lower(&current, &lower, tag);
        if i < last {
            lower = result.to_lowercase();
        }
        current = result;
    }
    current
}

/// Remove all occurrences of <tag>...</tag> blocks (including the tags).
#[cfg(test)]
fn remove_tag_blocks(html: &str, tag: &str) -> String {
    let lower = html.to_lowercase();
    remove_tag_blocks_with_lower(html, &lower, tag)
}

/// Remove tag blocks using a pre-computed lowercase version of the HTML.
fn remove_tag_blocks_with_lower(html: &str, lower: &str, tag: &str) -> String {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut result = String::with_capacity(html.len());
    let mut pos = 0;

    while let Some(start) = lower[pos..].find(&open) {
        let abs_start = pos + start;
        result.push_str(&html[pos..abs_start]);
        if let Some(end) = lower[abs_start..].find(&close) {
            pos = abs_start + end + close.len();
        } else {
            pos = html.len();
        }
    }
    result.push_str(&html[pos..]);
    result
}

/// Extract content from within all instances of a given tag.
fn extract_tag_contents(html: &str, tag: &str) -> String {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let lower = html.to_lowercase();
    let mut result = String::new();
    let mut pos = 0;

    while let Some(start) = lower[pos..].find(&open) {
        let abs_start = pos + start;
        // Find the end of the opening tag (the '>')
        if let Some(tag_end) = html[abs_start..].find('>') {
            let content_start = abs_start + tag_end + 1;
            if let Some(end) = lower[content_start..].find(&close) {
                result.push_str(&html[content_start..content_start + end]);
                result.push('\n');
                pos = content_start + end + close.len();
            } else {
                pos = html.len();
            }
        } else {
            break;
        }
    }

    if result.is_empty() {
        // Selector didn't match anything — fall back to full HTML
        html.to_owned()
    } else {
        result
    }
}

/// Strip all HTML tags from text.
fn strip_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }

    result
}

/// Decode common HTML entities in a single pass.
fn decode_entities(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        if ch != '&' {
            result.push(ch);
            continue;
        }

        // Find the closing ';'
        let rest = &text[i..];
        if let Some(semi) = rest.find(';') {
            let entity = &rest[..semi + 1];
            let replacement = match entity {
                "&amp;" => "&",
                "&lt;" => "<",
                "&gt;" => ">",
                "&quot;" => "\"",
                "&#39;" | "&apos;" | "&#x27;" => "'",
                "&nbsp;" => " ",
                "&#x2F;" => "/",
                "&mdash;" => "\u{2014}",
                "&ndash;" => "\u{2013}",
                "&hellip;" => "\u{2026}",
                _ => {
                    result.push('&');
                    continue;
                }
            };
            result.push_str(replacement);
            // Skip past the entity in the char iterator
            for _ in 0..entity.len() - 1 {
                chars.next();
            }
        } else {
            result.push('&');
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

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

    #[test]
    fn requires_url_argument() {
        let tool = BrowseTool;
        let call = ToolCall {
            name: "browse".to_owned(),
            arguments: BTreeMap::new(),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn handles_connection_failure() {
        let tool = BrowseTool;
        let call = ToolCall {
            name: "browse".to_owned(),
            // Use a public IP with a closed port (not a private IP)
            arguments: BTreeMap::from([("url".to_owned(), "http://203.0.113.1:1".to_owned())]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn blocks_private_ips() {
        let tool = BrowseTool;
        for url in [
            "http://localhost",
            "http://127.0.0.1",
            "http://10.0.0.1",
            "http://169.254.169.254",
            "file:///etc/passwd",
        ] {
            let call = ToolCall {
                name: "browse".to_owned(),
                arguments: BTreeMap::from([("url".to_owned(), url.to_owned())]),
            };
            let err = tool.run(&call, &ctx()).unwrap_err();
            assert!(
                matches!(err, ToolError::ExecutionFailed { .. }),
                "expected {url} to be blocked"
            );
        }
    }

    #[test]
    fn strip_tags_removes_html() {
        assert_eq!(strip_tags("<p>Hello <b>world</b></p>"), "Hello world");
        assert_eq!(strip_tags("no tags here"), "no tags here");
        assert_eq!(strip_tags("<div><span>nested</span></div>"), "nested");
    }

    #[test]
    fn remove_tag_blocks_strips_scripts() {
        let html = "before<script>alert('xss')</script>after";
        assert_eq!(remove_tag_blocks(html, "script"), "beforeafter");
    }

    #[test]
    fn remove_tag_blocks_case_insensitive() {
        let html = "before<SCRIPT type='text/javascript'>evil()</SCRIPT>after";
        assert_eq!(remove_tag_blocks(html, "script"), "beforeafter");
    }

    #[test]
    fn extract_tag_contents_finds_article() {
        let html = "<html><nav>menu</nav><article><p>content here</p></article><footer>foot</footer></html>";
        let result = extract_tag_contents(html, "article");
        assert!(result.contains("<p>content here</p>"));
        assert!(!result.contains("menu"));
    }

    #[test]
    fn extract_tag_contents_falls_back_on_no_match() {
        let html = "<p>just a paragraph</p>";
        let result = extract_tag_contents(html, "article");
        assert_eq!(result, html);
    }

    #[test]
    fn decode_entities_common() {
        assert_eq!(decode_entities("Tom &amp; Jerry"), "Tom & Jerry");
        assert_eq!(decode_entities("a &lt; b &gt; c"), "a < b > c");
        assert_eq!(decode_entities("&quot;hello&quot;"), "\"hello\"");
        assert_eq!(decode_entities("it&#39;s"), "it's");
    }

    #[test]
    fn extract_text_from_html_full() {
        let html = r#"
        <html>
        <head><title>Test Page</title></head>
        <body>
        <nav><a href="/">Home</a></nav>
        <script>console.log('hi');</script>
        <style>.foo { color: red; }</style>
        <article>
            <h1>Hello World</h1>
            <p>This is a &amp; test paragraph.</p>
        </article>
        <footer>Copyright 2026</footer>
        </body>
        </html>
        "#;

        let text = extract_text_from_html(html, None);
        assert!(text.contains("Test Page"));
        assert!(text.contains("Hello World"));
        assert!(text.contains("This is a & test paragraph."));
        // Scripts, styles, nav, footer should be removed
        assert!(!text.contains("console.log"));
        assert!(!text.contains(".foo"));
        assert!(!text.contains("Home"));
        assert!(!text.contains("Copyright"));
    }

    #[test]
    fn extract_text_with_selector() {
        let html = r#"
        <html>
        <body>
        <div class="sidebar">Navigation stuff</div>
        <main>
            <h1>Main Content</h1>
            <p>Important text here.</p>
        </main>
        <div class="ads">Buy stuff!</div>
        </body>
        </html>
        "#;

        let text = extract_text_from_html(html, Some("main"));
        assert!(text.contains("Main Content"));
        assert!(text.contains("Important text here."));
        assert!(!text.contains("Navigation stuff"));
        assert!(!text.contains("Buy stuff"));
    }
}
