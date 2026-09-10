#!/bin/sh
#
# Pulse 面板安装脚本。支持 systemd 与 OpenRC（Alpine）。
#
#   U=https://raw.githubusercontent.com/pulse-monitor/pulse/main/deploy/scripts/install-server.sh
#   (curl -fsSL $U || wget -qO- $U) | $(command -v sudo) sh -s -- --url https://panel.example.com
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
#
# 刻意写成 POSIX sh 而不是 bash：原生 Alpine 没有 bash（也没有 curl、sudo）。
set -eu

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
SERVICE=pulse-server

GRN=$(printf '\033[32m'); YEL=$(printf '\033[33m'); RED=$(printf '\033[31m'); RST=$(printf '\033[0m')
say()  { printf '%s==>%s %s\n' "$GRN" "$RST" "$1"; }
warn() { printf '%s警告:%s %s\n' "$YEL" "$RST" "$1" >&2; }
die()  { printf '%s错误:%s %s\n' "$RED" "$RST" "$1" >&2; exit 1; }

# POSIX sh 没有 bash 的 ERR trap。退出码非 0 时至少留个痕迹，不要静默退出。
TMP=""
# shellcheck disable=SC2329,SC2317
on_exit() {
    st=$?
    [ -n "$TMP" ] && rm -rf "$TMP"
    [ "$st" -eq 0 ] || printf '%s安装未完成%s（退出码 %s）\n' "$RED" "$RST" "$st" >&2
    exit "$st"
}
trap on_exit EXIT

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
        -h|--help)   sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "未知参数: $1（用 --help 看用法）" ;;
    esac
done

[ "$(id -u)" -eq 0 ] || die "需要 root 权限（要写服务定义、建专用用户）。请用 root 或 sudo/doas 运行。"

# ── init 系统 ─────────────────────────────────────────────────────────────
detect_init() {
    if [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then
        echo systemd
    elif command -v rc-update >/dev/null 2>&1; then
        echo openrc
    else
        echo unknown
    fi
}
INIT=$(detect_init)

# ── 卸载 ──────────────────────────────────────────────────────────────────
if [ "$UNINSTALL" -eq 1 ]; then
    if [ "$INIT" = systemd ]; then
        systemctl disable --now "$SERVICE" 2>/dev/null || true
        rm -f "/etc/systemd/system/$SERVICE.service"
        systemctl daemon-reload
    elif [ "$INIT" = openrc ]; then
        rc-service "$SERVICE" stop 2>/dev/null || true
        rc-update del "$SERVICE" default 2>/dev/null || true
        rm -f "/etc/init.d/$SERVICE" "/var/log/$SERVICE.log"
    fi
    rm -f "$DIR/pulse-server"
    rm -rf "$DIR/web"
    say "已卸载。**数据目录 $DIR/data 保留着**，确认不要了再自己删。"
    exit 0
fi

case "$INIT" in
    systemd|openrc) ;;
    *) die "不支持的 init（检测到: $INIT）。目前支持 systemd 与 OpenRC，别的 init 请参考文档手动部署。" ;;
esac

# 这些值会原样写进服务定义（OpenRC 下是 shell 脚本里的单引号字符串），
# 带单引号或换行会把服务定义写坏
for v in "$URL" "$BIND" "$DIR" "$TLS_CERT" "$TLS_KEY"; do
    case "$v" in
        *"'"*|*'
'*) die "参数里不能包含单引号或换行: $v" ;;
    esac
done

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

# ── 下载工具：原生 Alpine 没有 curl，只有 busybox 的 wget ──────────────────
if command -v curl >/dev/null 2>&1; then HAVE_CURL=1
elif command -v wget >/dev/null 2>&1; then HAVE_CURL=0
else die "需要 curl 或 wget"; fi

# fetch URL 文件
fetch() {
    if [ "$HAVE_CURL" -eq 1 ]; then curl -fsSL "$1" -o "$2"
    else wget -qO "$2" "$1"; fi
}
# fetch_out URL [超时秒数] —— 写到 stdout
fetch_out() {
    if [ "$HAVE_CURL" -eq 1 ]; then curl -fsSL --max-time "${2:-30}" "$1"
    else wget -qO- -T "${2:-30}" "$1"; fi
}

if [ -z "$VERSION" ]; then
    say "查询最新版本"
    VERSION=$(fetch_out "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -1)
    [ -n "$VERSION" ] || die "拿不到最新版本号，用 --version 指定"
fi
say "版本 $VERSION"

BASE="https://github.com/$REPO/releases/download/v$VERSION"
# 前端在另一个仓库，自己发自己的版。默认取它的 latest —— 前端是纯静态资源，
# 和面板之间只有 HTTP 接口这一层契约，不必锁死版本。要锁就用 --web-version。
WEB_REPO="${PULSE_WEB_REPO:-pulse-monitor/pulse-web}"
if [ -z "$WEB_VERSION" ]; then
    WEB_VERSION=$(fetch_out "https://api.github.com/repos/$WEB_REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -1)
    [ -n "$WEB_VERSION" ] || die "拿不到前端最新版本号，用 --web-version 指定"
fi
WEB_BASE="https://github.com/$WEB_REPO/releases/download/v$WEB_VERSION"
say "前端版本 $WEB_VERSION"

TMP=$(mktemp -d)

say "下载面板与前端"
fetch "$BASE/pulse-server-$ARCH-unknown-linux-musl" "$TMP/pulse-server" \
    || die "下载失败：$BASE/pulse-server-$ARCH-unknown-linux-musl"
fetch "$WEB_BASE/pulse-web-dist.tar.gz" "$TMP/web.tar.gz" \
    || die "下载前端失败：$WEB_BASE/pulse-web-dist.tar.gz"

# 前端也核对摘要 —— 它和面板不是同一个 Release，得单独查
if fetch "$WEB_BASE/SHA256SUMS" "$TMP/websums" 2>/dev/null; then
    wwant=$(awk '/pulse-web-dist\.tar\.gz$/{print $1}' "$TMP/websums" | head -1)
    if [ -n "$wwant" ]; then
        wgot=$(sha256sum "$TMP/web.tar.gz" | awk '{print $1}')
        [ "$wwant" = "$wgot" ] || die "前端摘要不符：期望 $wwant，实际 $wgot"
        say "前端摘要校验通过"
    fi
fi

# 校验摘要。清单里没有对应条目时只警告不中断 —— 早期版本可能没传
if fetch "$BASE/SHA256SUMS" "$TMP/sums" 2>/dev/null; then
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

# ── 用户 ──────────────────────────────────────────────────────────────────
if ! id "$RUN_USER" >/dev/null 2>&1; then
    # nologin 的路径各发行版不一样：Debian 系在 /usr/sbin，Alpine 在 /sbin
    NOLOGIN=$(command -v nologin 2>/dev/null || true)
    if [ -z "$NOLOGIN" ]; then
        for c in /usr/sbin/nologin /sbin/nologin /bin/false; do
            [ -x "$c" ] && { NOLOGIN=$c; break; }
        done
    fi
    if command -v useradd >/dev/null 2>&1; then
        useradd --system --no-create-home --shell "$NOLOGIN" "$RUN_USER"
    else
        # busybox 的 adduser（Alpine），参数语法和 Debian 版完全不同
        addgroup -S "$RUN_USER" 2>/dev/null || true
        adduser -S -H -s "$NOLOGIN" -h "$DIR/data" -G "$RUN_USER" "$RUN_USER" \
            || die "创建用户失败"
    fi
fi

install -d -m 0755 "$DIR" "$DIR/web"
install -d -m 0750 -o "$RUN_USER" -g "$RUN_USER" "$DIR/data"
install -m 0755 "$TMP/pulse-server" "$DIR/pulse-server"
rm -rf "$DIR/web/dist"
tar xzf "$TMP/web.tar.gz" -C "$DIR/web"

# 没给 --url 时按本机公网 IP 猜一个，并明确告诉用户这只是个占位
if [ -z "$URL" ]; then
    ip=$(fetch_out https://api.ipify.org 5 2>/dev/null || echo "")
    port=${BIND##*:}
    URL="http://${ip:-127.0.0.1}:$port"
    warn "没给 --url，暂用 $URL"
    warn "  这个地址会写进探针的安装命令。用域名的话请重跑并加上 --url"
fi

# 证书要让运行用户读得到
for f in "$TLS_CERT" "$TLS_KEY"; do
    [ -z "$f" ] && continue
    su -s /bin/sh -c "test -r '$f'" "$RUN_USER" 2>/dev/null \
        || warn "$f 对 $RUN_USER 不可读，面板会起不来。改一下权限。"
done

# ── 服务定义 ──────────────────────────────────────────────────────────────
if [ "$INIT" = systemd ]; then
    {
        echo "[Unit]"
        echo "Description=Pulse Server"
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
    } > "/etc/systemd/system/$SERVICE.service"

    systemctl daemon-reload
    systemctl enable "$SERVICE" >/dev/null 2>&1
    # 不能用 enable --now：服务已在跑时它什么都不做，升级换了二进制，
    # 跑着的还是旧进程，要等下次重启机器才生效
    systemctl restart "$SERVICE"
else
    # OpenRC（Alpine 等）。没有 systemd 那套命名空间沙箱，
    # 能保证的是非 root 专用用户 + no_new_privs + umask。
    # 环境变量必须在脚本**顶层** export —— start_pre 里 export 的
    # 传不到 supervise-daemon 起的进程（agent 那边实测踩过）。
    {
        echo '#!/sbin/openrc-run'
        echo
        echo 'name="pulse-server"'
        echo 'description="Pulse Server"'
        echo
        echo "command='$DIR/pulse-server'"
        echo "command_user='$RUN_USER:$RUN_USER'"
        echo "directory='$DIR'"
        echo 'supervisor=supervise-daemon'
        echo 'respawn_delay=3'
        echo 'respawn_max=0'
        echo 'no_new_privs=yes'
        echo 'umask=0077'
        echo 'pidfile="/run/${RC_SVCNAME}.pid"'
        echo 'output_log="/var/log/${RC_SVCNAME}.log"'
        echo 'error_log="/var/log/${RC_SVCNAME}.log"'
        echo
        echo 'depend() {'
        echo '    need net'
        echo '    after firewall'
        echo '}'
        echo
        echo "export PULSE_BIND='$BIND'"
        echo "export PULSE_DATA_DIR='$DIR/data'"
        echo "export PULSE_DATABASE_URL='sqlite://$DIR/data/pulse.db'"
        echo "export PULSE_PUBLIC_URL='$URL'"
        echo "export PULSE_WEB_DIR='$DIR/web/dist'"
        [ -z "$TLS_CERT" ] || echo "export PULSE_TLS_CERT='$TLS_CERT'"
        [ -z "$TLS_KEY" ]  || echo "export PULSE_TLS_KEY='$TLS_KEY'"
        echo
        echo 'start_pre() {'
        echo "    checkpath -f -m 0640 -o $RUN_USER:$RUN_USER \"/var/log/\${RC_SVCNAME}.log\""
        echo '}'
    } > "/etc/init.d/$SERVICE"
    chmod 0755 "/etc/init.d/$SERVICE"

    rc-update add "$SERVICE" default >/dev/null 2>&1 || true
    rc-service "$SERVICE" restart
fi

# ── 启动后验证 ────────────────────────────────────────────────────────────
is_running() {
    if [ "$INIT" = systemd ]; then
        systemctl is-active --quiet "$SERVICE"
    else
        # supervise-daemon 自己活着就报 started，哪怕面板一起来就崩。
        # 还要确认面板进程在，且隔两秒还是同一个 pid。
        rc-service "$SERVICE" status 2>/dev/null | grep -q started || return 1
        p1=$(child_pid)
        [ -n "$p1" ] || return 1
        sleep 2
        [ "$(child_pid)" = "$p1" ]
    fi
}
child_pid() {
    p=$(cat "/run/openrc/options/$SERVICE/child_pid" 2>/dev/null || true)
    [ -n "$p" ] && [ -d "/proc/$p" ] && echo "$p"
    return 0
}

for _ in $(seq 1 20); do
    is_running && break
    sleep 1
done

if ! is_running; then
    if [ "$INIT" = systemd ]; then
        journalctl -u "$SERVICE" -n 30 --no-pager >&2 || true
    else
        tail -n 30 "/var/log/$SERVICE.log" >&2 2>/dev/null || true
    fi
    die "面板没起来，日志在上面"
fi

echo
say "安装完成"
echo "  面板地址: $URL"
echo "  安装目录: $DIR"
echo "  运行身份: $RUN_USER（非 root）"
# 有没有建过管理员，问面板自己 —— 比翻数据库或猜 unit 文件都准
setup_needed=$(fetch_out "$URL/api/v1/auth/setup" 5 2>/dev/null | grep -o 'true' || echo "")
if [ -n "$setup_needed" ]; then
    echo
    echo "  ${YEL}下一步：打开面板设置管理员账号${RST}"
    echo "    $URL"
    echo "  用户名和密码由你在页面上自行设定，安装脚本不生成、也不保存密码。"
    echo "  ${YEL}请立刻完成${RST} —— 在设置好之前，任何能打开该地址的人都能抢先创建管理员。"
fi
echo
if [ "$INIT" = systemd ]; then
    echo "  查看日志: journalctl -u $SERVICE -f"
else
    echo "  查看日志: tail -f /var/log/$SERVICE.log"
    echo "  服务状态: rc-service $SERVICE status"
fi
echo "  卸载    : 用同样的方式重跑本脚本，加 --uninstall"
if [ -z "$TLS_CERT" ] && ! printf '%s' "$URL" | grep -q '^https://'; then
    echo
    warn "面板走的是明文 HTTP。探针的 token 放在握手头里，明文链路上谁都能拿到。"
    warn "  配 --tls-cert/--tls-key，或者放到反代后面并用 --url 指定 https 地址。"
fi
