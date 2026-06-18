# AGENTS.md

## Project

`route-llm` is a Rust/Axum OpenAI-compatible routing proxy for local and
self-hosted use. SDK clients use one local base URL and one managed client token
while the router forwards requests to one or more OpenAI-compatible upstream
providers stored in SQLite.

Keep the project small, auditable, and provider-neutral. Do not add hosted
server provisioning, cloud-specific defaults, personal hostnames, or
provider-specific bootstrap paths unless the user explicitly asks for them.

## Commands

- Format: `cargo fmt --check`
- Test: `cargo test`
- Lint: `cargo clippy -- -D warnings`
- Package audit: `cargo package --list --allow-dirty`
- Run locally: `cargo run -- serve --bind 127.0.0.1:8080`
- Build release service binary: `cargo build --release --bin api-router`
- Run local container: `docker compose -f docker-compose.local.yml up --build`
- List routing state: `cargo run -- list`
- Web admin UI: `/admin` when `ROUTE_LLM_ADMIN_PASSWORD` is set
- Add provider/base URL:
  `cargo run -- add-upstream --name <provider> --base-url <url>/v1 --priority <n>`
- Add provider key:
  `cargo run -- add-key --upstream <provider> --name <key-name> --api-key <secret> --priority <n>`
- Add public model alias:
  `cargo run -- add-model-alias --public-model <alias> --target-type <capability>`
- Add upstream model capability:
  `cargo run -- add-upstream-model --upstream <provider> --model <real-model> --capability <capability> --priority <n> --max-model-len <tokens>`

## Runtime Scope

- Default server binding must remain local-only: `127.0.0.1:8080`.
- Container examples may bind `0.0.0.0` inside the container, but host port
  publishing must stay loopback-only unless explicitly changed by the user.
- Do not add production server units, cloud tunnels, DNS instructions, managed
  database setup, or deployment automation. This repo should ship local
  production-like runtime artifacts only.
- Public/admin display values must be configurable through environment variables
  or CLI flags. Do not hardcode personal hostnames, local paths, or provider
  names in source, README, tests, assets, or generated metadata.

## Secrets

- Do not commit upstream API keys, local proxy keys, `.env`, SQLite databases,
  WAL/SHM files, logs, backups, or cloud credential files.
- SQLite stores upstream keys in plaintext so the proxy can replay them to the
  upstream service. It also stores admin-issued client token plaintext in
  `client_tokens.api_key` so the admin UI can copy tokens later. Treat
  `data/router.sqlite` and its WAL/SHM files as secret material.
- Client tokens are also stored as SHA-256 hashes for authentication and audit
  fingerprints.
- Audit rows must not store request bodies, response bodies, raw Authorization
  headers, raw API keys, raw user agents, raw query strings, or upstream base
  URLs. Store ids, names, statuses, sizes, timings, date buckets, hashes,
  fingerprints, and numeric token counts instead.
- Responses compatibility state is not audit data. `response_states` may store
  conversation history, assistant outputs, and function-call arguments so
  `previous_response_id` can be replayed for the same client. Treat it as
  sensitive SQLite runtime state.
- Do not record admin UI visits or browser artifact requests such as
  `/favicon.ico` in request audits or admin usage statistics.
- Before publishing or packaging, run `cargo package --list --allow-dirty` and
  confirm it excludes `data/`, `logs/`, `.env`, and build output.

## Implementation Notes

- Preserve OpenAI-compatible `/v1/...` paths and streaming responses.
- `POST /v1/responses` is a compatibility adapter for upstreams that support
  `/v1/chat/completions` but not `/v1/responses`. Preserve `input`,
  `instructions`, function `tools`, `tool_choice`, `previous_response_id`,
  `output`, `output_text`, `usage`, and Responses SSE events. Do not claim
  support for OpenAI-hosted built-in tools such as hosted web search or file
  search unless they are explicitly implemented.
- For streaming chat/completions-style requests, preserve the stream while
  adding `stream_options.include_usage = true` to JSON request bodies so
  compatible upstreams can emit final usage. Parse only SSE `usage` metadata and
  update the audit row after the stream completes.
- Keep public model aliases stable. Clients use aliases like `llm-model` as the
  actual SDK model name. The router resolves that alias to an ordered list of
  concrete upstream models and rewrites request bodies to the chosen upstream
  model.
- Model routing is base URL specific: keep `upstream_models` and
  `upstream_model_capabilities` in sync with each upstream's actual model
  availability.
- `/v1/models` should expose public aliases, not upstream model names.
- `/v1/models` model items should include `max_model_len`. Prefer stored
  upstream-model metadata, and fall back to the router default when no stored
  value exists.
- Retriable upstream failures are `401`, `403`, `429`, and `5xx`.
- Failed upstream keys should be skipped via `disabled_until` rather than
  rechecked on every request.
- Keep audit writes best-effort; audit persistence problems should be logged but
  should not fail an otherwise valid proxy response.
- Admin writes must go through SQLite and take effect on the next request
  without process restart. Do not add in-memory route caches unless they have
  explicit invalidation.
- Keep direct dependencies minimal. Remove unused crate dependencies after
  confirming `cargo test` and `cargo clippy -- -D warnings` still pass.

## Responses Compatibility Guide

Use this section whenever the task touches `POST /v1/responses`, Codex custom
model-provider support, streaming event shape, tools, or `previous_response_id`.

Primary files:

- `src/http_proxy.rs`: request authentication, routing candidate selection,
  upstream retry behavior, audit writes, and the decision to route
  `POST /v1/responses` to the selected upstream `/chat/completions` path.
- `src/responses_compat.rs`: Responses request normalization, Chat
  Completions request construction, JSON response conversion, SSE event
  conversion, function tool-call mapping, and `previous_response_id` replay
  preparation.
- `src/db/schema.rs`, `src/db.rs`, and `src/db/models.rs`: `response_states`
  migration and state read/write helpers.

Data flow:

1. Authenticate the client token exactly as normal proxy requests do.
2. Parse the Responses body and extract the public `model` before routing.
3. Resolve candidates with the existing model routing rules. Do not create a
   separate routing path for Responses requests.
4. Build a Chat Completions payload from Responses `input`, optional
   `instructions`, function `tools`, `tool_choice`, and stream settings.
5. Send the converted request to the selected upstream `/chat/completions`
   endpoint, while preserving retry and disabled-key behavior.
6. Convert upstream JSON or SSE back into Responses-compatible `output`,
   `output_text`, `usage`, and streaming events.
7. Store `response_states` only after a successful converted response so
   `previous_response_id` can replay the same client's prior chat history.

Do not store Responses request or response bodies in `request_audits`.
`response_states` is allowed to store conversation state for
`previous_response_id`, but it is sensitive runtime state and must stay in the
SQLite database only.

Supported Responses compatibility surface:

- Text input and message-array input.
- Optional `instructions` as a system message.
- Function tools and function `tool_choice`.
- Function-call outputs through `input` items of type
  `function_call_output`.
- `stream: true` SSE conversion for text deltas and function-call argument
  deltas.
- `previous_response_id` scoped to the authenticated client.
- `output`, `output_text`, and token `usage`.

Do not imply full native OpenAI Responses parity. The adapter does not implement
OpenAI-hosted built-in tools such as hosted web search or file search unless
explicit code and tests are added for them.

When validating against a real configured endpoint, do not print raw client
tokens or upstream API keys. Read tokens into shell variables and report only
HTTP status, response ids, event names, output snippets, usage fields, and audit
row summaries.

## Model Routing Model

The routing model has separate concepts. Keep them separate when making changes:

- `model_aliases`: public SDK-facing model names. Example: `llm-model`.
- `target_type` / capability: generic model type requested by the user.
  Examples: `llm`, `multimodal`, `image`, `tts`, `stt`, `audio`, `video`,
  `embedding`.
- `model_alias_routes`: ordered alias-to-upstream-model rows. This is the
  default model list for a public alias; lower `priority` means higher order in
  the UI.
- `upstream_models` plus `upstream_model_capabilities`: real model names
  available on a specific provider/base URL and the capabilities each model can
  satisfy.
- `client_model_routes`: optional per-client allowlist that maps a client plus
  public alias to one or more concrete upstream models. All auth tokens under
  the same client share these routes.

Routing behavior:

- If a request model matches an enabled public alias, resolve it to a capability
  and first use enabled `client_model_routes` for that client and alias when at
  least one route row exists.
- If no client-specific route exists, use enabled `model_alias_routes` for that
  alias in UI order.
- If an alias has route rows but all are disabled or unhealthy, do not fall back
  to generic capability routing.
- If an alias has no route rows, fall back to enabled upstream models that
  support that capability for backward compatibility.
- If a public alias exists but is disabled, do not pass it through as an unknown
  model. It should produce no healthy candidates.
- If the request model is a registered real upstream model, route only to
  providers where that exact model is enabled.
- If the request model is unknown and not a public alias, keep pass-through
  behavior for backward compatibility.
- Global candidate order is upstream priority, upstream model priority,
  upstream key priority, then stable row ids.
- Alias route candidate order is alias route priority, upstream key priority,
  then stable row ids.
- Per-client route candidate order is route priority, upstream key priority,
  then stable row ids.
- A single upstream model may support multiple capabilities.

## Web Admin UI

- Keep `/admin` password-protected with `ROUTE_LLM_ADMIN_PASSWORD`.
- Use the UI for day-to-day changes: provider/base URL, upstream keys, upstream
  models, public aliases, client creation, multiple client-token generation, and
  client route access.
- Treat aliases as the model-name enum shown to SDK clients.
- The normal admin UI should not expose a separate alias category/capability
  picker. Alias names such as `llm-model`, `multimodal-model`, `tts-model`,
  `stt-model`, and `video-model` should infer the internal `target_type`;
  expose capability controls only for an explicitly requested advanced flow.
- Keep the web UI client-first. The main page should have a client list on one
  side and the selected client's alias routing editor on the other side.
  Provider, alias, and upstream-key setup belongs in a separate settings section
  below the client routing workspace.
- Keep the authenticated admin UI visually aligned with the login page: dark
  navy brand topbar, white working panels, restrained blue accents, 8px radii,
  and no decorative gradient/orb backgrounds.
- Keep favicon, manifest, apple-touch-icon, and Open Graph routes public and
  outside the proxy fallback so browser and link-preview asset requests are not
  written as model request audits.
- Keep client token management in one place: the client list card opens a token
  modal where tokens can be issued, copied, or deleted.
- The statistics section should show recent request duration and token usage
  when available. Token detail cards should show cumulative duration, not
  average duration.
- Admin UI delete actions are hard deletes. Do not implement admin deletion as
  `enabled = false`, and do not show restore controls in the normal UI.
- Token access routes and alias routes must validate that the selected upstream
  model supports the alias target capability before writing route rows.

## Adding Providers Or Models

When adding a provider/base URL:

1. Determine provider name, OpenAI-compatible base URL, one or more API keys,
   upstream priority, supported real model names, model capabilities, and
   per-model priority.
2. Do not expose raw API keys in responses, logs, docs, tests, or commit
   messages.
3. Insert the provider with `add-upstream`.
4. Insert each provider token with `add-key`.
5. Insert each real model with `add-upstream-model`, repeating `--capability`
   for every supported type. Add `--max-model-len <tokens>` when known.
6. Run `cargo run -- list` and confirm provider `models` and `keys` shape.
7. Run a small real or mock request through the public alias when safe.

When adding a new public model name:

1. Determine whether it is a new capability or an alias for an existing
   capability.
2. Add it with
   `add-model-alias --public-model <alias> --target-type <capability>`.
3. Ensure at least one enabled upstream model supports that capability.
4. Verify `GET /v1/models` exposes the alias and does not expose raw upstream
   model names.

## Test Expectations

Before finishing routing, admin, packaging, or documentation changes, run:

- `cargo fmt --check`
- `cargo test`
- `cargo clippy -- -D warnings`
- `cargo package --list --allow-dirty`

Routing tests should cover:

- public alias to capability resolution
- provider/base URL specific model availability
- models with multiple capabilities
- disabled aliases, disabled models, disabled keys, and cached failed keys
- client-specific alias route allowlists and disabled route fallback blocking
- generated client token authentication with multiple tokens per client
- exact registered model routing
- unknown model pass-through behavior
- request body model rewrite edge cases
- streaming `stream_options.include_usage` injection and SSE usage parsing
- `/v1/responses` JSON conversion, SSE event conversion, function tool calls,
  and `previous_response_id` state replay
- `/v1/models` returning public aliases only
- `/v1/models` returning `max_model_len` for every public alias
- audit rows recording public model names and date buckets
