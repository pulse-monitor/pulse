#!/usr/bin/env bash
# 依赖审计：安全公告 + 许可证 + 被忽略项的前提是否仍然成立。
#
# 最后一步是关键：.cargo/audit.toml 里忽略 RUSTSEC-2023-0071 的**唯一理由**
# 是「rsa 不在实际构建图里」。哪天某个依赖真的把它拉进来了，
# 那条忽略就变成了在掩盖一个真实漏洞 —— 所以这里独立验证一次。
set -Eeuo pipefail
trap 'echo "❌ 第 $LINENO 行失败" >&2' ERR
cd "$(dirname "$0")/.."

fail=0
say() { printf '  %s\n' "$*"; }

echo "=== cargo audit（安全公告）==="
if cargo audit; then say "✔ 无未忽略的安全公告"; else say "❌ 有安全公告"; fail=1; fi

echo
echo "=== cargo deny（许可证 / 来源 / 禁用项）==="
if cargo deny check; then say "✔ 通过"; else say "❌ 未通过"; fail=1; fi

echo
echo "=== 被忽略项的前提是否仍然成立 ==="
# cargo tree 在包不在图里时会打印 "nothing to print" 到 stderr 且退出码为 0，
# 所以判据是**输出里有没有那个包名**，不能看退出码。
for pkg in rsa sqlx-mysql sqlx-postgres; do
  out=$(cargo tree -i "$pkg" 2>&1 || true)
  if grep -qE "^${pkg} v" <<<"$out"; then
    say "❌ $pkg 已进入构建图 —— .cargo/audit.toml 里对它的忽略不再成立，请重新评估"
    fail=1
  else
    say "✔ $pkg 不在构建图里"
  fi
done

echo
echo "=== 禁止 openssl（会打断 musl 交叉编译）==="
if cargo tree -i openssl-sys 2>&1 | grep -qE "^openssl-sys v"; then
  say "❌ openssl-sys 进入了构建图，musl 交叉编译会崩"
  fail=1
else
  say "✔ 全链路 rustls，无 openssl"
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "全部通过。"
else
  echo "有未通过项，见上。" >&2
  exit 1
fi
