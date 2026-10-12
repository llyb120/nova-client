/* @refresh reload */
import { render } from "solid-js/web";
// The prewarmed overlay must not load main-window CSS, fonts, session listeners or prompt queues.
if (new URLSearchParams(window.location.search).get("meteor") === "1") {
  document.documentElement.classList.add("meteor-mode");
  const { default: MeteorOverlay } = await import("./components/MeteorOverlay");
  render(() => <MeteorOverlay />, document.getElementById("root")!);
} else {
  const [{ default: App }, { initTheme }] = await Promise.all([
    import("./App"), import("./store"), import("./app.css"),
    import("@fontsource-variable/inter"), import("@fontsource-variable/noto-sans-sc"), import("@fontsource-variable/jetbrains-mono"),
  ]);
  initTheme();
  render(() => <App />, document.getElementById("root")!);
}
