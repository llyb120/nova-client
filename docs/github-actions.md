# GitHub Actions

推送到 GitHub 后自动跑 CI；发版在 Actions 面板手动触发。
自更新从 **GitHub Releases** 拉取，无需私服 token。

## 日常 CI（自动）

- 触发：`push` / `pull_request` → `master` / `main`
- Windows：`Nova.exe` Artifact
- macOS：DMG + `Nova` Artifact

## 发版（手动）

1. **Actions → Release → Run workflow**
2. 选择 `bump` 或填写 `version`
3. 流程：写回版本号并 push → 打 Win/Mac zip（+ macOS DMG）→ 创建 GitHub Release

客户端请求 `https://api.github.com/repos/<owner>/nova-client/releases/latest`，
按资产名 `nova-*.zip` / `nova-macos-*-*.zip` 下载。

编译前可设环境变量 `NOVA_GH_REPO=owner/nova-client` 覆盖默认仓库（见 `src-tauri/src/updater.rs`）。
