#!/usr/bin/env bash
# 打包发布 Nova（跨平台入口）
set -euo pipefail
cd "$(dirname "$0")"
exec python3 -u scripts/package.py "$@"
