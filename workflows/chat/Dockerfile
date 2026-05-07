# Image for the chat workflow. One container per session: CP runs
# `docker run --rm` per StartSession with the per-project volume mounted
# at /project, the session state dir bind-mounted at /session, and the
# env handoff documented in `src/engine.rs::Env`. The container exits
# when the session ends; the volume outlives it.
#
# Build context: workspace root (the crate has path deps into
# ../../crates/lutin-*).
#   docker build -f workflows/chat/Dockerfile -t lutin-workflow-chat:<version> .
#
# CP extracts artefacts on demand via `docker run --rm <image> cat <path>`:
#   - cdylib UI (legacy egui, removed in Phase 4) at the path declared by
#     the `lutin.workflow.cdylib` label.
#   - Plugin bundle tarball (HTML/JS UI for the Tauri chrome) at the path
#     declared by the `lutin.workflow.bundle` label.

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
COPY workflows/chat/ui/ ./workflows/chat/ui/

RUN cat > package.json <<'JSON'
{
  "name": "lutin-ui-build",
  "private": true,
  "workspaces": ["packages/*", "workflows/*/ui"]
}
JSON

RUN bun install

WORKDIR /repo/workflows/chat/ui
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

# The chat crate is its own cargo workspace (`[workspace]` empty in its
# Cargo.toml) — its build doesn't compile the parent workspace. But the
# SDK crates under `crates/` inherit deps from the parent root manifest
# via `workspace = true`, so the *parent* workspace must still resolve.
# That means every crate listed in the parent's `members` must be on
# disk, even though only chat + its transitive deps actually compile.
# Copy the whole context (build artefacts excluded by .dockerignore);
# the cost is a few MB of source and one fast-path COPY layer.
COPY . .

WORKDIR /build/workflows/chat
RUN cargo build --release --locked

# Sanity: both artifacts must exist before we copy them to the runtime
# stage. Failing here is louder than a confusing "file not found" later.
RUN test -f target/release/chat \
 && test -f target/release/libchat.so

# ---------- runtime ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        libssl3 libdbus-1-3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Fixed image-side layout. CP reads each artefact path from the label
# below, so changing them here is a one-line change.
RUN mkdir -p /workflow /project /session
COPY --from=builder    /build/workflows/chat/target/release/chat        /workflow/bin
COPY --from=builder    /build/workflows/chat/target/release/libchat.so  /workflow/lib.so
COPY --from=ui-builder /ui.tar                                          /workflow/ui.tar

LABEL lutin.workflow.id="chat"
LABEL lutin.workflow.cdylib="/workflow/lib.so"
LABEL lutin.workflow.bundle="/workflow/ui.tar"
LABEL lutin.workflow.display_name="Chat"
LABEL lutin.workflow.icon="💬"

# Default to a non-privileged uid; CP overrides with `--user $(id -u):$(id -g)`
# at run-time so files written into the bind-mounted /project volume are
# owned by the host user, matching the lutin-project image's pattern.
RUN useradd --system --uid 1000 --user-group --no-create-home lutin
USER lutin

WORKDIR /workflow
ENTRYPOINT ["/workflow/bin"]
