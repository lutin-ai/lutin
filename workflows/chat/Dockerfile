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
# CP extracts the cdylib from this image at install time via
# `docker create` + `docker cp <id>:/workflow/lib.so <host_path>`. The
# path is declared via the `lutin.workflow.cdylib` label so CP can find
# it without hard-coding.

# ---------- builder ----------
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

# Fixed image-side layout. CP reads the cdylib path from the label
# below, so changing it here is a one-line change. The bin path is
# implicit in the ENTRYPOINT.
RUN mkdir -p /workflow /project /session
COPY --from=builder /build/workflows/chat/target/release/chat        /workflow/bin
COPY --from=builder /build/workflows/chat/target/release/libchat.so  /workflow/lib.so

LABEL lutin.workflow.id="chat"
LABEL lutin.workflow.cdylib="/workflow/lib.so"

# Default to a non-privileged uid; CP overrides with `--user $(id -u):$(id -g)`
# at run-time so files written into the bind-mounted /project volume are
# owned by the host user, matching the lutin-project image's pattern.
RUN useradd --system --uid 1000 --user-group --no-create-home lutin
USER lutin

WORKDIR /workflow
ENTRYPOINT ["/workflow/bin"]
