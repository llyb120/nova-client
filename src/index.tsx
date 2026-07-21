/* @refresh reload */
import { render } from "solid-js/web";
import App from "./App";
import "@fontsource-variable/inter";
import "@fontsource-variable/noto-sans-sc";
import "@fontsource-variable/jetbrains-mono";
import "./app.css";
import { initTheme } from "./store";

initTheme();
render(() => <App />, document.getElementById("root")!);
