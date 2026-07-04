# Conceptify

A personal macOS app for AI-assisted conceptual artifacts — diagrams, explainers, and research documents.

## Prerequisites

Run `conceptify doctor` to verify your environment has everything needed:

- Conceptify.app (installed or running)
- `conceptify` CLI on PATH
- `d2` (diagram generation)
- `dot` / graphviz (graph rendering)
- `node` >= v20 (Shiki syntax highlighting)
- `claude` (Claude Code agent binary)

Install missing tools via Homebrew:

```bash
brew install d2 graphviz node
```

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
