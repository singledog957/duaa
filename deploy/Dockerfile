FROM rust:1.88-slim AS build
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
ARG CARGO_MIRROR=""
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN if [ -n "$CARGO_MIRROR" ]; then \
      mkdir -p /usr/local/cargo && \
      printf '[source.crates-io]\nreplace-with = "mirror"\n[source.mirror]\nregistry = "%s"\n' "$CARGO_MIRROR" > /usr/local/cargo/config.toml; \
    fi && \
    cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/duaa /usr/local/bin/duaa
ENTRYPOINT ["/usr/local/bin/duaa"]
