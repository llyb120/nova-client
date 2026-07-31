#!/usr/bin/env bash
# context-bundle.sh — 把"找上下文"从 agent 推理循环里剥离出来的打包器
#
# 用法:
#   scripts/context-bundle.sh <关键词|符号> [更多关键词...]
#   BUDGET=800 scripts/context-bundle.sh parse_config   # 自定义行数预算
#
# 输出: context-bundle.md (按相关性排序, 分层装配)
#
# 三阶段, 全程无 LLM:
#   Phase1 定位   git grep 命中 -> 文件按命中数排名
#   Phase2 扩展   命中符号的调用方/被调用方, 1 跳
#   Phase3 装配   小文件全文 / 大文件命中段+符号大纲, 超预算降级为大纲
set -eo pipefail

cd "$(git rev-parse --show-toplevel)"

BUDGET="${BUDGET:-700}"        # 总行数预算(近似 token)
CTX="${CTX:-12}"               # 命中行上下文半径
MAX_FILES="${MAX_FILES:-12}"   # 最多装配文件数
OUT="${OUT:-context-bundle.md}"
EXCLUDE='src-tauri/target|node_modules|package-lock|\.png|dist/'
# 扩展(邻居)专用过滤: 比命中更严, 只留源码, 排除文档/CI/配置噪声
NEIGHBOR_EXCLUDE='\.md$|\.github/|\.yml$|\.yaml$|\.json$|\.toml$|docs/|scripts/legacy-context'

if [ $# -lt 1 ]; then
  echo "用法: $0 <关键词> [关键词...]" >&2
  exit 1
fi

# ---- Phase 1: 定位, 文件按命中数排名 ----
declare -A HITS
for kw in "$@"; do
  while IFS=: read -r f _; do
    [[ "$f" =~ $EXCLUDE ]] && continue
    HITS["$f"]=$(( ${HITS["$f"]:-0} + 1 ))
  done < <(git grep -nI -- "$kw" 2>/dev/null || true)
done

if [ ${#HITS[@]} -eq 0 ]; then
  echo "无命中: $*" >&2
  exit 1
fi

# 排序: 命中数降序
mapfile -t RANKED < <(
  for f in "${!HITS[@]}"; do echo "${HITS[$f]} $f"; done | sort -rn
)

# ---- Phase 2: 1 跳扩展(调用方) ----
# 从排名前几的文件里抽顶层符号, 再 grep 其调用方文件
declare -A EXTRA
for entry in "${RANKED[@]:0:5}"; do
  f="${entry#* }"
  # 粗抽顶层符号: 行首关键字后的标识符
  syms=$(rg -oN '^\s*(?:pub(?:\(.*\))?\s+)?(?:async\s+)?(?:fn|struct|enum|trait|type)\s+([A-Za-z_][A-Za-z0-9_]*)' \
           -r '$1' "$f" 2>/dev/null | sort -u | head -20 || true)
  for s in $syms; do
    [ ${#s} -lt 4 ] && continue
    while IFS=: read -r cf _; do
      [[ "$cf" == "$f" ]] && continue
      [[ "$cf" =~ $EXCLUDE ]] && continue
      [[ "$cf" =~ $NEIGHBOR_EXCLUDE ]] && continue
      EXTRA["$cf"]=1
    done < <(git grep -nI -- "\b$s\b" 2>/dev/null | head -5 || true)
  done
done

# ---- Phase 3: 装配 ----
{
  echo "# Context Bundle"
  echo
  echo "- 查询: $*"
  echo "- 生成: $(date '+%F %T')  commit: $(git rev-parse --short HEAD)"
  echo "- 预算: ${BUDGET} 行  命中文件: ${#HITS[@]}  扩展文件: ${#EXTRA[@]}"
  echo
  echo "## 命中排名 (命中数 文件)"
  for entry in "${RANKED[@]}"; do echo "    $entry"; done
  echo

  used=0
  assemble() {
    local f="$1" tag="$2"
    [ ! -f "$f" ] && return
    local total; total=$(wc -l < "$f")
    echo "----- [$tag] $f  (${total} 行) -----"
    if [ "$total" -le 200 ]; then
      # 小文件: 全文
      cat -n "$f"
      used=$(( used + total ))
    else
      # 大文件: 符号大纲 + 命中段
      echo "  ## 符号大纲"
      rg -nN '^\s*(pub(?:\(.*\))?\s+)?(async\s+)?(fn|struct|enum|trait|impl|type|export)\b' "$f" \
        2>/dev/null | sed 's/\s*{.*//;s/ *=.*//;s/ *$//' | head -60 || true
      echo "  ## 命中上下文"
      for kw in "$@"; do :; done
      git grep -nI -C "$CTX" -- "${QUERY_KW[@]}" "$f" 2>/dev/null | head -120 || true
      used=$(( used + 120 ))
    fi
    echo
  }

  QUERY_KW=("$@")
  echo "# ===== 核心命中文件 ====="
  for entry in "${RANKED[@]:0:MAX_FILES}"; do
    [ "$used" -ge "$BUDGET" ] && { echo "(预算耗尽, 其余文件仅列名)"; break; }
    assemble "${entry#* }" "HIT"
  done

  echo "# ===== 1 跳扩展文件(仅大纲) ====="
  for f in "${!EXTRA[@]}"; do
    [ -f "$f" ] || continue
    [ "$used" -ge "$BUDGET" ] && { echo "(预算耗尽, 剩余邻居仅列名于下)"; break; }
    echo "----- [NEIGHBOR] $f -----"
    rg -nN '^\s*(pub(?:\(.*\))?\s+)?(async\s+)?(fn|struct|enum|trait|impl|type|export)\b' "$f" \
      2>/dev/null | sed 's/\s*{.*//;s/ *=.*//;s/ *$//' | head -30 || true
    echo
    used=$(( used + 30 ))
  done

  echo "# ===== 未展开的大文件(可追加) ====="
  for entry in "${RANKED[@]:MAX_FILES}"; do
    echo "    ${entry#* }"
  done
} > "$OUT"

echo "已生成 $OUT ($(wc -l < "$OUT") 行)"
