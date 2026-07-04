#!/usr/bin/env node
// Conceptify skill — Shiki v4 code highlighting (D4).
//
// Pre-renders a code excerpt to dual-theme HTML (vitesse-light /
// vitesse-dark, `--shiki-dark` variable prefix) for inlining into an
// artifact. Zero runtime JS in the artifact; the design-system scaffold
// flips the spans under `prefers-color-scheme: dark`.
//
// Usage:
//   node highlight.mjs --lang rust --input src/main.rs [--highlight 3,7-9]
//   cat snippet.py | node highlight.mjs --lang python
//
// Output: the `<pre class="shiki ...">...</pre>` block on stdout.
// Wrap it in `<figure class="cfy-listing">` per design-system.md.
//
// First run bootstraps shiki@^4 into ~/.cache/conceptify/shiki-env via
// npm (network needed once); later runs are offline.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  return i !== -1 ? process.argv[i + 1] : undefined;
}

const lang = arg("lang");
if (!lang) {
  console.error(
    "usage: highlight.mjs --lang <lang> [--input <file>] [--highlight 3,7-9]",
  );
  process.exit(2);
}
const input = arg("input");
// (async stdin read: readFileSync(0) EAGAINs on macOS non-blocking pipes)
async function readStdin() {
  const chunks = [];
  for await (const c of process.stdin) chunks.push(c);
  return Buffer.concat(chunks).toString("utf8");
}
const code = (input ? fs.readFileSync(input, "utf8") : await readStdin()).replace(/\n$/, "");

// --highlight "3,7-9" -> Set{3,7,8,9} (1-based line numbers)
const highlighted = new Set();
for (const part of (arg("highlight") ?? "").split(",").filter(Boolean)) {
  const [a, b] = part.split("-").map(Number);
  for (let n = a; n <= (b ?? a); n++) highlighted.add(n);
}

// --- bootstrap shiki into a persistent cache env, then import it ---------
const envDir = path.join(os.homedir(), ".cache", "conceptify", "shiki-env");
if (!fs.existsSync(path.join(envDir, "node_modules", "shiki"))) {
  fs.mkdirSync(envDir, { recursive: true });
  console.error("bootstrapping shiki@^4 into " + envDir + " ...");
  execFileSync(
    "npm",
    ["install", "--prefix", envDir, "--no-audit", "--no-fund", "shiki@^4"],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
}
// A shim module inside the env dir makes `import "shiki"` resolve against
// the env's node_modules regardless of where this script lives.
const shim = path.join(envDir, "cfy-shiki-shim.mjs");
fs.writeFileSync(shim, 'export * from "shiki";\n');
const { codeToHtml } = await import(pathToFileURL(shim).href);

const html = await codeToHtml(code, {
  lang,
  themes: { light: "vitesse-light", dark: "vitesse-dark" },
  transformers: [
    {
      line(node, line) {
        if (highlighted.has(line)) this.addClassToHast(node, "highlighted");
      },
    },
  ],
});

process.stdout.write(html + "\n");
