// App-shell appearance (PRD FR-7.2): system | light | dark.
//
// The shell's dark styles key off a `data-theme` attribute on <html> (see the
// `@custom-variant dark` in App.css) rather than the raw `prefers-color-scheme`
// media query, so Settings can *force* light or dark. `system` resolves the OS
// preference live via `matchMedia`. `color-scheme` is set to the resolved value
// so native form controls (checkboxes, selects) render for the right appearance.
//
// LIMITATION (honest, and surfaced in the Settings UI): the artifact viewer runs
// in a cross-origin sandboxed iframe (PRD §9 S2 origin isolation — the shell
// cannot reach into it), so it follows its OWN `prefers-color-scheme` (the OS),
// not this setting. In forced light/dark the shell switches but the artifact
// tracks the OS. `system` is the one mode where the reading surface and the
// shell always agree; it is the primary path.

export type Appearance = "system" | "light" | "dark";

const mql = window.matchMedia("(prefers-color-scheme: dark)");
let current: Appearance = "system";

function resolveDark(): boolean {
  return current === "dark" || (current === "system" && mql.matches);
}

function apply(): void {
  const dark = resolveDark();
  const root = document.documentElement;
  root.setAttribute("data-theme", dark ? "dark" : "light");
  root.style.colorScheme = dark ? "dark" : "light";
}

/** Set the appearance and apply it immediately (no restart — FR-7.2). */
export function setAppearance(appearance: Appearance): void {
  current = appearance;
  apply();
}

/** The appearance currently applied to the shell. */
export function getAppearance(): Appearance {
  return current;
}

/**
 * Initialize theming once at startup: apply the current value and keep it in
 * sync with the OS while in `system` mode. Safe to call before settings load —
 * it starts in `system`, then `App` re-applies the stored value once fetched.
 * Re-applying on the media-query change is a no-op for forced light/dark.
 */
export function initTheme(): void {
  mql.addEventListener("change", apply);
  apply();
}
