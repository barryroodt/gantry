use async_trait::async_trait;
use rig_core::{
    client::CompletionClient,
    completion::{CompletionModel, CompletionRequest, ToolDefinition},
    message::{AssistantContent, Message},
    providers::anthropic::{
        self,
        completion::{CompletionModel as AnthropicCompletionModel, ANTHROPIC_VERSION_LATEST},
    },
    OneOrMany,
};

use crate::cli::Provider;

use super::retry::{classify_error, with_retry, RetryConfig};
use super::{
    rig_convert::chat_messages_to_rig, ChatMessage, ProviderAdapter, ProviderResponse,
    ToolCallRequest, ToolSchema,
};

/// Per-request output-token cap sent to the Anthropic Messages API when rig's
/// model table has no entry for the model id. rig REQUIRES `max_tokens` and
/// derives it from a built-in table of hosted Claude ids; an
/// Anthropic-compatible local server (e.g. oMLX) serving a custom id is not in
/// that table, so without a fallback rig hard-fails before sending. 8192 is
/// ample for agentic turns (mostly tool calls) while leaving context headroom
/// on smaller local models.
const LOCAL_ANTHROPIC_FALLBACK_MAX_TOKENS: u64 = 8192;

/// Resolve the per-request `max_tokens`: rig's per-model value when known
/// (hosted Claude behavior unchanged), else [`LOCAL_ANTHROPIC_FALLBACK_MAX_TOKENS`].
fn resolve_max_tokens(model_default: Option<u64>) -> u64 {
    model_default.unwrap_or(LOCAL_ANTHROPIC_FALLBACK_MAX_TOKENS)
}

pub struct AnthropicProvider {
    model: String,
    client: anthropic::Client,
}

impl AnthropicProvider {
    pub fn new(model: String) -> anyhow::Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

        let mut builder = anthropic::Client::builder()
            .api_key(api_key)
            .anthropic_version(ANTHROPIC_VERSION_LATEST)
            .anthropic_beta("prompt-caching-2024-07-31");

        if let Ok(base_url) = std::env::var("ANTHROPIC_API_BASE") {
            builder = builder.base_url(base_url);
        }

        let client = builder
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build Anthropic client: {e}"))?;

        Ok(Self { model, client })
    }

    fn completion_model(&self) -> AnthropicCompletionModel {
        self.client
            .completion_model(&self.model)
            .with_prompt_caching()
    }
}

#[async_trait]
impl ProviderAdapter for AnthropicProvider {
    fn provider(&self) -> Provider {
        Provider::Anthropic
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        let config = RetryConfig::default();
        with_retry(&config, classify_error, || {
            Box::pin(self.complete_once(system, messages, tools))
        })
        .await
    }
}

impl AnthropicProvider {
    async fn complete_once(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        let rig_messages = chat_messages_to_rig(messages)?;
        let chat_history = if rig_messages.is_empty() {
            OneOrMany::one(Message::user(""))
        } else {
            OneOrMany::many(rig_messages)
                .map_err(|_| anyhow::anyhow!("chat history must not be empty"))?
        };

        let model = self.completion_model();
        // rig REQUIRES `max_tokens` for Anthropic and derives it from its own
        // per-model table (`default_max_tokens`), which only knows hosted
        // Claude ids. An Anthropic-compatible local server (e.g. oMLX serving a
        // custom id) yields `None`, and rig then hard-fails with "`max_tokens`
        // must be set for Anthropic" before the request leaves the process.
        // Use rig's per-model value when known (hosted behavior unchanged) and
        // a conservative fallback otherwise.
        let max_tokens = resolve_max_tokens(model.default_max_tokens);

        let request = CompletionRequest {
            model: None,
            preamble: Some(system.to_string()),
            chat_history,
            documents: vec![],
            tools: tools
                .iter()
                .map(|tool| ToolDefinition {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.json_schema.clone(),
                })
                .collect(),
            temperature: None,
            max_tokens: Some(max_tokens),
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let response = model
            .completion(request)
            .await
            .map_err(|e| anyhow::anyhow!("anthropic completion failed: {e}"))?;

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for content in response.choice.iter() {
            match content {
                AssistantContent::Text(text) => text_parts.push(text.text.clone()),
                AssistantContent::ToolCall(tool_call) => {
                    tool_calls.push(ToolCallRequest {
                        id: tool_call.id.clone(),
                        name: tool_call.function.name.clone(),
                        args_json: serde_json::to_string(&tool_call.function.arguments)?,
                    });
                }
                AssistantContent::Reasoning(reasoning) => {
                    text_parts.push(reasoning.display_text());
                }
                AssistantContent::Image(_) => {}
            }
        }

        Ok(ProviderResponse {
            text: text_parts.join("\n"),
            tool_calls,
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
            cache_read: response.usage.cached_input_tokens,
            cache_write: response.usage.cache_creation_input_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_max_tokens, LOCAL_ANTHROPIC_FALLBACK_MAX_TOKENS};

    #[test]
    fn known_model_keeps_rig_per_model_cap() {
        // A hosted Claude id rig knows about must pass through untouched, so
        // existing runs keep their model-specific output cap.
        assert_eq!(resolve_max_tokens(Some(128_000)), 128_000);
        assert_eq!(resolve_max_tokens(Some(64_000)), 64_000);
    }

    #[test]
    fn unknown_model_uses_fallback_not_none() {
        // A custom id from an Anthropic-compatible local server has no rig
        // table entry; we must still send a concrete cap (never `None`, which
        // makes rig hard-fail with "`max_tokens` must be set for Anthropic").
        assert_eq!(
            resolve_max_tokens(None),
            LOCAL_ANTHROPIC_FALLBACK_MAX_TOKENS
        );
    }
}
