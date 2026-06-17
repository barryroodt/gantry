# ADR-0014: Generic OpenAI-compatible `local` provider (self-hosted models, oMLX)

**Status:** Accepted
**Date:** 2026-06-15

## Context

Users want to drive gantry with a self-hosted model — the prompting case being
[oMLX](https://omlx.ai/), a native macOS MLX inference server with SSD KV
caching that exposes **both** an OpenAI-compatible (`/v1/chat/completions`) and
an Anthropic-compatible (`/v1/messages`) API on localhost. Other local servers
(Ollama, vLLM, LM Studio, llama.cpp) expose the same OpenAI-compatible surface.

Before this change, gantry's `openai` and `anthropic` adapters already supported
a base-URL override (`OPENAI_BASE_URL`, `ANTHROPIC_API_BASE`), but both
**hard-require an API key** and report a hosted provider label — so pointing
`openai` at a keyless local server was awkward and dishonest in `start`/`result`
events and metering.

gantry core meters **tokens only** (no cost/pricing — that lives solely in the
`bench` crate), so a local provider needs no cost handling; the token budget
applies unchanged.

## Decision

Add a first-class **generic `local` provider** (slug `local/<model>`) that
speaks the **OpenAI-compatible** protocol, implemented as a **config-driven
shared OpenAI engine**: one `OpenAiProvider` request/response code path with two
constructors — `openai()` (hosted, requires `OPENAI_API_KEY`) and `local()`
(resolved base URL, optional key, `Provider::Local` label).

- **Endpoint:** `--base-url` flag → `GANTRY_LOCAL_BASE_URL` → default
  `http://localhost:8000/v1` (oMLX's default; `/v1` included because rig's
  OpenAI client appends `/chat/completions`). `--base-url` is rejected for
  non-local providers (they keep their own env overrides).
- **Auth:** optional `GANTRY_LOCAL_API_KEY`; a placeholder bearer is sent when
  absent (local servers ignore it when auth is off).
- **Errors:** local connection failures (after retries) are wrapped with a hint
  naming the endpoint ("could not reach the local server at `<url>` — is it
  running?").

oMLX is the headline consumer, but the provider is server-agnostic.

## Options considered

- **A — Generic OpenAI-compatible `local` provider (chosen).** Reusable across
  all OpenAI-compatible local servers; honest provider label; one shared engine.
- **B — oMLX-specific `omlx` provider.** Tightest fit but narrow; a second local
  server later means another provider. Rejected — no benefit over A.
- **C — Just relax the API-key requirement on the `openai` adapter.** Minimal,
  but the provider still reports as `openai` in events/metering and there's no
  clean `--base-url`/`local` UX. Rejected as dishonest and less discoverable.
- **Anthropic-compatible path (rejected).** oMLX also speaks `/v1/messages`, but
  the `anthropic` adapter injects `anthropic-version` + `anthropic-beta:
  prompt-caching` headers and `cache_control` blocks a local server may reject;
  OpenAI-compat is the cleaner, more universal surface (oMLX does its own SSD
  caching regardless).

## Consequences

- One OpenAI-compatible engine backs both `openai` and `local`; the existing
  wiremock OpenAI tests guard the shared path, plus a keyless `local` test.
- No bench-crate integration and no `/v1/models` auto-discovery (model is named
  explicitly in the slug) — possible follow-ups, deliberately out of scope.
- `--model local/<id>` joins `anthropic|openai|gemini` in slug routing and the
  `UnknownProvider` error text.
