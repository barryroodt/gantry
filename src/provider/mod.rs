use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::cli::Provider;

pub mod anthropic;
pub mod cursor;
pub mod gemini;
pub mod openai;
pub mod retry;
mod rig_convert;

pub use anthropic::AnthropicProvider;
pub use cursor::CursorProvider;
pub use gemini::GeminiProvider;
pub use openai::OpenAiProvider;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCallRequest>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: String,
    pub content: String,
    pub is_error: bool,
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn provider(&self) -> Provider;
    fn model(&self) -> &str;
    /// One round of completion: send system + messages + available tools, get back text + tool calls + token counts.
    async fn complete(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse>;

    /// Whether this provider can return schema-validated structured output via
    /// [`Self::complete_structured`]. Default: true (uses the tool mechanism).
    fn supports_structured_output(&self) -> bool {
        true
    }

    /// Force a single structured result: expose one `respond` tool carrying
    /// `schema` and return its parsed arguments. `Err` if the model did not call
    /// it (the caller may retry or fall back to fence parsing).
    async fn complete_structured(
        &self,
        system: &str,
        messages: &[ChatMessage],
        schema: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let respond = ToolSchema {
            name: "respond".into(),
            description: "Return the final structured result as this tool's arguments.".into(),
            json_schema: schema.clone(),
        };
        let resp = self
            .complete(system, messages, std::slice::from_ref(&respond))
            .await?;
        let call = resp
            .tool_calls
            .iter()
            .find(|c| c.name == "respond")
            .ok_or_else(|| {
                anyhow::anyhow!("model did not return structured output via the respond tool")
            })?;
        serde_json::from_str(&call.args_json)
            .map_err(|e| anyhow::anyhow!("structured output was not valid JSON: {e}"))
    }
}

#[derive(Debug, Clone)]
pub enum ChatMessage {
    User(String),
    Assistant {
        text: String,
        tool_calls: Vec<ToolCallRequest>,
    },
    ToolResults(Vec<ToolResult>),
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub json_schema: serde_json::Value,
}

/// Resolve a `Provider` enum + model string into a boxed adapter. Errors if the
/// requested provider's API key env var is missing or the provider is not yet implemented.
pub fn build_adapter(
    provider: Provider,
    model: String,
) -> anyhow::Result<Box<dyn ProviderAdapter>> {
    match provider {
        Provider::Anthropic => Ok(Box::new(anthropic::AnthropicProvider::new(model)?)),
        Provider::OpenAi => Ok(Box::new(openai::OpenAiProvider::new(model)?)),
        Provider::Gemini => Ok(Box::new(GeminiProvider::new(model)?)),
        Provider::Cursor => Ok(Box::new(cursor::CursorProvider::new(model)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubStructured {
        tool_calls: Vec<ToolCallRequest>,
    }

    #[async_trait]
    impl ProviderAdapter for StubStructured {
        fn provider(&self) -> Provider {
            Provider::OpenAi
        }
        fn model(&self) -> &str {
            "stub"
        }
        async fn complete(
            &self,
            _system: &str,
            _messages: &[ChatMessage],
            _tools: &[ToolSchema],
        ) -> anyhow::Result<ProviderResponse> {
            Ok(ProviderResponse {
                text: String::new(),
                tool_calls: self.tool_calls.clone(),
                input_tokens: 0,
                output_tokens: 0,
                cache_read: 0,
                cache_write: 0,
            })
        }
    }

    #[tokio::test]
    async fn complete_structured_parses_respond_tool_args() {
        let p = StubStructured {
            tool_calls: vec![ToolCallRequest {
                id: "1".into(),
                name: "respond".into(),
                args_json: r#"{"verdict":"ready","count":2}"#.into(),
            }],
        };
        let v = p
            .complete_structured("sys", &[], &serde_json::json!({"type":"object"}))
            .await
            .unwrap();
        assert_eq!(v["verdict"], "ready");
        assert_eq!(v["count"], 2);
    }

    #[tokio::test]
    async fn complete_structured_errs_when_tool_not_called() {
        let p = StubStructured { tool_calls: vec![] };
        let err = p
            .complete_structured("sys", &[], &serde_json::json!({"type":"object"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("did not return structured output"));
    }
}
