#!/usr/bin/env bash
#
# Conceptify setup script — builds the app, installs the CLI and skill.
# Run with --skip-build to skip the expensive release build if already done.
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Ensure cargo is on PATH (justfile does this too)
if [ -d "$HOME/.cargo/bin" ]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Flags
SKIP_BUILD=0

# Parse arguments
for arg in "$@"; do
  case $arg in
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    --help)
      cat <<EOF
Conceptify setup script

Usage: $0 [OPTIONS]

Options:
  --skip-build    Skip the release build step (use if already built)
  --help          Show this help message

This script:
  1. Checks prerequisites (offers brew commands for missing tools)
  2. Builds Conceptify in release mode (unless --skip-build)
  3. Installs the conceptify CLI to PATH
  4. Installs the Claude Code skill globally
  5. Runs 'conceptify doctor' to verify everything works
EOF
      exit 0
      ;;
    *)
      echo -e "${RED}Error: Unknown option: $arg${NC}" >&2
      echo "Run '$0 --help' for usage." >&2
      exit 1
      ;;
  esac
done

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Conceptify Setup"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo

# Step 1: Check prerequisites
echo "→ Checking prerequisites..."
echo

MISSING=()

# Check for brew-installable tools
if ! command -v d2 &> /dev/null; then
  echo -e "${YELLOW}  ✗ d2 not found${NC}"
  echo "    Install with: brew install d2"
  MISSING+=("d2")
else
  echo -e "${GREEN}  ✓ d2${NC}"
fi

if ! command -v dot &> /dev/null; then
  echo -e "${YELLOW}  ✗ graphviz (dot) not found${NC}"
  echo "    Install with: brew install graphviz"
  MISSING+=("graphviz")
else
  echo -e "${GREEN}  ✓ graphviz (dot)${NC}"
fi

if ! command -v node &> /dev/null; then
  echo -e "${YELLOW}  ✗ node not found${NC}"
  echo "    Install with: brew install node"
  MISSING+=("node")
else
  NODE_VERSION=$(node --version | sed 's/v//')
  NODE_MAJOR=$(echo "$NODE_VERSION" | cut -d. -f1)
  if [ "$NODE_MAJOR" -lt 20 ]; then
    echo -e "${YELLOW}  ✗ node version $NODE_VERSION (need >= 20)${NC}"
    echo "    Upgrade with: brew upgrade node"
    MISSING+=("node>=20")
  else
    echo -e "${GREEN}  ✓ node v$NODE_VERSION${NC}"
  fi
fi

if ! command -v claude &> /dev/null; then
  echo -e "${YELLOW}  ✗ claude CLI not found${NC}"
  echo "    Install Claude Code from: https://claude.ai/code"
  MISSING+=("claude")
else
  echo -e "${GREEN}  ✓ claude CLI${NC}"
fi

# Check for Rust/Cargo
if ! command -v cargo &> /dev/null; then
  echo -e "${RED}  ✗ cargo not found${NC}"
  echo "    Install Rust from: https://rustup.rs"
  MISSING+=("cargo")
else
  echo -e "${GREEN}  ✓ cargo${NC}"
fi

# Check for npm
if ! command -v npm &> /dev/null; then
  echo -e "${RED}  ✗ npm not found${NC}"
  echo "    Install with: brew install node"
  MISSING+=("npm")
else
  echo -e "${GREEN}  ✓ npm${NC}"
fi

echo

if [ ${#MISSING[@]} -gt 0 ]; then
  echo -e "${YELLOW}Missing prerequisites: ${MISSING[*]}${NC}"
  echo
  echo "Install all brew-installable tools with:"
  echo "  brew install d2 graphviz node"
  echo
  read -p "Continue anyway? (y/N) " -n 1 -r
  echo
  if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Setup aborted."
    exit 1
  fi
fi

# Step 2: Build
if [ $SKIP_BUILD -eq 0 ]; then
  echo "→ Building Conceptify (release mode)..."
  echo "  This may take several minutes on first run."
  echo

  cd "$REPO_ROOT"

  if ! just build; then
    echo -e "${RED}Build failed!${NC}" >&2
    exit 1
  fi

  echo
  echo -e "${GREEN}  ✓ Build complete${NC}"
  echo
else
  echo "→ Skipping build (--skip-build)"
  echo

  # Verify binaries exist
  if [ ! -f "$REPO_ROOT/target/release/conceptify" ]; then
    echo -e "${RED}Error: CLI binary not found at target/release/conceptify${NC}" >&2
    echo "Run without --skip-build to build first." >&2
    exit 1
  fi
fi

# Step 3: Install CLI
echo "→ Installing conceptify CLI..."
echo

cd "$REPO_ROOT"

if ! just install-cli; then
  echo -e "${RED}CLI installation failed!${NC}" >&2
  exit 1
fi

echo
echo -e "${GREEN}  ✓ CLI installed${NC}"
echo

# Step 4: Install skill
echo "→ Installing Claude Code skill..."
echo

cd "$REPO_ROOT"

if ! just install-skill; then
  echo -e "${RED}Skill installation failed!${NC}" >&2
  exit 1
fi

echo
echo -e "${GREEN}  ✓ Skill installed to ~/.claude/skills/conceptify${NC}"
echo

# Step 5: Run doctor
echo "→ Running verification (conceptify doctor)..."
echo

# Ensure the new CLI is available by using the full path
CLI_PATH="$REPO_ROOT/target/release/conceptify"
if [ -w /usr/local/bin ] && [ -L /usr/local/bin/conceptify ]; then
  CLI_PATH="conceptify"
elif [ -L "$HOME/.local/bin/conceptify" ]; then
  # Add to PATH for this script if not already there
  export PATH="$HOME/.local/bin:$PATH"
  CLI_PATH="conceptify"
fi

if ! $CLI_PATH doctor; then
  echo
  echo -e "${YELLOW}Warning: Some checks failed. Review output above.${NC}"
else
  echo
  echo -e "${GREEN}  ✓ All checks passed!${NC}"
fi

# Final message
echo
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "${GREEN}Setup complete!${NC}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo
echo "Next steps:"
echo
echo "  1. Launch the app:"
echo "     just dev          # Development mode with hot reload"
echo "     open -a Conceptify.app  # Launch the release bundle"
echo
echo "  2. Try the CLI:"
echo "     conceptify status"
echo
echo "  3. Use from Claude Code in any codebase:"
echo "     \"Use conceptify to explain how authentication works in this repo.\""
echo
echo "See README.md for more usage examples and documentation links."
echo
