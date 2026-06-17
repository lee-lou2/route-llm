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
