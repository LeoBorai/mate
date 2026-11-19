# Sandbox image for `mate`. Embedded in the binary and built on first use;
# see `.agents/docs/sandbox.md` for the image contract.
FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        gnupg \
        tmux \
    && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g @anthropic-ai/claude-code \
    && npm cache clean --force \
    && rm -rf /var/lib/apt/lists/*

# System-wide tmux config: no status bar, so the screen looks like plain
# Claude Code.
COPY tmux.conf /etc/tmux.conf

# Non-root user. `~/.claude` must exist and be owned by this user so the
# per-session state volume mounted there inherits writable ownership.
RUN useradd --create-home --shell /bin/bash --uid 1000 claude \
    && mkdir -p /home/claude/.claude /workspace \
    && chown -R claude:claude /home/claude /workspace

ENV HOME=/home/claude
USER claude
WORKDIR /workspace
