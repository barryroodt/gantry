use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::cli::Provider;

pub mod anthropic;
pub mod gemini;
pub mod openai;
pub mod retry;
mod rig_convert;

pub use anthropic::AnthropicProvider;
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

/// Default endpoint for the `local` provider when neither `--base-url` nor
/// `GANTRY_LOCAL_BASE_URL` is set. Includes `/v1` because rig's OpenAI client
/// appends `/chat/completions` to the base URL.
pub const LOCAL_DEFAULT_BASE_URL: &str = "http://localhost:8000/v1";

/// Env var overriding the `local` endpoint (below the `--base-url` flag).
pub const LOCAL_BASE_URL_ENV: &str = "GANTRY_LOCAL_BASE_URL";

/// Env var carrying an optional API key for local servers that enforce auth.
pub const LOCAL_API_KEY_ENV: &str = "GANTRY_LOCAL_API_KEY";

/// Resolve the local endpoint: `--base-url` flag → `GANTRY_LOCAL_BASE_URL` →
/// [`LOCAL_DEFAULT_BASE_URL`].
pub fn resolve_local_base_url(flag: Option<&str>) -> String {
    resolve_base_url(flag, std::env::var(LOCAL_BASE_URL_ENV).ok().as_deref())
}

/// Pure core of [`resolve_local_base_url`] (env value injected for testing).
fn resolve_base_url(flag: Option<&str>, env: Option<&str>) -> String {
    flag.map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| env.map(str::trim).filter(|s| !s.is_empty()))
        .map(str::to_string)
        .unwrap_or_else(|| LOCAL_DEFAULT_BASE_URL.to_string())
}

/// Resolve a `Provider` enum + model string into a boxed adapter. Hosted
/// providers error if their API-key env var is missing; `local` needs no key.
/// `base_url_flag` is the `--base-url` CLI value (used only by `local`).
pub fn build_adapter(
    provider: Provider,
    model: String,
    base_url_flag: Option<String>,
) -> anyhow::Result<Box<dyn ProviderAdapter>> {
    match provider {
        Provider::Anthropic => Ok(Box::new(anthropic::AnthropicProvider::new(model)?)),
        Provider::OpenAi => Ok(Box::new(openai::OpenAiProvider::openai(model)?)),
        Provider::Gemini => Ok(Box::new(GeminiProvider::new(model)?)),
        Provider::Local => {
            let base_url = resolve_local_base_url(base_url_flag.as_deref());
            let api_key = std::env::var(LOCAL_API_KEY_ENV).ok();
            Ok(Box::new(openai::OpenAiProvider::local(
                model, base_url, api_key,
            )?))
        }
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

    #[test]
    fn resolve_base_url_precedence() {
        // flag wins
        assert_eq!(
            resolve_base_url(Some("http://flag/v1"), Some("http://env/v1")),
            "http://flag/v1"
        );
        // blank flag falls through to env
        assert_eq!(
            resolve_base_url(Some("   "), Some("http://env/v1")),
            "http://env/v1"
        );
        // env when no flag
        assert_eq!(
            resolve_base_url(None, Some("http://env/v1")),
            "http://env/v1"
        );
        // default when neither
        assert_eq!(resolve_base_url(None, None), LOCAL_DEFAULT_BASE_URL);
        // blank env also falls through to default
        assert_eq!(resolve_base_url(None, Some("  ")), LOCAL_DEFAULT_BASE_URL);
    }

    #[test]
    fn local_provider_builds_with_and_without_key() {
        let no_key =
            openai::OpenAiProvider::local("qwen3".into(), LOCAL_DEFAULT_BASE_URL.into(), None)
                .expect("builds without key");
        assert_eq!(no_key.provider(), Provider::Local);
        assert_eq!(no_key.model(), "qwen3");

        let with_key = openai::OpenAiProvider::local(
            "qwen3".into(),
            "http://localhost:1234/v1".into(),
            Some("secret".into()),
        )
        .expect("builds with key");
        assert_eq!(with_key.provider(), Provider::Local);
    }
}
