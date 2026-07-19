// Deterministic content-box geometry shared by Stage and the diagram
// compositions. Stage applies PAD padding on every side and reserves a
// fixed TITLE_H band for the title when one is present, so a composition
// can lay out inside a box of known pixel size (no measurement, no
// coordinate-system mixing).

export const PAD = 64;
export const TITLE_H = 112;

/** Inner drawing area, in px, inside Stage's padding + title band. */
export function contentBox(width: number, height: number, hasTitle = true) {
  return {
    w: width - PAD * 2,
    h: height - PAD * 2 - (hasTitle ? TITLE_H : 0),
  };
}
