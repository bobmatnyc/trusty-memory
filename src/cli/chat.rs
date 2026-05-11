//! `chat` subcommand: ask OpenRouter a question with palace memory injected.
//!
//! Why: The whole point of trusty-memory is to ground LLM replies in stored
//! context. A built-in `chat` lets users see that value in one command without
//! wiring up a client app — and gives us a clean integration test surface for
//! the L0+L1+L2 retrieval path end-to-end.
//! What: Loads `~/.trusty-memory/config.toml`, opens the requested palace,
//! runs `recall(query, top_k=10)`, builds a token-budgeted memory context
//! string, posts to the OpenRouter chat-completions API, and prints the
//! assistant message. With `--remember` it stores the response back into the
//! palace.
//! Test: `chat_builds_context_under_budget` verifies the budgeted context
//! truncation. The full HTTP path is exercised manually (no mock server in
//! this iteration).

use crate::cli::config::UserConfig;
use crate::cli::output::OutputConfig;
use crate::cli::palace::data_root;
use anyhow::{Context, Result};
use std::sync::Arc;
use tiktoken_rs::cl100k_base;
use trusty_common::{openrouter_chat, ChatMessage};
use trusty_memory_core::retrieval::{recall, RecallResult};
use trusty_memory_core::{embed::FastEmbedder, PalaceId, PalaceRegistry};

const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a helpful assistant grounded by the user's trusty-memory palace. \
Use the MEMORY CONTEXT below as background when relevant. If the context \
does not cover the question, answer from general knowledge and say so.";

/// CLI options for the chat subcommand.
#[derive(Debug, Clone)]
pub struct ChatOpts {
    pub message: String,
    pub remember: bool,
    pub top_k: usize,
}

/// Build a memory-context string from L0+L1+L2 recall results, truncated to
/// `max_tokens` cl100k tokens.
///
/// Why: LLMs charge per token and have hard context limits; we must respect
/// the user-configured budget regardless of how much memory matched.
/// What: Renders `[layer L<n>] [<room_or_identity>] <content>` lines, accumulates
/// them while the running cl100k token count stays at or below `max_tokens`,
/// and stops at the first line that would push us over.
/// Test: `chat_builds_context_under_budget` enforces the cap with 20 long
/// drawers and a small budget.
pub fn build_memory_context(results: &[RecallResult], max_tokens: usize) -> Result<String> {
    if max_tokens == 0 || results.is_empty() {
        return Ok(String::new());
    }
    let bpe = cl100k_base().context("load cl100k_base tokenizer")?;

    let mut out = String::new();
    let mut used: usize = 0;
    for r in results {
        let label = match r.layer {
            0 => "identity".to_string(),
            1 => "essential".to_string(),
            2 => "topic".to_string(),
            3 => "deep".to_string(),
            other => format!("L{other}"),
        };
        let line = format!("[{label}] {}\n", r.drawer.content.trim());
        let cost = bpe.encode_with_special_tokens(&line).len();
        if used.saturating_add(cost) > max_tokens {
            break;
        }
        used += cost;
        out.push_str(&line);
    }
    Ok(out)
}

/// Run a chat turn against OpenRouter using palace memory as context.
pub async fn handle_chat(palace_id_str: &str, opts: ChatOpts, out: &OutputConfig) -> Result<()> {
    let cfg = UserConfig::load().context("load user config")?;
    if cfg.openrouter.api_key.is_empty() {
        anyhow::bail!(
            "OpenRouter API key not configured. Run `trusty-memory config set openrouter.api_key sk-or-...` \
or `trusty-memory setup`."
        );
    }

    out.print_header(palace_id_str, "chat");

    // Open palace handle (blocking IO -> spawn_blocking).
    let root = data_root()?;
    let palace_id = PalaceId::new(palace_id_str.to_string());
    let palace_id_clone = palace_id.clone();
    let root_clone = root.clone();
    let handle = tokio::task::spawn_blocking(move || -> Result<_> {
        let reg = PalaceRegistry::new();
        reg.open_palace(&root_clone, &palace_id_clone)
            .with_context(|| format!("open palace {palace_id_clone}"))
    })
    .await
    .context("join open_palace")??;

    // Embed + retrieve. Reuse a per-call FastEmbedder; daemon mode caches it
    // separately on AppState.
    let embedder = FastEmbedder::new()
        .await
        .context("initialize FastEmbedder")?;
    let results = recall(&handle, &embedder, &opts.message, opts.top_k)
        .await
        .context("recall from palace")?;

    let context_str = build_memory_context(&results, cfg.openrouter.max_context_tokens)?;

    // Compose system prompt = (custom or default) + memory context.
    let base_prompt = if cfg.openrouter.system_prompt.is_empty() {
        DEFAULT_SYSTEM_PROMPT.to_string()
    } else {
        cfg.openrouter.system_prompt.clone()
    };
    let system_content = if context_str.is_empty() {
        base_prompt
    } else {
        format!("{base_prompt}\n\nMEMORY CONTEXT (palace `{palace_id_str}`):\n{context_str}")
    };

    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: system_content,
            tool_call_id: None,
            tool_calls: None,
        },
        ChatMessage {
            role: "user".into(),
            content: opts.message.clone(),
            tool_call_id: None,
            tool_calls: None,
        },
    ];
    let answer = openrouter_chat(&cfg.openrouter.api_key, &cfg.openrouter.model, messages)
        .await
        .context("openrouter chat completions")?;

    println!("{answer}");

    if opts.remember {
        // Best-effort persist as a drawer; failures here shouldn't fail the
        // chat command itself.
        let palace_name = palace_id.as_str().to_string();
        let answer_for_store = answer.clone();
        let res = tokio::task::spawn_blocking(move || -> Result<()> {
            // Append to the in-memory drawer table and flush identity/L1.
            // Full drawer-write semantics live in #11; this is a minimal hook.
            let reg = PalaceRegistry::new();
            let h = reg.open_palace(&root, &PalaceId::new(palace_name))?;
            let drawer = trusty_memory_core::Drawer::new(uuid::Uuid::nil(), &answer_for_store);
            let h_arc: Arc<_> = h.clone();
            h_arc.add_drawer(drawer);
            h_arc.flush()?;
            Ok(())
        })
        .await
        .context("join remember-after-chat")?;
        if let Err(e) = res {
            tracing::warn!(error = %e, "failed to remember chat response");
        } else {
            out.print_success("response remembered");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_memory_core::palace::Drawer;
    use uuid::Uuid;

    fn fake_results(n: usize, content_len: usize) -> Vec<RecallResult> {
        (0..n)
            .map(|i| {
                let content = format!("drawer-{i}: {}", "lorem ipsum ".repeat(content_len));
                let mut drawer = Drawer::new(Uuid::nil(), &content);
                drawer.importance = 0.5;
                RecallResult {
                    drawer,
                    score: 0.5,
                    layer: 2,
                }
            })
            .collect()
    }

    #[test]
    fn chat_builds_context_under_budget() {
        // 20 drawers, each big enough to blow the budget if we don't truncate.
        let results = fake_results(20, 8);
        let max_tokens = 200usize;
        let ctx = build_memory_context(&results, max_tokens).unwrap();

        let bpe = cl100k_base().unwrap();
        let used = bpe.encode_with_special_tokens(&ctx).len();
        assert!(
            used <= max_tokens,
            "context {used} tokens exceeded budget {max_tokens}"
        );
        // Should still include at least the first drawer.
        assert!(
            ctx.contains("drawer-0"),
            "expected first drawer present: {ctx}"
        );
    }

    #[test]
    fn chat_context_empty_when_no_results() {
        let ctx = build_memory_context(&[], 1000).unwrap();
        assert_eq!(ctx, "");
    }

    #[test]
    fn chat_context_zero_budget_yields_empty() {
        let results = fake_results(3, 1);
        let ctx = build_memory_context(&results, 0).unwrap();
        assert_eq!(ctx, "");
    }
}
