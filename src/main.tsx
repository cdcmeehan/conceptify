import { render } from "preact";
import App from "./App";
import { initTheme } from "./lib/theme";

// Apply the appearance (FR-7.2) before first paint. Starts in `system` (OS
// preference); App re-applies the stored setting once it loads.
initTheme();

render(<App />, document.getElementById("root") as HTMLElement);
