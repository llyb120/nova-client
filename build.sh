#!/usr/bin/env bash
# 构建 Nova release 二进制（macOS / Linux）
set -euo pipefail
cd "$(dirname "$0")"

command -v node >/dev/null || { echo "[错误] 未找到 Node.js，请先安装"; exit 1; }
command -v cargo >/dev/null || { echo "[错误] 未找到 Rust/cargo，请先安装"; exit 1; }

echo "[1/2] 安装前端依赖..."
npm install

echo "[2/2] 构建 release 版本（首次编译需要几分钟）..."
npx tauri build --no-bundle

BIN="src-tauri/target/release/Nova"
if [[ ! -f "$BIN" ]]; then
  echo "[错误] 未找到构建产物: $BIN"
  exit 1
fi
chmod +x "$BIN"

echo
echo "============================================================"
echo " 构建成功！"
echo " 可执行文件: $(pwd)/$BIN"
echo " 运行前提: 目标机器已安装 Devin CLI 并完成登录"
echo "============================================================"
