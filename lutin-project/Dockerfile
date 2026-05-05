# Image for the per-project supervisor (`lutin-project`). The
# control-panel runs one container of this image per project, with
# the per-project tree (`<projects_root>/<slug>/`) bind-mounted at
# `/project` and the global config dir bind-mounted read-only at
# `/global`. See `lutin-control-panel/src/lib.rs` (`launch_docker`).
#
# Build context: workspace root.
#   docker build -f lutin-project/Dockerfile -t lutin-project:<version> .
#
# Single image (not multi-stage) because the runtime needs the Rust
# toolchain to build user-supplied workflow crates on `StartSession`.
# Splitting builder/runtime would just duplicate the toolchain.
#
# Mirrors the toolchain set from `../lutin/engine/Dockerfile.runtime`:
# workflows (and the agents they host) routinely shell out to Node /
# Python / Go / Rust / Java / Zig / Deno, drive a headless browser,
# call into whisper.cpp / llama.cpp (Vulkan + OpenBLAS), and edit code
# via Helix + language servers. Keeping everything pre-installed
# avoids per-session install latency on cold StartSession.

FROM debian:trixie-slim

# ---------- system deps + browser ----------
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl wget gnupg \
    libvulkan1 libgomp1 libopenblas0 libgfortran5 \
    mesa-vulkan-drivers libdrm2 libdrm-amdgpu1 \
    chromium \
    # build essentials for native modules / compilation
    build-essential pkg-config cmake \
    # llama.cpp / whisper.cpp build deps
    libclang-dev libvulkan-dev glslc \
    libopenblas-dev gfortran \
    libssl-dev libdbus-1-dev \
    git openssh-client openssh-server openssh-sftp-server \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /run/sshd

# ---------- Node.js (LTS) ----------
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/* \
    && corepack enable

# ---------- Python 3 + pip ----------
RUN apt-get update && apt-get install -y --no-install-recommends \
    python3 python3-pip python3-venv \
    && rm -rf /var/lib/apt/lists/*

# ---------- Rust (via rustup, minimal profile) ----------
ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo
ENV PATH="/usr/local/cargo/bin:${PATH}"
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal \
    && rustup component add rustfmt clippy rust-analyzer

# ---------- Go ----------
ENV GOPATH=/usr/local/go-packages
ENV PATH="/usr/local/go/bin:${GOPATH}/bin:${PATH}"
RUN ARCH=$(dpkg --print-architecture) \
    && curl -fsSL "https://go.dev/dl/go1.24.2.linux-${ARCH}.tar.gz" \
    | tar -C /usr/local -xz

# ---------- Java (JDK headless) ----------
RUN apt-get update && apt-get install -y --no-install-recommends \
    default-jdk-headless \
    && rm -rf /var/lib/apt/lists/*

# ---------- common CLI tools ----------
RUN apt-get update && apt-get install -y --no-install-recommends \
    jq ripgrep fd-find unzip sqlite3 xz-utils \
    && rm -rf /var/lib/apt/lists/*

# ---------- Deno ----------
RUN curl -fsSL https://deno.land/install.sh | DENO_INSTALL=/usr/local sh

# ---------- Zig 0.16 ----------
ENV ZIG_VERSION=0.16.0
RUN ARCH=$(uname -m) \
    && curl -fsSL "https://ziglang.org/download/${ZIG_VERSION}/zig-${ARCH}-linux-${ZIG_VERSION}.tar.xz" \
    | tar -C /usr/local -xJ \
    && ln -s "/usr/local/zig-${ARCH}-linux-${ZIG_VERSION}/zig" /usr/local/bin/zig

# ---------- Helix editor ----------
ENV HELIX_VERSION=25.07.1
RUN ARCH_HX=$(uname -m) \
    && curl -fsSL "https://github.com/helix-editor/helix/releases/download/${HELIX_VERSION}/helix-${HELIX_VERSION}-${ARCH_HX}-linux.tar.xz" \
    | tar -C /opt -xJ \
    && mv "/opt/helix-${HELIX_VERSION}-${ARCH_HX}-linux" /opt/helix \
    && ln -s /opt/helix/hx /usr/local/bin/hx
ENV HELIX_RUNTIME=/opt/helix/runtime

# ---------- Language servers ----------
# zls (Zig LS) — match zig version
RUN ARCH_ZLS=$(uname -m) \
    && curl -fsSL "https://github.com/zigtools/zls/releases/download/${ZIG_VERSION}/zls-${ARCH_ZLS}-linux.tar.xz" \
    | tar -C /usr/local/bin -xJ zls \
    && chmod +x /usr/local/bin/zls

# marksman (Markdown LS)
RUN ARCH_MM=$(uname -m); \
    case "$ARCH_MM" in \
      x86_64) MM_ASSET=marksman-linux-x64 ;; \
      aarch64) MM_ASSET=marksman-linux-arm64 ;; \
    esac \
    && curl -fsSL "https://github.com/artempyanykh/marksman/releases/latest/download/${MM_ASSET}" \
       -o /usr/local/bin/marksman \
    && chmod +x /usr/local/bin/marksman

# JSON LS (vscode-langservers-extracted) via npm
RUN npm install -g vscode-langservers-extracted

# ---------- Build lutin-project ----------
WORKDIR /build
COPY . .
RUN cargo build --release --bin lutin-project \
 && install -m 0755 target/release/lutin-project /usr/local/bin/lutin-project \
 && cargo clean

# Drop the workspace source — the runtime only needs the binary plus
# the toolchain for workflow builds. Keeping `/build` would bake a
# stale source copy into the image.
WORKDIR /
RUN rm -rf /build

# `--user` at `docker run` overrides this — but if someone runs the
# image without `--user`, defaulting to a non-privileged user limits
# damage from accidental writes.
WORKDIR /project
ENTRYPOINT ["/usr/local/bin/lutin-project"]
