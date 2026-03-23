use genesis_provider::{ChatCompletionRequest, ChatMessage, ContentPart, MessageContent};
use tracing::{info, warn};

use super::AgentLoop;

impl AgentLoop {
    /// Replace old tool result content with a compact placeholder to reduce
    /// token usage without an LLM call. Preserves tool call (assistant)
    /// messages so the reasoning chain remains intact.
    ///
    /// Based on "The Complexity Trap" (NeurIPS 2025): observation masking
    /// achieves ~52% cost reduction while maintaining or improving solve rate.
    pub(crate) fn mask_old_tool_outputs(&mut self) {
        /// Number of recent messages to protect from masking (approximately
        /// the last 4 assistant + tool result pairs).
        const PROTECT_RECENT: usize = 8;
        /// Only mask tool outputs longer than this many bytes.
        const MIN_CONTENT_LEN: usize = 200;

        let has_system = self.messages.first().is_some_and(|m| m.role == "system");
        let start = if has_system { 1 } else { 0 };
        let end = self.messages.len().saturating_sub(PROTECT_RECENT);

        if end <= start {
            return;
        }

        let mut masked_count = 0u32;
        for msg in &mut self.messages[start..end] {
            if msg.role == "tool" {
                if let Some(ref content) = msg.content {
                    let text_len = match content {
                        MessageContent::Text(t) => t.len(),
                        MessageContent::Parts(parts) => {
                            // Skip masking if any part is non-text (e.g. images)
                            // to avoid silently discarding non-text content.
                            let all_text =
                                parts.iter().all(|p| matches!(p, ContentPart::Text { .. }));
                            if !all_text {
                                continue;
                            }
                            parts
                                .iter()
                                .map(|p| match p {
                                    ContentPart::Text { text } => text.len(),
                                    _ => 0,
                                })
                                .sum()
                        }
                    };
                    if text_len > MIN_CONTENT_LEN {
                        msg.content = Some(MessageContent::Text(
                            "[Tool output masked — see preceding tool call for context]".to_owned(),
                        ));
                        masked_count += 1;
                    }
                }
            }
        }

        if masked_count > 0 {
            info!(
                masked_count,
                "masked old tool outputs to reduce context tokens"
            );
        }
    }

    /// Prune messages to stay within context limits, preserving the system
    /// prompt at index 0 (if present) and the most recent messages.
    ///
    /// Two triggers:
    /// 1. **Message count**: `max_context_messages` caps total non-system messages.
    /// 2. **Token count**: `max_context_tokens` triggers when the last API call's
    ///    prompt_tokens exceeds 85% of the limit, compressing the middle of the
    ///    conversation while protecting the first 3 and last 4 non-system messages.
    ///
    /// Before dropping old messages, the agent calls the LLM to produce a
    /// concise summary. This summary is inserted as a system message right
    /// after the main system prompt so the agent retains awareness of context.
    pub(crate) async fn prune_context(&mut self) {
        let has_system = self.messages.first().is_some_and(|m| m.role == "system");
        let drop_start = if has_system { 1 } else { 0 };
        let non_system_count = self.messages.len() - drop_start;

        // Determine how many messages to drop.
        let drop_count = self.compute_drop_count(non_system_count, drop_start);

        if drop_count == 0 {
            return;
        }

        // Lightweight first pass: mask old tool outputs (no LLM call).
        // Only runs when context is actually under pressure (drop_count > 0).
        self.mask_old_tool_outputs();

        // Extract the messages we're about to drop and summarize them.
        let to_drop: Vec<ChatMessage> = self.messages[drop_start..drop_start + drop_count].to_vec();

        info!(
            drop_count,
            remaining = non_system_count - drop_count,
            trigger = if self.token_compression_needed() {
                "tokens"
            } else {
                "messages"
            },
            "pruning conversation context"
        );

        let summary = self.summarize_messages(&to_drop).await;

        let messages_before = self.messages.len();
        // Remove the old messages.
        self.messages.drain(drop_start..drop_start + drop_count);

        // Inject the summary right after the system prompt (or at position 0).
        if let Some(text) = summary {
            let summary_msg = ChatMessage::system(format!("[Prior conversation summary]\n{text}"));
            self.messages.insert(drop_start, summary_msg);
        }

        let hook_session = self.session_id_str().to_owned();
        self.hooks
            .on_context_prune(&hook_session, messages_before, self.messages.len());
    }

    /// Check if token-based compression should trigger (>85% of max_context_tokens).
    pub(crate) fn token_compression_needed(&self) -> bool {
        if let Some(max_tokens) = self.config.max_context_tokens {
            let threshold = (max_tokens as f64
                * genesis_config::defaults::limits::CONTEXT_COMPRESSION_THRESHOLD)
                as u32;
            self.last_prompt_tokens > threshold
        } else {
            false
        }
    }

    /// Compute how many messages to drop. Returns 0 if no pruning needed.
    ///
    /// Prefers token-based compression (protects first 3 + last 4) over
    /// simple message-count pruning. If both triggers fire, uses whichever
    /// drops more messages.
    pub(crate) fn compute_drop_count(&self, non_system_count: usize, _drop_start: usize) -> usize {
        let mut drop = 0;

        // Message-count trigger.
        if let Some(limit) = self.config.max_context_messages {
            if non_system_count > limit {
                drop = non_system_count - limit;
            }
        }

        // Token-count trigger: protect first 3 and last 4 non-system messages.
        if self.token_compression_needed() {
            let protect_head = 3usize;
            let protect_tail = 4usize;
            let protected = protect_head + protect_tail;
            if non_system_count > protected {
                let token_drop = non_system_count - protected;
                // Use whichever drops more to aggressively reclaim context.
                drop = drop.max(token_drop);
            }
        }

        drop
    }

    /// Ask the LLM to produce a compact summary of a slice of conversation
    /// messages. Returns `None` on any failure so the caller can degrade
    /// gracefully to plain pruning.
    pub(crate) async fn summarize_messages(&self, messages: &[ChatMessage]) -> Option<String> {
        if messages.is_empty() {
            return None;
        }

        // Build a transcript for the summarizer.
        let mut transcript = String::new();
        for msg in messages {
            let role = &msg.role;
            let content = msg.content_text().unwrap_or("[tool call]");
            // Truncate very long tool results to keep the summarization prompt small.
            let truncated = match content.char_indices().nth(500) {
                Some((i, _)) => format!("{}...", &content[..i]),
                None => content.to_owned(),
            };
            transcript.push_str(&format!("{role}: {truncated}\n"));
        }

        let prompt = format!(
            "Summarize the following conversation excerpt in 2-4 sentences. \
             Focus on: key decisions made, tasks completed, important facts \
             established, and any open questions. Be factual and concise.\n\n\
             ---\n{transcript}---"
        );

        let request = ChatCompletionRequest {
            model: String::new(), // client fills this in
            messages: vec![ChatMessage::user(&prompt)],
            tools: Vec::new(),
            temperature: Some(0.3),
            max_tokens: Some(256),
            stream: None,
            stream_options: None,
            response_format: None,
            tool_choice: None,
            thinking: None,
            extra_body: None,
        };

        match self.client.complete(request).await {
            Ok(response) => {
                let text = response
                    .choices
                    .first()
                    .and_then(|c| c.message.content_text().map(|s| s.to_owned()))
                    .unwrap_or_default();
                if text.is_empty() {
                    None
                } else {
                    info!(
                        summary_len = text.len(),
                        dropped_messages = messages.len(),
                        "summarized pruned context"
                    );
                    Some(text)
                }
            }
            Err(e) => {
                warn!(error = %e, "context summarization failed; dropping messages without summary");
                None
            }
        }
    }
}
