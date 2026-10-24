/* @refresh reload */
import { render } from "solid-js/web";
// The prewarmed overlay must not load main-window CSS, fonts, session listeners or prompt queues.
if (new URLSearchParams(window.location.search).get("meteor") === "1") {
  document.documentElement.classList.add("meteor-mode");
  const { default: MeteorOverlay } = await import("./components/MeteorOverlay");
  render(() => <MeteorOverlay />, document.getElementById("root")!);
} else if (new URLSearchParams(window.location.search).get("bubble") === "1") {
  // Taskbar island bubble: same lightweight isolation as the overlay.
  document.documentElement.classList.add("bubble-mode");
  const { default: TaskbarBubble } = await import("./components/TaskbarBubble");
  render(() => <TaskbarBubble />, document.getElementById("root")!);
} else if (new URLSearchParams(window.location.search).get("ask") === "1") {
  // 待确认小气泡：与蝴蝶面板同样的轻量隔离，只渲染要确认的事。
  document.documentElement.classList.add("bubble-mode");
  const { TaskbarAsk } = await import("./components/TaskbarBubble");
  render(() => <TaskbarAsk />, document.getElementById("root")!);
} else if (new URLSearchParams(window.location.search).has("miniapp")) {
  // A runtime renders only its app; never mount Nova's shell or prompt queues twice.
  const appId = new URLSearchParams(window.location.search).get("miniapp") || "invalid";
  const [{ default: MiniAppsView }, { initTheme }] = await Promise.all([
    import("./components/MiniAppsView"), import("./store"), import("./app.css"),
    import("@fontsource-variable/inter"), import("@fontsource-variable/jetbrains-mono"),
  ]);
  initTheme();
  render(() => <MiniAppsView runtimeAppId={appId} />, document.getElementById("root")!);
} else {
  const [{ default: App }, { initTheme, migrateLocalModelSelections }] = await Promise.all([
    import("./App"), import("./store"), import("./app.css"),
    import("@fontsource-variable/inter"), import("@fontsource-variable/jetbrains-mono"),
  ]);
  initTheme();
  // HomeView reads lastUsed when constructed, before App.onMount/initStore.
  await migrateLocalModelSelections();
  render(() => <App />, document.getElementById("root")!);
}
