# Image for the principled workflow. One container per session: CP runs
# `docker run --rm` per StartSession with the per-project volume mounted
# at /project, the session state dir bind-mounted at /session, and the
# env handoff documented in `src/engine.rs::Env`. The container exits
# when the session ends; the volume outlives it.
#
# Build context: workspace root (the crate has path deps into
# ../../crates/lutin-*).
#   docker build -f workflows/principled/Dockerfile -t lutin-workflow-principled:<version> .
#
# CP extracts the plugin bundle tarball (HTML/JS UI for the Tauri
# chrome) on demand via `docker run --rm <image> cat <path>`, where
# `<path>` is declared by the `lutin.workflow.bundle` label.

# ---------- ui-builder (Vite/React) ----------
# Built first so a Rust source change doesn't bust the JS layer's cache.
# `oven/bun` ships bun + a recent Node toolchain; we only need bun to
# install + `bun run build`.
FROM oven/bun:1-debian AS ui-builder

# The UI is a member of the repo-root Bun workspace and depends on
# `@lutin/chat-widgets` from `packages/chat-widgets/`. We copy only
# what the UI build needs (no `lutin-desktop/` Tauri tree) and write
# a slim workspace root inline so bun resolves the workspace symlink
# without trying to install the desktop's deps.
WORKDIR /repo
COPY packages/ ./packages/
COPY workflows/principled/ui/ ./workflows/principled/ui/

RUN cat > package.json <<'JSON'
{
  "name": "lutin-ui-build",
  "private": true,
  "workspaces": ["packages/*", "workflows/*/ui"]
}
JSON

RUN bun install

WORKDIR /repo/workflows/principled/ui
RUN bun run build

# Tar the dist directory at the bundle root so chrome's BundleCache
# can `tar::Archive::new(...)` it directly. `--directory=dist` keeps
# `lutin.workflow.json` and `index.html` at archive root rather than
# nested under `dist/`.
RUN tar --create --file=/ui.tar --directory=dist .

# ---------- engine builder ----------
FROM rust:1-bookworm AS builder

# reqwest's default features pull in native-tls → OpenSSL. pkg-config +
# libssl-dev satisfy the openssl-sys build script. build-essential is
# for the cc invocations on transitive C deps (ring, etc.).
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev libdbus-1-dev build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# The principled crate is its own cargo workspace (`[workspace]` empty in its
# Cargo.toml) — its build doesn't compile the parent workspace. But the
# SDK crates under `crates/` inherit deps from the parent root manifest
# via `workspace = true`, so the *parent* workspace must still resolve.
# That means every crate listed in the parent's `members` must be on
# disk, even though only principled + its transitive deps actually compile.
# Copy the whole context (build artefacts excluded by .dockerignore);
# the cost is a few MB of source and one fast-path COPY layer.
COPY . .

WORKDIR /build/workflows/principled
RUN cargo build --release --locked

# Sanity: the engine binary must exist before we copy it to the
# runtime stage. Failing here is louder than a confusing "file not
# found" later.
RUN test -f target/release/principled

# ---------- runtime ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        libssl3 libdbus-1-3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Dev toolchains for the agent to use inside /project. Kept in its own
# layer (and ordered before the engine COPY) so a code change in the
# builder stage doesn't bust this ~hundreds-of-MB apt cache. Versions
# are whatever bookworm ships — we trade bleeding-edge for cache hits
# and reproducibility.
RUN apt-get update && apt-get install -y --no-install-recommends \
        nodejs npm \
        python3 python3-pip python3-venv \
        git curl build-essential pkg-config \
        ripgrep jq \
    && rm -rf /var/lib/apt/lists/*

# Bookworm's `rustc cargo` is ~1.63 and can't read Cargo.lock v4 (needs
# 1.78+). Install a current stable toolchain via rustup so the agent
# can build crates that target newer lock formats. Lives under
# /usr/local so any uid the container runs as can read it.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile minimal --no-modify-path \
    && chmod -R a+rwX "$RUSTUP_HOME" "$CARGO_HOME"

# Fixed image-side layout. CP reads each artefact path from the label
# below, so changing them here is a one-line change.
RUN mkdir -p /workflow /project /session
COPY --from=builder    /build/workflows/principled/target/release/principled /workflow/bin
COPY --from=ui-builder /ui.tar                                   /workflow/ui.tar

LABEL lutin.workflow.id="principled"
LABEL lutin.workflow.bundle="/workflow/ui.tar"
LABEL lutin.workflow.display_name="Principled"
LABEL lutin.workflow.icon="🛡️"

# Default to a non-privileged uid; CP overrides with `--user $(id -u):$(id -g)`
# at run-time so files written into the bind-mounted /project volume are
# owned by the host user, matching the lutin-project image's pattern.
RUN useradd --system --uid 1000 --user-group --no-create-home lutin
USER lutin

WORKDIR /workflow
ENTRYPOINT ["/workflow/bin"]
