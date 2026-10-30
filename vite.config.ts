import { defineConfig } from "vite";
import solid from "@solidjs/vite-plugin";

// Univer 文档自动断词字典（~4MB，按语言动态 import）换成空模块：loadPattern 拿不到字典会直接 return，
// 内联的 en-us 仍可用；表格不用断词，文档断词默认关闭。
const univerHyphenStub = {
  name: "univer-hyphen-stub",
  enforce: "pre" as const,
  load(id: string) {
    if (/@univerjs[\\/]engine-render[\\/]lib[\\/]es[\\/](?!index\.js)[^\\/]+\.js$/.test(id)) return "export {}";
  },
};

export default defineConfig({
  plugins: [univerHyphenStub, solid({ refresh: false })],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
    hmr: false,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "es2022",
  },
});
