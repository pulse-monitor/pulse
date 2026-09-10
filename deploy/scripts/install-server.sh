#!/usr/bin/env bash
#
# Pulse 面板安装脚本。
#
#   curl -fsSL https://raw.githubusercontent.com/pulse-monitor/pulse/main/deploy/scripts/install-server.sh | sudo bash
#
# 参数：
#   --url <地址>       面板对外地址，如 https://panel.example.com
#                      **装在公网上必须给**，安装命令要靠它生成
#   --bind <地址:端口>  监听地址，默认 0.0.0.0:25774
#   --dir <目录>        安装目录，默认 /opt/pulse
#   --version <版本>    指定面板版本，默认取最新
#   --web-version <版本> 指定前端版本（前端是独立仓库，自己发版），默认取最新
#   --tls-cert <路径>   证书；和 --tls-key 一起给就直接跑 HTTPS
#   --tls-key <路径>
#   --uninstall        卸载（**保留数据目录**）
#
# 装完之后面板以专用用户 pulse-server 运行，只对数据目录有写权限。
set -Eeuo pipefail
trap 'echo "❌ 第 $LINENO 行失败" >&2' ERR

REPO=pulse-monitor/pulse
DIR=/opt/pulse
BIND=0.0.0.0:25774
URL=""
VERSION=""
WEB_VERSION=""   # 前端版本，独立于面板；留空取前端仓库的 latest
TLS_CERT=""
TLS_KEY=""
UNINSTALL=0
RUN_USER=pulse-server

GRN=$'\033[32m'; YEL=$'\033[33m'; RED=$'\033[31m'; RST=$'\033[0m'
say()  { printf '%s==>%s %s\n' "$GRN" "$RST" "$1"; }
warn() { printf '%s警告:%s %s\n' "$YEL" "$RST" "$1" >&2; }
die()  { printf '%s错误:%s %s\n' "$RED" "$RST" "$1" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --url)       URL="${2:?--url 需要一个值}"; shift 2 ;;
        --bind)      BIND="${2:?}"; shift 2 ;;
        --dir)       DIR="${2:?}"; shift 2 ;;
        --version)   VERSION="${2:?}"; shift 2 ;;
        --web-version) WEB_VERSION="${2:?}"; shift 2 ;;
        --tls-cert)  TLS_CERT="${2:?}"; shift 2 ;;
        --tls-key)   TLS_KEY="${2:?}"; shift 2 ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help)   sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "未知参数: $1（用 --help 看用法）" ;;
    esac
done

[ "$(id -u)" -eq 0 ] || die "需要 root 权限（要写 systemd unit、建专用用户）。请用 sudo。"
command -v systemctl >/dev/null 2>&1 || die "这个脚本只支持 systemd。别的 init 请参考文档手动部署。"

# ── 卸载 ──────────────────────────────────────────────────────────────────
if [ "$UNINSTALL" -eq 1 ]; then
    systemctl disable --now pulse-server 2>/dev/null || true
    rm -f /etc/systemd/system/pulse-server.service
    systemctl daemon-reload
    rm -f "$DIR/pulse-server"
    rm -rf "$DIR/web"
    say "已卸载。**数据目录 $DIR/data 保留着**，确认不要了再自己删。"
    exit 0
fi

# TLS 两个必须同时给 —— 「以为开了 TLS 其实没开」比起不来危险得多
if { [ -n "$TLS_CERT" ] && [ -z "$TLS_KEY" ]; } || { [ -z "$TLS_CERT" ] && [ -n "$TLS_KEY" ]; }; then
    die "--tls-cert 与 --tls-key 必须同时给"
fi
for f in "$TLS_CERT" "$TLS_KEY"; do
    [ -z "$f" ] || [ -f "$f" ] || die "文件不存在: $f"
done

case "$(uname -m)" in
    x86_64|amd64)  ARCH=x86_64 ;;
    aarch64|arm64) ARCH=aarch64 ;;
    *) die "不支持的架构: $(uname -m)" ;;
esac

if [ -z "$VERSION" ]; then
    say "查询最新版本"
    VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -1)
    [ -n "$VERSION" ] || die "拿不到最新版本号，用 --version 指定"
fi
say "版本 $VERSION"

BASE="https://github.com/$REPO/releases/download/v$VERSION"
# 前端在另一个仓库，自己发自己的版。默认取它的 latest —— 前端是纯静态资源，
# 和面板之间只有 HTTP 接口这一层契约，不必锁死版本。要锁就用 --web-version。
WEB_REPO="${PULSE_WEB_REPO:-pulse-monitor/pulse-web}"
if [ -z "$WEB_VERSION" ]; then
    WEB_VERSION=$(curl -fsSL "https://api.github.com/repos/$WEB_REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -1)
    [ -n "$WEB_VERSION" ] || die "拿不到前端最新版本号，用 --web-version 指定"
fi
WEB_BASE="https://github.com/$WEB_REPO/releases/download/v$WEB_VERSION"
say "前端版本 $WEB_VERSION"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

say "下载面板与前端"
curl -fsSL "$BASE/pulse-server-$ARCH-unknown-linux-musl" -o "$TMP/pulse-server" \
    || die "下载失败：$BASE/pulse-server-$ARCH-unknown-linux-musl"
curl -fsSL "$WEB_BASE/pulse-web-dist.tar.gz" -o "$TMP/web.tar.gz" \
    || die "下载前端失败：$WEB_BASE/pulse-web-dist.tar.gz"

# 前端也核对摘要 —— 它和面板不是同一个 Release，得单独查
if curl -fsSL "$WEB_BASE/SHA256SUMS" -o "$TMP/websums" 2>/dev/null; then
    wwant=$(awk '/pulse-web-dist\.tar\.gz$/{print $1}' "$TMP/websums" | head -1)
    if [ -n "$wwant" ]; then
        wgot=$(sha256sum "$TMP/web.tar.gz" | awk '{print $1}')
        [ "$wwant" = "$wgot" ] || die "前端摘要不符：期望 $wwant，实际 $wgot"
        say "前端摘要校验通过"
    fi
fi

# 校验摘要。清单里没有对应条目时只警告不中断 —— 早期版本可能没传
if curl -fsSL "$BASE/SHA256SUMS" -o "$TMP/sums" 2>/dev/null; then
    want=$(grep " pulse-server-$ARCH-unknown-linux-musl\$" "$TMP/sums" | awk '{print $1}' || true)
    if [ -n "$want" ]; then
        got=$(sha256sum "$TMP/pulse-server" | awk '{print $1}')
        [ "$want" = "$got" ] || die "摘要不匹配，下载可能被篡改"
        say "摘要校验通过"
    else
        warn "清单里没有对应条目，跳过校验"
    fi
else
    warn "取不到 SHA256SUMS，跳过校验"
fi

id "$RUN_USER" >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin "$RUN_USER"

install -d -m 0755 "$DIR" "$DIR/web"
install -d -m 0750 -o "$RUN_USER" -g "$RUN_USER" "$DIR/data"
install -m 0755 "$TMP/pulse-server" "$DIR/pulse-server"
rm -rf "$DIR/web/dist"
tar xzf "$TMP/web.tar.gz" -C "$DIR/web"

# 没给 --url 时按本机公网 IP 猜一个，并明确告诉用户这只是个占位
if [ -z "$URL" ]; then
    ip=$(curl -fsS --max-time 5 https://api.ipify.org 2>/dev/null || echo "")
    port=${BIND##*:}
    URL="http://${ip:-127.0.0.1}:$port"
    warn "没给 --url，暂用 $URL"
    warn "  这个地址会写进探针的安装命令。用域名的话请重跑并加上 --url"
fi

{
    echo "[Unit]"
    echo "Description=Pulse 面板"
    echo "After=network-online.target"
    echo "Wants=network-online.target"
    echo
    echo "[Service]"
    echo "Type=simple"
    echo "User=$RUN_USER"
    echo "Group=$RUN_USER"
    echo "WorkingDirectory=$DIR"
    echo "Environment=PULSE_BIND=$BIND"
    echo "Environment=PULSE_DATA_DIR=$DIR/data"
    echo "Environment=PULSE_DATABASE_URL=sqlite://$DIR/data/pulse.db"
    echo "Environment=PULSE_PUBLIC_URL=$URL"
    echo "Environment=PULSE_WEB_DIR=$DIR/web/dist"
    [ -z "$TLS_CERT" ] || echo "Environment=PULSE_TLS_CERT=$TLS_CERT"
    [ -z "$TLS_KEY" ]  || echo "Environment=PULSE_TLS_KEY=$TLS_KEY"
    echo "ExecStart=$DIR/pulse-server"
    echo "Restart=always"
    echo "RestartSec=3"
    echo
    echo "# 加固：面板只需要读自己的目录、写数据目录"
    echo "NoNewPrivileges=yes"
    echo "PrivateTmp=yes"
    echo "ProtectSystem=strict"
    echo "ProtectHome=yes"
    echo "ReadWritePaths=$DIR/data"
    echo "ProtectKernelTunables=yes"
    echo "ProtectKernelModules=yes"
    echo "ProtectControlGroups=yes"
    echo "RestrictNamespaces=yes"
    echo "RestrictSUIDSGID=yes"
    echo "MemoryDenyWriteExecute=yes"
    echo "UMask=0077"
    echo
    echo "[Install]"
    echo "WantedBy=multi-user.target"
} > /etc/systemd/system/pulse-server.service

# 证书要让运行用户读得到
for f in "$TLS_CERT" "$TLS_KEY"; do
    [ -z "$f" ] && continue
    su -s /bin/sh -c "test -r '$f'" "$RUN_USER" 2>/dev/null \
        || warn "$f 对 $RUN_USER 不可读，面板会起不来。改一下权限。"
done

systemctl daemon-reload
systemctl enable --now pulse-server

for _ in $(seq 1 20); do
    systemctl is-active --quiet pulse-server && break
    sleep 1
done

if ! systemctl is-active --quiet pulse-server; then
    journalctl -u pulse-server -n 30 --no-pager >&2
    die "面板没起来，日志在上面"
fi

echo
say "安装完成"
echo "  面板地址: $URL"
echo "  安装目录: $DIR"
echo "  运行身份: $RUN_USER（非 root）"
# 有没有建过管理员，问面板自己 —— 比翻数据库或猜 unit 文件都准
setup_needed=$(curl -fsS --max-time 5 "$URL/api/v1/auth/setup" 2>/dev/null | grep -o 'true' || echo "")
if [ -n "$setup_needed" ]; then
    echo
    echo "  ${YEL}下一步：打开面板设置管理员账号${RST}"
    echo "    $URL"
    echo "  用户名和密码由你在页面上自行设定，安装脚本不生成、也不保存密码。"
    echo "  ${YEL}请立刻完成${RST} —— 在设置好之前，任何能打开该地址的人都能抢先创建管理员。"
fi
echo
echo "  查看日志: journalctl -u pulse-server -f"
echo "  卸载    : $0 --uninstall"
if [ -z "$TLS_CERT" ] && ! printf '%s' "$URL" | grep -q '^https://'; then
    echo
    warn "面板走的是明文 HTTP。探针的 token 放在握手头里，明文链路上谁都能拿到。"
    warn "  配 --tls-cert/--tls-key，或者放到反代后面并用 --url 指定 https 地址。"
fi
