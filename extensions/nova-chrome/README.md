# Nova Chrome 独立扩展（v0.2.0）

扩展独立安装和发布。Nova 的 `browser_*` 工具通过持久化本地 WebSocket 与本扩展通信，复用用户 Chrome 的登录态与标签页；并在被操作页面内渲染 Top-Layer 虚拟鼠标指针完成所有点击、聚焦输入、悬停、拖拽与滚动操作。

## 安装与自动升级

1. 打开 `chrome://extensions`，开启开发者模式，点击「加载已解压的扩展程序」，选择含 `manifest.json` 的目录。
2. 启动 Nova 后，扩展通过 `ws://127.0.0.1:47653–47662/ws` 自动连接，无需手工配置地址或密钥；原有 0.1.x 版本会在首次连接时自动重载升级到 `0.2.0`。
3. 点击扩展图标可查看当前连接数并一键复制当前激活标签的 tag。

## 验证

- `node --test extensions/nova-chrome/worker.test.mjs scripts/chrome-tools.test.mjs`
- `cargo test --manifest-path src-tauri/Cargo.toml --lib -- browser::`
