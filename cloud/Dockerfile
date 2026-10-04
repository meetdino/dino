# syntax=docker/dockerfile:1.7
#
# From the repository root, since the server builds on crates/dino-sync:
#
#   docker build -f cloud/Dockerfile -t dino-cloud .

FROM rust:1.94-bookworm AS build
WORKDIR /src
COPY Cargo.toml ./
COPY crates/dino-sync crates/dino-sync
COPY cloud cloud
WORKDIR /src/cloud
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/cloud/target \
    cargo build --release --locked -p dino-cloud \
 && cp target/release/dino-cloud /dino-cloud

# glibc, CA certificates and a non-root user, nothing else.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /dino-cloud /usr/local/bin/dino-cloud
ENV DINO_BIND=0.0.0.0:8787 \
    DINO_METRICS_BIND=0.0.0.0:9464 \
    DINO_ENV=production
EXPOSE 8787 9464
USER nonroot
ENTRYPOINT ["/usr/local/bin/dino-cloud"]
