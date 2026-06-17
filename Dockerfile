FROM rust:1-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY assets ./assets
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/route-llm /usr/local/bin/route-llm

ENV ROUTE_LLM_BIND=0.0.0.0:8080
ENV ROUTE_LLM_DATABASE_URL=sqlite:///data/router.sqlite

VOLUME ["/data"]
EXPOSE 8080

CMD ["route-llm", "serve"]
