#!/usr/bin/env bash
# 清掉构建缓存与生成物。删的都是能重新生成的东西，源码一个字节不动。
#
# 默认**保留** musl 交叉编译产物 —— 那是部署要用的，重建一次一分多钟。
# 想连它一起删：tools/clean.sh --all
set -Eeuo pipefail
trap 'echo "❌ 第 $LINENO 行失败" >&2' ERR
cd "$(dirname "$0")/.."

all=0
[ "${1:-}" = "--all" ] && all=1

size() { du -sh "$1" 2>/dev/null | cut -f1; }
gone=0
drop() {
  [ -e "$1" ] || return 0
  printf '  删 %-46s %s\n' "$1" "$(size "$1")"
  # macOS 上删大目录树偶尔会报 "Directory not empty" —— 有别的进程正往里写，
  # 或者 APFS 自己的时序问题。重试两次就好，别为这个整个脚本失败。
  for i in 1 2 3; do
    rm -rf "$1" 2>/dev/null && break
    [ "$i" = 3 ] && { echo "  ⚠ 删不掉 $1，可能有进程正在用它" >&2; return 0; }
    sleep 1
  done
  gone=$((gone + 1))
}

echo "=== Rust 构建产物 ==="
drop target/debug          # cargo test / cargo check 的产物，最大的一块
drop target/release        # 本机 release —— 部署用的是 musl，这个没人要
[ "$all" = 1 ] && drop target/x86_64-unknown-linux-musl || \
  echo "  留 target/x86_64-unknown-linux-musl（部署产物，--all 可一并删）"

echo "=== 前端 ==="
drop web/dist
drop web/node_modules/.vite      # vite 依赖预打包缓存
drop web/tsconfig.tsbuildinfo    # tsc -b 的增量信息。这个项目在 ~/Desktop 下、归 iCloud 同步，
                                 # 该文件会被写坏，之后 tsc 就挂住不动（踩过三次）。
                                 # 构建脚本已经改用 `tsc --noEmit`（根本不产生它），
                                 # 这行留着清理历史残留

echo "=== 构建期生成物（都由 npm prebuild 重新生成）==="
drop web/public/flags        # flag-icons 拷过来的 271 面国旗
drop web/public/world-geo.json # world-atlas 生成的国家经纬度多边形
drop web/public/os           # simple-icons 生成的系统图标

echo "=== macOS 垃圾 ==="
n=$(find . -name '.DS_Store' -not -path './web/node_modules/*' 2>/dev/null | wc -l | tr -d ' ')
find . -name '.DS_Store' -not -path './web/node_modules/*' -delete 2>/dev/null || true
echo "  删 .DS_Store × $n"
m=$(find . \( -name '*.bak' -o -name '*.orig' -o -name '*.rej' \) -not -path './web/node_modules/*' -not -path './target/*' 2>/dev/null | wc -l | tr -d ' ')
find . \( -name '*.bak' -o -name '*.orig' -o -name '*.rej' \) -not -path './web/node_modules/*' -not -path './target/*' -delete 2>/dev/null || true
echo "  删 编辑残留 × $m"

echo
echo "✔ 清完。重新构建："
echo "    cargo test --workspace          # Rust（要重新编一遍依赖）"
echo "    cd web && npm run build         # 前端（prebuild 会重新生成国旗/地图/图标）"
echo
echo "没动的东西："
echo "  ~/.cargo/registry（$(size ~/.cargo/registry)）—— 全局依赖源码缓存，"
echo "     多个项目共用，删了别的项目也得重下。要清跑 cargo cache --autoclean"
echo "  web/node_modules —— 删了得重新 npm install"
