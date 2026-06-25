# route-llm Architecture

This project is intentionally small: one Axum server, SQLite-backed routing
state, and an OpenAI-compatible proxy surface. Keep new code provider-neutral,
local-first, and close to the module that owns the behavior.

## Runtime Entry Points

- `src/main.rs`: CLI entrypoint and command dispatch.
- `src/cli.rs`: clap argument definitions and defaults.
- `src/server.rs`: app state, request client, admin config, runtime cleanup,
  router wiring, and graceful shutdown.

## Proxy

- `src/http_proxy/mod.rs`: proxy request lifecycle, client authentication,
  model candidate selection, upstream retry loop, and Responses routing bridge.
- `src/http_proxy/request_body.rs`: request model extraction, model rewrite, and
  `stream_options.include_usage` injection.
- `src/http_proxy/stream.rs`: streaming usage capture for chat-style SSE
  responses.
- `src/http_proxy/audit.rs`: safe request/attempt audit construction and token
  usage enrichment.
- `src/http_proxy/errors.rs`: upstream failure responses and safe upstream
  response diagnostics.
- `src/http_proxy/models_catalog.rs`: public `/v1/models` and Codex catalog
  response shapes.
- `src/http_proxy/support.rs`: path, header, retry, and sanitization helpers.

## Responses Compatibility

- `src/responses_compat/mod.rs`: Responses request parsing and Chat
  Completions payload construction.
- `src/responses_compat/input.rs`: Responses `input` item normalization into
  chat messages.
- `src/responses_compat/tools.rs`: function/custom/namespace tool mapping.
- `src/responses_compat/json_response.rs`: upstream JSON response conversion
  and `response_states` persistence.
- `src/responses_compat/stream.rs`: upstream Chat Completions SSE conversion
  into Responses SSE events.
- `src/responses_compat/usage.rs`: token usage extraction.

## SQLite

- `src/db/schema.rs`: SQLite connection setup, migrations, indexes, and seed
  aliases.
- `src/db/models.rs`: shared data structs and constants.
- `src/db/routing.rs`: public alias, client route, exact model, and capability
  candidate selection.
- `src/db/admin_mutations.rs`: client/provider/model/alias creation and update
  operations.
- `src/db/admin_operations.rs`: key health, reordering, deletion, discovered
  model refresh, and related admin operations.
- `src/db/audit.rs`: request audit and upstream attempt audit writes.
- `src/db/admin_stats.rs`: admin dashboard statistics and health summaries.
- `src/db/state.rs`: admin state summaries used by CLI and UI.
- `src/db/response_state.rs`: Responses `previous_response_id` state read/write.
- `src/db/runtime_cleanup.rs`: retention cleanup for audits and response state.

## Admin UI

- `src/admin_ui/mod.rs`: admin router wiring and render context.
- `src/admin_ui/actions.rs`: route handlers and form actions.
- `src/admin_ui/auth.rs`: admin session checks, redirects, and display URL
  helpers.
- `src/admin_ui/render.rs`: dashboard, client, provider, model, and route
  rendering.
- `src/admin_ui/stats_render.rs`: request, token, key, and health statistics
  rendering.
- `src/admin_ui/components.rs`: small shared HTML controls and badges.
- `src/admin_ui/forms.rs`, `page.rs`, `scripts.rs`, `styles.rs`, `text.rs`:
  form decoding, page shell, embedded asset helpers, and HTML/query escaping.

## Tests

Unit tests live beside each module in `tests.rs` files so private routing,
Responses, proxy, DB, and UI behavior stays directly testable. Before finishing
changes that touch routing, admin, packaging, or documentation, run:

```bash
cargo fmt --check
cargo test
cargo clippy -- -D warnings
cargo package --list --allow-dirty
```
