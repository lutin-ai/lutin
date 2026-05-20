# Image for the reviewed workflow. One container per session: CP runs
# `docker run --rm` per StartSession with the per-project volume mounted
# at /project, the session state dir bind-mounted at /session, and the
# env handoff documented in `src/engine.rs::Env`. The container exits
# when the session ends; the volume outlives it.
#
# Build context: workspace root (the crate has path deps into
# ../../crates/lutin-*).
#   docker build -f workflows/reviewed/Dockerfile -t lutin-workflow-reviewed:<version> .
#
# CP extracts the plugin bundle tarball (HTML/JS UI for the Tauri
# chrome) on demand via `docker run --rm <image> cat <path>`, where
# `<path>` is declared by the `lutin.workflow.bundle` label.

# ---------- ui-builder (Vite/React) ----------
FROM oven/bun:1-debian AS ui-builder

WORKDIR /repo
COPY packages/ ./packages/
COPY workflows/reviewed/ui/ ./workflows/reviewed/ui/

RUN cat > package.json <<'JSON'
{
  "name": "lutin-ui-build",
  "private": true,
  "workspaces": ["packages/*", "workflows/*/ui"]
}
JSON

RUN bun install

WORKDIR /repo/workflows/reviewed/ui
RUN bun run build

RUN tar --create --file=/ui.tar --directory=dist .

# ---------- engine builder ----------
FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev libdbus-1-dev build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY . .

WORKDIR /build/workflows/reviewed
RUN cargo build --release --locked

RUN test -f target/release/reviewed

# ---------- runtime ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        libssl3 libdbus-1-3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN apt-get update && apt-get install -y --no-install-recommends \
        nodejs npm \
        python3 python3-pip python3-venv \
        git curl build-essential pkg-config \
        libssl-dev libdbus-1-dev \
        ripgrep jq \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile minimal --no-modify-path \
    && chmod -R a+rwX "$RUSTUP_HOME" "$CARGO_HOME"

RUN mkdir -p /workflow /project /session
COPY --from=builder    /build/workflows/reviewed/target/release/reviewed /workflow/bin
COPY --from=ui-builder /ui.tar                                  /workflow/ui.tar

LABEL lutin.workflow.id="reviewed"
LABEL lutin.workflow.bundle="/workflow/ui.tar"
LABEL lutin.workflow.display_name="Reviewed"
LABEL lutin.workflow.icon="🧐"

RUN useradd --system --uid 1000 --user-group --no-create-home lutin
USER lutin

WORKDIR /workflow
ENTRYPOINT ["/workflow/bin"]
