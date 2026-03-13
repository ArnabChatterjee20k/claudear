//! Prompt construction for the local code chat.

use super::types::ChatMessage;
use std::fmt::Write;

/// System prompt template for code-aware chat.
const SYSTEM_PROMPT: &str = "\
You are a code assistant. Answer questions about the codebase using the provided code context.
Be precise and reference specific files, functions, and line numbers.
If the code context doesn't contain enough information to answer the question, say so.
Format code references as `file_path:line_number`.";

/// Build a complete chat prompt from code context, conversation history, and user message.
///
/// The returned string is formatted for instruction-following models with a system section,
/// retrieved code context, conversation history, and the current user question.
pub fn build_chat_prompt(
    code_context: &str,
    history: &[ChatMessage],
    user_message: &str,
) -> String {
    let mut prompt = String::with_capacity(code_context.len() + 2048);

    // System instructions
    prompt.push_str("<|system|>\n");
    prompt.push_str(SYSTEM_PROMPT);

    // Code context (from RAG retrieval)
    if !code_context.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(code_context);
    }

    prompt.push_str("\n<|end|>\n");

    // Conversation history
    for msg in history {
        let role_tag = msg.role.as_str();
        let _ = write!(prompt, "<|{role_tag}|>\n{}\n<|end|>\n", msg.content);
    }

    // Current user message
    let _ = write!(prompt, "<|user|>\n{user_message}\n<|end|>\n<|assistant|>\n");

    prompt
}

/// Estimate the token count of a string (rough approximation: ~4 chars per token).
pub fn estimate_tokens(text: &str) -> u32 {
    (text.len() as u32) / 4
}

/// Trim conversation history to fit within a token budget.
///
/// Keeps the most recent messages that fit within `max_tokens`, always
/// preserving the last message pair (user + assistant) if possible.
pub fn trim_history(history: &[ChatMessage], max_tokens: u32) -> Vec<ChatMessage> {
    if history.is_empty() {
        return Vec::new();
    }

    let mut total_tokens = 0u32;
    let mut keep_from = history.len();

    // Walk backwards, accumulating token estimates
    for (i, msg) in history.iter().enumerate().rev() {
        let msg_tokens = estimate_tokens(&msg.content) + 20; // overhead for role tags
        if total_tokens + msg_tokens > max_tokens {
            break;
        }
        total_tokens += msg_tokens;
        keep_from = i;
    }

    history[keep_from..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::types::ChatRole;
    use chrono::Utc;

    fn make_msg(role: ChatRole, content: &str) -> ChatMessage {
        ChatMessage {
            id: None,
            role,
            content: content.to_string(),
            sources_json: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_build_prompt_empty_context() {
        let prompt = build_chat_prompt("", &[], "Hello");
        assert!(prompt.contains(SYSTEM_PROMPT));
        assert!(prompt.contains("<|user|>\nHello\n"));
        assert!(prompt.ends_with("<|assistant|>\n"));
    }

    #[test]
    fn test_build_prompt_with_context_and_history() {
        let history = vec![
            make_msg(ChatRole::User, "What is this?"),
            make_msg(ChatRole::Assistant, "It's a Rust project."),
        ];
        let prompt = build_chat_prompt(
            "## Code\n```rust\nfn main() {}\n```",
            &history,
            "Tell me more",
        );

        assert!(prompt.contains("## Code"));
        assert!(prompt.contains("<|user|>\nWhat is this?"));
        assert!(prompt.contains("<|assistant|>\nIt's a Rust project."));
        assert!(prompt.contains("<|user|>\nTell me more"));
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("a".repeat(400).as_str()), 100);
    }

    #[test]
    fn test_trim_history_empty() {
        assert!(trim_history(&[], 1000).is_empty());
    }

    #[test]
    fn test_trim_history_fits() {
        let history = vec![
            make_msg(ChatRole::User, "Hi"),
            make_msg(ChatRole::Assistant, "Hello!"),
        ];
        let trimmed = trim_history(&history, 10000);
        assert_eq!(trimmed.len(), 2);
    }

    #[test]
    fn test_trim_history_overflow() {
        let long_msg = "x".repeat(10000);
        let history = vec![
            make_msg(ChatRole::User, &long_msg),
            make_msg(ChatRole::Assistant, &long_msg),
            make_msg(ChatRole::User, "short"),
            make_msg(ChatRole::Assistant, "also short"),
        ];
        // Budget only fits the last 2 messages
        let trimmed = trim_history(&history, 100);
        assert!(trimmed.len() <= 2);
    }
}
