use gantry::cli::{parse_model_slug, ConfigError, Provider};
use gantry::provider::build_adapter;
use std::sync::{LazyLock, Mutex, MutexGuard};

#[test]
fn build_adapter_anthropic_requires_api_key() {
    let _env = EnvVarGuard::set("ANTHROPIC_API_KEY", None);
    let result = build_adapter(Provider::Anthropic, "claude-sonnet-4".into(), None);
    assert!(result.is_err());
    assert!(result
        .err()
        .expect("error")
        .to_string()
        .contains("ANTHROPIC_API_KEY not set"));
}

#[test]
fn build_adapter_openai_requires_api_key() {
    let _env = EnvVarGuard::set("OPENAI_API_KEY", None);
    let result = build_adapter(Provider::OpenAi, "gpt-4o".into(), None);
    assert!(result.is_err());
    assert!(result
        .err()
        .expect("error")
        .to_string()
        .contains("OPENAI_API_KEY not set"));
}

#[test]
fn build_adapter_gemini_requires_api_key() {
    let _env = EnvVarGuard::set("GEMINI_API_KEY", None);
    let result = build_adapter(Provider::Gemini, "gemini-2.0-flash".into(), None);
    assert!(result.is_err());
    assert!(result
        .err()
        .expect("error")
        .to_string()
        .contains("GEMINI_API_KEY not set"));
}

#[test]
fn build_adapter_local_needs_no_api_key() {
    // The local provider builds with no key env set; the --base-url flag (here
    // None → default) is the only local-specific input.
    let _env = EnvVarGuard::set("GANTRY_LOCAL_API_KEY", None);
    let adapter = build_adapter(Provider::Local, "qwen3-coder-next".into(), None)
        .expect("local adapter builds without an API key");
    assert_eq!(adapter.provider(), Provider::Local);
    assert_eq!(adapter.model(), "qwen3-coder-next");
}

/// Process-wide lock serializing env-mutating tests in this binary; env vars are
/// global so the default multi-threaded runner otherwise races provider API-key
/// vars between the `build_adapter_*_requires_api_key` tests. Held for the
/// guard's lifetime.
static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

struct EnvVarGuard {
    key: String,
    previous: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvVarGuard {
    fn set(key: &str, value: Option<&str>) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let previous = std::env::var(key).ok();
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        Self {
            key: key.to_string(),
            previous,
            _lock: lock,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(&self.key, value),
            None => std::env::remove_var(&self.key),
        }
    }
}

#[test]
fn provider_gemini_as_str_is_google() {
    // G2: Provider::Gemini.as_str() must return "google" after the slug rename.
    assert_eq!(Provider::Gemini.as_str(), "google");
}

#[test]
fn google_slug_routes_to_gemini_adapter() {
    // G2: `google/<model>` must dispatch to the Gemini adapter.
    let (provider, model) = parse_model_slug("google/gemini-2.5-pro").expect("google slug parses");
    assert_eq!(provider, Provider::Gemini);
    assert_eq!(model, "gemini-2.5-pro");
}

#[test]
fn gemini_slug_is_now_unknown_provider() {
    // G2: `gemini/` is no longer a valid slug prefix → config error.
    assert_eq!(
        parse_model_slug("gemini/gemini-2.0-flash"),
        Err(ConfigError::UnknownProvider {
            provider: "gemini".into(),
        })
    );
    assert_eq!(
        parse_model_slug("gemini/gemini-1.5-pro"),
        Err(ConfigError::UnknownProvider {
            provider: "gemini".into(),
        })
    );
}

#[test]
fn parse_slug_routes_provider_table() {
    let cases = [
        ("anthropic/claude-3-5-sonnet-latest", Provider::Anthropic),
        ("anthropic/claude-haiku-4-5-20251001", Provider::Anthropic),
        ("openai/gpt-4o", Provider::OpenAi),
        ("openai/gpt-4o-mini", Provider::OpenAi),
        ("openai/o1", Provider::OpenAi),
        ("openai/o3-mini", Provider::OpenAi),
        ("google/gemini-2.0-flash", Provider::Gemini),
        ("google/gemini-1.5-pro", Provider::Gemini),
        ("local/qwen3-coder-next", Provider::Local),
        ("local/llama-3.3-70b", Provider::Local),
    ];

    for (slug, expected) in cases {
        assert_eq!(
            parse_model_slug(slug).expect("slug parses").0,
            expected,
            "slug {slug}"
        );
    }
}

#[test]
fn parse_slug_rejects_unroutable_models() {
    // Bare names without a provider prefix no longer route by guesswork.
    for slug in ["o", "model-x", "gpt", "claude"] {
        assert_eq!(
            parse_model_slug(slug),
            Err(ConfigError::MissingProviderPrefix { model: slug.into() }),
            "slug {slug}"
        );
    }

    // A slug with an unrecognized provider segment is a config error.
    assert_eq!(
        parse_model_slug("nope/some-model"),
        Err(ConfigError::UnknownProvider {
            provider: "nope".into(),
        })
    );
}
