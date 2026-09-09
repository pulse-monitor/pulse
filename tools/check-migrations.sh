#!/usr/bin/env bash
# 已发布的迁移文件一个字节都不能再改 —— 连注释也不行。
#
# sqlx 对每个迁移算校验和并记进库里。文件内容一变，已部署的实例启动就报
#   migration N was previously applied but has been modified
# 然后进入重启循环，而且从报错里看不出「是谁改了它」。
#
# 这坑真踩过：一次目录重构，批量替换文档路径的脚本顺手把迁移注释里的
# 数据模型文档路径改成了新目录，真机面板当场挂掉。
#
# 用法：
#   tools/check-migrations.sh            校验（CI / 提交前）
#   tools/check-migrations.sh --update   新增迁移后登记进 CHECKSUMS
set -Eeuo pipefail
trap 'echo "❌ 第 $LINENO 行失败" >&2' ERR
cd "$(dirname "$0")/.."

dir=crates/pulse-server/migrations
sums="$dir/CHECKSUMS"
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

# 排序保证顺序稳定，与目录遍历顺序无关
hash_all() {
  find "$dir" -name '*.sql' | LC_ALL=C sort | while read -r f; do
    printf '%s  %s\n' "$(sha "$f")" "${f#"$dir"/}"
  done
}

if [ "${1:-}" = "--update" ]; then
  hash_all > "$sums"
  echo "✔ 已登记 $(wc -l < "$sums" | tr -d ' ') 个迁移到 $sums"
  exit 0
fi

[ -f "$sums" ] || { echo "❌ 缺少 $sums，先跑：tools/check-migrations.sh --update" >&2; exit 1; }

now=$(mktemp); trap 'rm -f "$now"' EXIT
hash_all > "$now"

if diff -u "$sums" "$now" > /dev/null; then
  echo "✔ $(wc -l < "$sums" | tr -d ' ') 个迁移文件均未被改动"
else
  {
    echo "❌ 迁移文件被改动了。已部署的实例会启动失败："
    echo "     migration N was previously applied but has been modified"
    echo
    diff -u "$sums" "$now" | sed -n '3,$p' | grep -E '^[+-][^+-]' || true
    echo
    echo "要改 schema 请**新增**一个迁移文件，不要动旧的。"
    echo "若确实是新增了迁移：tools/check-migrations.sh --update"
  } >&2
  exit 1
fi
