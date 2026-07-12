/* @refresh reload */
import { render } from "solid-js/web";
import App from "./App";
import "./app.css";
import { initTheme } from "./store";

initTheme();

render(() => <App />, document.getElementById("root")!);
