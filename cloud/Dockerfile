# syntax=docker/dockerfile:1.7
#
# docker build --secret id=github_token,env=GITHUB_TOKEN -t dino-cloud .
#
# dino-sync comes from the dino repo, which is private for now: the token is used for that one
# fetch and is never written into a layer.

FROM rust:1.94-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=secret,id=github_token \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    GIT_CONFIG_COUNT=1 \
    GIT_CONFIG_KEY_0="url.https://x-access-token:$(cat /run/secrets/github_token 2>/dev/null)@github.com/.insteadOf" \
    GIT_CONFIG_VALUE_0="https://github.com/" \
    CARGO_NET_GIT_FETCH_WITH_CLI=true \
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
