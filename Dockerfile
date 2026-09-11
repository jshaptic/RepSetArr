# Build with cargo-chef so a source-only change does not rebuild every crate.
FROM rust:1-slim-bookworm AS chef
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config cmake make perl g++ ca-certificates \
 && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked && strip target/release/repsetarr

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tzdata gosu curl \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/repsetarr /usr/local/bin/repsetarr
COPY config.example.yml /usr/local/share/repsetarr/config.example.yml
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# Stamped by CI so the running image can name itself; declared after the build
# stages so a new version never invalidates the compile cache.
ARG APP_VERSION=dev
ARG GIT_SHA=unknown

ENV APP_VERSION=${APP_VERSION} \
    GIT_SHA=${GIT_SHA} \
    REPSETARR_CONFIG=/config/config.yml \
    PUID=99 \
    PGID=100 \
    UMASK=002 \
    TZ=Etc/UTC \
    LOG_LEVEL=info

EXPOSE 9797
VOLUME ["/config"]

# The port lives in config.yml; override HEALTH_URL if you change it there.
ENV HEALTH_URL=http://127.0.0.1:9797/api/health
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s \
    CMD curl -fsS "$HEALTH_URL" > /dev/null || exit 1

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["repsetarr"]
