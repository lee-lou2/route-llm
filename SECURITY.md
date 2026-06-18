# Security Policy

## Supported Versions

Security fixes are handled on the `main` branch until the project publishes
versioned releases.

## Reporting a Vulnerability

Please do not open a public issue for a suspected secret leak, authentication
bypass, request body exposure, or routing vulnerability. Report it privately to
the project maintainer listed by the repository host.

Include:

- affected commit or version
- reproduction steps
- expected and observed behavior
- whether any API keys, client tokens, SQLite files, logs, or audit rows may
  have been exposed

## Secret Handling Expectations

`route-llm` stores upstream API keys and recoverable admin-issued client tokens
in SQLite so the proxy and admin UI can use them later. Treat the configured
SQLite database, WAL files, `.env` files, logs, and backups as private runtime
state. They must not be committed or attached to public issues.

Request audit rows are sanitized and must not contain prompts, responses, raw
authorization headers, raw API keys, raw user agents, raw query strings, or
upstream base URLs. Attempt audits may store upstream status, content type,
response byte count, response hash, and response kind, but not raw response
bodies or body prefixes.

Responses compatibility state is different from audit data. When
`/v1/responses` uses `previous_response_id`, the `response_states` table stores
the chat history needed to continue that response. That state may include user
input, assistant output, and function-call arguments, so treat it as sensitive
runtime data.
