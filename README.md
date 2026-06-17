# route-llm

`route-llm` is a small OpenAI-compatible routing proxy for local and
self-hosted use. SDK clients talk to one local base URL and one managed client
token while the router forwards requests to one or more OpenAI-compatible
upstream providers stored in SQLite.

The project is intentionally narrower than a full LLM gateway. It preserves
OpenAI-style request and response bodies, keeps routing state in SQLite, retries
healthy upstream keys in priority order, and records sanitized operational audit
metadata without storing prompts or responses.

## Features

- OpenAI-compatible `/v1/...` proxying for chat, responses, completions,
  embeddings, image, audio, and other pass-through paths.
- Public model aliases such as `llm-model` that map to real upstream model ids.
- Per-client routing allowlists for restricting a client to specific upstream
  models.
- Multiple local client tokens per client.
- Multiple upstream providers and API keys, ordered by priority.
- Retry cache for upstream `401`, `403`, `429`, and `5xx` failures using
  `disabled_until`.
- SQLite-backed admin UI for local day-to-day setup.
- Sanitized request audits with status, duration, route, model, token usage, and
  upstream attempt metadata.
- Streaming response proxying with optional SSE usage extraction when upstreams
  emit final `usage` chunks.

## Dashboard Preview

![route-llm admin dashboard preview](docs/dashboard.png)

The screenshot uses temporary demo clients, upstreams, model routes, and
sanitized request audit rows. It does not include real provider keys or prompts.

## Local Quick Start

Install a current stable Rust toolchain, then copy the example environment:

```bash
cp .env.example .env
```

Set local bootstrap values in your shell. Use your own OpenAI-compatible
provider URL and model id:

```bash
export ROUTE_LLM_CLIENT_TOKEN="$(openssl rand -hex 24)"
export UPSTREAM_NAME="openai-compatible"
export UPSTREAM_BASE_URL="https://api.example.com/v1"
export UPSTREAM_API_KEY="replace-with-your-provider-key"
export UPSTREAM_MODEL="provider-llm"
```

Initialize SQLite:

```bash
cargo run -- add-client --name local --api-key "$ROUTE_LLM_CLIENT_TOKEN"
cargo run -- add-upstream --name "$UPSTREAM_NAME" --base-url "$UPSTREAM_BASE_URL" --priority 10
cargo run -- add-key --upstream "$UPSTREAM_NAME" --name primary --api-key "$UPSTREAM_API_KEY" --priority 10
cargo run -- add-model-alias --public-model llm-model --target-type llm
cargo run -- add-upstream-model --upstream "$UPSTREAM_NAME" --model "$UPSTREAM_MODEL" --capability llm --priority 10
```

Start the local server:

```bash
cargo run -- serve --bind 127.0.0.1:8080
```

Point an OpenAI-compatible SDK at the router:

```python
from openai import OpenAI

client = OpenAI(
    api_key="the-value-of-ROUTE_LLM_CLIENT_TOKEN",
    base_url="http://127.0.0.1:8080/v1",
)

response = client.chat.completions.create(
    model="llm-model",
    messages=[{"role": "user", "content": "ping"}],
)
print(response.choices[0].message.content)
```

`GET /v1/models` returns public aliases only. Each model item includes
`max_model_len`; the router uses stored upstream-model metadata when available
and falls back to the built-in default.

## Admin UI

Set an admin password to enable `/admin`:

```bash
export ROUTE_LLM_ADMIN_PASSWORD="$(openssl rand -hex 24)"
cargo run -- serve
```

The admin UI can create clients, issue client tokens, add providers, add
provider API keys, register provider models, connect models to public aliases,
set per-client routes, inspect recent sanitized audits, and clear upstream key
failure cache.

Display metadata is configurable and has no project-specific default:

```bash
ROUTE_LLM_ADMIN_SITE_NAME="Route LLM"
ROUTE_LLM_ADMIN_SITE_DESCRIPTION="Local OpenAI-compatible routing proxy"
ROUTE_LLM_PUBLIC_BASE_URL="https://router.example.com"
```

`ROUTE_LLM_PUBLIC_BASE_URL` is optional. It is used only for rendered admin
metadata and display text; it does not expose or bind the server.

## Local Container Run

The included container setup is for local production-like runs, not server
provisioning:

```bash
docker compose -f docker-compose.local.yml up --build
```

The compose file binds the service to `127.0.0.1:8080` on the host and stores
SQLite data in a Docker volume. Bootstrap the volume with commands such as:

```bash
docker compose -f docker-compose.local.yml run --rm route-llm \
  route-llm --database-url sqlite:///data/router.sqlite add-client \
  --name local --api-key "$ROUTE_LLM_CLIENT_TOKEN"
```

Repeat the same `add-upstream`, `add-key`, `add-model-alias`, and
`add-upstream-model` commands from the local quick start, using
`sqlite:///data/router.sqlite` inside the container.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `ROUTE_LLM_DATABASE_URL` | `sqlite://data/router.sqlite` | SQLite database location |
| `ROUTE_LLM_BIND` | `127.0.0.1:8080` | server bind address |
| `ROUTE_LLM_PUBLIC_PREFIX` | `/v1` | SDK-facing OpenAI-compatible path prefix |
| `ROUTE_LLM_REQUEST_TIMEOUT_SECS` | `300` | upstream request timeout |
| `ROUTE_LLM_TRANSIENT_FAILURE_TTL_SECS` | `300` | retry cache TTL for transient upstream failures |
| `ROUTE_LLM_AUTH_FAILURE_TTL_SECS` | `3600` | retry cache TTL for upstream auth failures |
| `ROUTE_LLM_MAX_BODY_BYTES` | `33554432` | maximum request body accepted by the proxy |
| `ROUTE_LLM_ADMIN_PASSWORD` | unset | enables `/admin` when set |
| `ROUTE_LLM_ADMIN_SESSION_SECRET` | derived from password | optional stable cookie-signing secret |
| `ROUTE_LLM_ADMIN_SITE_NAME` | `Route LLM` | admin UI display name |
| `ROUTE_LLM_ADMIN_SITE_DESCRIPTION` | `Local OpenAI-compatible routing proxy` | admin page metadata |
| `ROUTE_LLM_PUBLIC_BASE_URL` | unset | optional display and Open Graph base URL |

## Routing Model

The router keeps three concepts separate:

- `model_aliases`: public SDK-facing model names such as `llm-model`.
- `target_type`: generic capability requested by an alias, such as `llm`,
  `multimodal`, `image`, `tts`, `stt`, `audio`, `video`, or `embedding`.
- `upstream_models`: real model ids available on a specific provider/base URL.

If a request model matches an enabled alias, the router first checks
client-specific routes for that client and alias. If none exist, it checks
default alias routes. If no default alias route exists, it falls back to enabled
upstream models that support the alias capability. If the requested model is a
registered real upstream model, the router routes only to providers where that
exact model is enabled. Unknown models keep backward-compatible pass-through
behavior.

## SQLite And Secrets

SQLite stores upstream API keys in plaintext because the proxy must replay them
to upstream providers. It also stores admin-issued client token plaintext in
`client_tokens.api_key` so the admin UI can copy newly generated tokens later,
alongside SHA-256 hashes for authentication and audit fingerprints.

Treat these files as secret runtime state:

- `data/router.sqlite`
- `data/router.sqlite-wal`
- `data/router.sqlite-shm`
- `.env`
- logs and backups

They are ignored by git and excluded from Cargo packaging. Do not attach them to
public issues.

Audit rows intentionally do not store request bodies, response bodies, raw
authorization headers, raw API keys, raw user agents, raw query strings, or
upstream base URLs. They store ids, names, statuses, sizes, timings, date
buckets, hashes/fingerprints, and numeric token counts.

## Project Structure

```text
src/main.rs          CLI entrypoint and command dispatch
src/cli.rs           clap argument definitions
src/server.rs        Axum app state, runtime config, and server startup
src/http_proxy.rs    OpenAI-compatible proxy, retry, streaming, and audit glue
src/db.rs            routing queries, admin summaries, audits, and DB mutations
src/db/models.rs     shared data structs and constants
src/db/schema.rs     SQLite connection setup, migrations, indexes, seed aliases
src/admin_ui.rs      admin routes, form handlers, and HTML rendering
src/admin_ui/        admin form, page shell, style/script helpers
src/assets.rs        public favicon, manifest, robots, and Open Graph assets
assets/              embedded admin CSS, JS, and static images
```

The project currently keeps SQL close to the functions that own each behavior.
When adding broad DB behavior, prefer splitting cohesive modules such as
`db/routing.rs`, `db/audit.rs`, or `db/admin_stats.rs` rather than growing
unrelated sections in one place.

## Development

Run the full local gate before publishing changes:

```bash
cargo fmt --check
cargo test
cargo clippy -- -D warnings
cargo package --list --allow-dirty
```

`cargo package --list --allow-dirty` should list source, docs, and assets only.
It must not include SQLite files, logs, `.env`, or build output.

## License

MIT. See [LICENSE](LICENSE).
