# Image for the image workflow. Mirrors workflows/chat/Dockerfile —
# see that file for the full design notes (build context = workspace
# root, parent workspace must still resolve, etc.).
#
#   docker build -f workflows/image/Dockerfile -t lutin-workflow-image:<version> .

# ---------- ui-builder (Vite/React) ----------
FROM oven/bun:1-debian AS ui-builder

WORKDIR /repo
COPY packages/ ./packages/
COPY workflows/image/ui/ ./workflows/image/ui/

RUN cat > package.json <<'JSON'
{
  "name": "lutin-ui-build",
  "private": true,
  "workspaces": ["packages/*", "workflows/*/ui"]
}
JSON

RUN bun install

WORKDIR /repo/workflows/image/ui
RUN bun run build

RUN tar --create --file=/ui.tar --directory=dist .

# ---------- engine builder ----------
FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev libdbus-1-dev build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY . .

WORKDIR /build/workflows/image
RUN cargo build --release --locked

RUN test -f target/release/image

# ---------- runtime ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        libssl3 libdbus-1-3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /workflow /project /session
COPY --from=builder    /build/workflows/image/target/release/image /workflow/bin
COPY --from=ui-builder /ui.tar                                    /workflow/ui.tar

LABEL lutin.workflow.id="image"
LABEL lutin.workflow.bundle="/workflow/ui.tar"
LABEL lutin.workflow.display_name="Image"
LABEL lutin.workflow.icon="🖼"

RUN useradd --system --uid 1000 --user-group --no-create-home lutin
USER lutin

WORKDIR /workflow
ENTRYPOINT ["/workflow/bin"]
