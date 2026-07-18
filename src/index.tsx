/* @refresh reload */
import { render } from "solid-js/web";
import App from "./App";
import "@fontsource-variable/inter";
import "@fontsource-variable/noto-sans-sc";
import "@fontsource-variable/jetbrains-mono";
import modernStyleUrl from "./app.css?url";
import classicStyleUrl from "./app.classic.css?url";
import { configureUiStyles, initTheme } from "./store";

const styleLink = configureUiStyles(modernStyleUrl, classicStyleUrl);
let mounted = false;
const mount = () => {
  if (mounted) return;
  mounted = true;
  render(() => <App />, document.getElementById("root")!);
};

styleLink.addEventListener("load", mount, { once: true });
styleLink.addEventListener("error", mount, { once: true });
initTheme();
if (styleLink.sheet) mount();
