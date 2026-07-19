// Conceptify video — palette + type tokens for Remotion compositions.
//
// These VALUES mirror the light-mode ("Manuscript") --cfy-* defaults in
// skill/design-system.css so rendered clips share the artifact's visual
// register (paper background, ink text, terracotta accent, serif display
// / sans body). We hardcode a plain object rather than importing the CSS
// because compositions are React/TS, not a document that loads the
// scaffold.
//
// FUTURE (out of scope here): once the theme-integration bead
// (conceptify-89k.3) lands, this could accept a per-theme palette so a
// video matches the artifact's active theme (incl. dark mode). For now
// it is the single light default — do not wire theme switching here.
//
// Font stacks intentionally drop the Chromium-incompatible `ui-serif`
// keyword and "New York" (WKWebView-only) that the CSS uses: Remotion
// renders in a headless Chromium on the same macOS box, so we name real
// installed families (Iowan Old Style / Palatino for serif, the system
// sans, Menlo for mono) that Chromium resolves deterministically.

export const theme = {
  // palette (design-system.css :root light) -------------------------------
  paper: '#fbf9f4', // page background
  surface: '#f3f0e8', // panels / node fills
  ink: '#211d16', // primary text
  muted: '#6d6759', // secondary text, captions
  line: '#e4dfd2', // hairlines, borders
  accent: '#a34d24', // terracotta — emphasis, active state, markers
  mark: '#f3e5c3', // highlighter swipe

  // diagram tokens (design-system.css :root light) ------------------------
  nodeFill: '#f3f0e8',
  nodeStroke: '#58513f',
  edge: '#6d6759',
  label: '#211d16',
  diagramAccent: '#a34d24',
  diagramAccentBg: '#f0ddcf',

  // type stacks (Chromium-resolvable, macOS installed families) -----------
  serif: '"Iowan Old Style", "Palatino Nova", Palatino, Georgia, "Times New Roman", serif',
  sans: '-apple-system, "Helvetica Neue", Helvetica, Arial, sans-serif',
  mono: '"SF Mono", Menlo, Monaco, Consolas, "Liberation Mono", monospace',
} as const;

// Fixed render targets. 1280x720 @ 30fps keeps every clip inside the
// artifact-spec §1.4 SHOULD budget (<= 720p, <= 30 fps) and well under
// H.264 High profile level 4.0 by construction.
export const RENDER = {
  width: 1280,
  height: 720,
  fps: 30,
} as const;
