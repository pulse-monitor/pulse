#!/usr/bin/env bash
# 打一个发布：交叉编译五个目标 → 生成 SHA256 清单 → 用 minisign 签名。
#
# 签名私钥**绝不进仓库**：本地发布用 MINISIGN_SECRET_KEY 指向密钥文件，
# CI 里用加密 secret。
#
# 用法：
#   tools/release.sh 0.4.0 [输出目录]
set -Eeuo pipefail
trap 'echo "❌ 第 $LINENO 行失败" >&2' ERR
cd "$(dirname "$0")/.."

VERSION="${1:?用法: tools/release.sh <版本号> [输出目录]}"
OUT="${2:-dist}"

# 版本号格式必须与 agent 侧的判定一致（crates/pulse-agent/src/update/manifest.rs）
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "版本号必须形如 0.4.0（不带 v 前缀、不带预发布后缀）" >&2
  exit 1
fi

TARGETS=(
  x86_64-unknown-linux-musl
  aarch64-unknown-linux-musl
  x86_64-pc-windows-msvc
  aarch64-apple-darwin
  x86_64-apple-darwin
)

# 公钥必须在编译期注入，否则产出的 agent 自更新是关闭的
: "${PULSE_UPDATE_PUBKEY:?必须设置 PULSE_UPDATE_PUBKEY（minisign 公钥的 base64 那一行）}"

rm -rf "$OUT"
mkdir -p "$OUT"

for t in "${TARGETS[@]}"; do
  echo "=== 构建 $t ==="
  # cargo-zigbuild：ring 的 build script 需要目标平台的 C 编译器，
  # zig 提供它，不用装五套交叉工具链（见 tools/cross-check.sh）
  cargo zigbuild --release --target "$t" -p pulse-agent -p pulse-server

  ext=""; [[ "$t" == *windows* ]] && ext=".exe"
  cp "target/$t/release/pulse-agent$ext" "$OUT/pulse-agent-$t$ext"
  cp "target/$t/release/pulse-server$ext" "$OUT/pulse-server-$t$ext"
done

echo "=== 生成 SHA256 清单 ==="
cd "$OUT"
# 用 sha256sum 的标准格式；agent 侧的解析器认这个格式
if command -v sha256sum >/dev/null; then
  sha256sum ./* > SHA256SUMS
else
  shasum -a 256 ./* > SHA256SUMS
fi
# 去掉 ./ 前缀 —— agent 是按裸文件名查的
sed -i.bak 's| \./| |; s|\*\./|*|' SHA256SUMS && rm -f SHA256SUMS.bak
cat SHA256SUMS

echo "=== 签名 ==="
: "${MINISIGN_SECRET_KEY:?必须设置 MINISIGN_SECRET_KEY（私钥文件路径）}"
minisign -S -s "$MINISIGN_SECRET_KEY" -m SHA256SUMS -t "pulse $VERSION"

echo
echo "产物在 $OUT/："
ls -1
echo
echo "发布时把 SHA256SUMS 与 SHA256SUMS.minisig 一并上传到"
echo "  <update_base>/v$VERSION/"
echo "agent 会先取清单验签，再按清单校验自己那个平台的二进制。"
