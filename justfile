# Conceptify development & build recipes
# See CLAUDE.md and prd.md §11 M0 for context

# Ensure cargo is on PATH for all recipes
export PATH := env_var('HOME') / '.cargo/bin:' + env_var('PATH')

# App dev loop: install deps if needed, then start Tauri dev server
dev:
    @test -d node_modules || npm install
    npm run tauri dev

# Release build of both the app bundle and the CLI
build:
    npm run tauri build
    cargo build --release -p conceptify-cli

# Build the CLI in release mode and symlink into ~/.local/bin (or /usr/local/bin)
install-cli:
    cargo build --release -p conceptify-cli
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -w /usr/local/bin ]; then \
      target_dir="/usr/local/bin"; \
    else \
      target_dir="$HOME/.local/bin"; \
      mkdir -p "$target_dir"; \
      if ! echo "$PATH" | grep -q "$target_dir"; then \
        echo "NOTE: $target_dir is not on PATH; add it to your shell rc:"; \
        echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""; \
      fi; \
    fi; \
    ln -sf "$(pwd)/target/release/conceptify" "$target_dir/conceptify"; \
    echo "Installed: $target_dir/conceptify -> $(pwd)/target/release/conceptify"

# Run cargo check and clippy on all workspace members
check:
    cargo check --workspace
    cargo clippy --workspace -- -D warnings
