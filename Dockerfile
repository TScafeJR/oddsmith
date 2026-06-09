FROM rust:1.97-slim-bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY crates/ crates/
RUN cargo build --release --locked -p oddsmith-cli --features parquet

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /src/target/release/oddsmith /usr/local/bin/oddsmith
COPY strategies/ strategies/
EXPOSE 8080
ENTRYPOINT ["oddsmith", "serve", "--port", "8080"]
