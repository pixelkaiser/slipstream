FROM rust:1.92-bookworm AS builder

WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/session-sharing-relay /usr/local/bin/session-sharing-relay

EXPOSE 8788
ENTRYPOINT ["session-sharing-relay"]
