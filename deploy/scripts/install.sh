#!/bin/sh
# ---------------------------------------------------------------------------
# Pulse 探针安装脚本。
#
# 需要 root（写服务定义、建专用用户），但**装完之后 agent 以非特权用户运行**。
# 支持 systemd 与 OpenRC（Alpine）。
# 这个区别很重要，
#
# 这个脚本用 root 权限只做四件事：
#   1. 建专用系统用户 pulse（非登录）
#   2. 下载并校验二进制，放到 /var/lib/pulse-agent/bin/
#   3. 写 systemd unit（或 OpenRC 的 /etc/init.d 脚本）与 /etc/pulse-agent/env（0600）
#   4. enable + start 服务
# 它**不会**：改 sysctl、改防火墙、装额外的包、动 .bashrc、上传任何信息。
#
# 用法（U=https://面板地址/install.sh；Debian 系与 Alpine 通用）：
#   安装：(curl -fsSL $U || wget -qO- $U) | $(command -v sudo) sh -s -- --server wss://面板 --token XXX
#   升级：(curl -fsSL $U || wget -qO- $U) | $(command -v sudo) sh -s --
#         已装过的机器**不带参数**重跑即可：沿用 /etc/pulse-agent/env 里的 token 与配置，
#         下载最新版。指定版本用 --version 0.0.4。
#   卸载：(curl -fsSL $U || wget -qO- $U) | $(command -v sudo) sh -s -- --uninstall
# ---------------------------------------------------------------------------
set -eu

# 探针版本。留空 = 装 Release 里的最新版。
#
# 原先这里写死 "0.0.1"：新装的机器拿到的永远是最老的版本，
# 已装的机器重跑安装命令也还是 0.0.1 —— 「升级」实际上从来没生效过。
VERSION="${PULSE_AGENT_VERSION:-}"
AGENT_REPO="${PULSE_AGENT_REPO:-pulse-monitor/pulse-agent}"
SERVICE="pulse-agent"
RUN_USER="pulse"
STATE_DIR="/var/lib/pulse-agent"
CONF_DIR="/etc/pulse-agent"
BIN_DIR="$STATE_DIR/bin"

# 默认值，可被参数覆盖
SERVER=""
TOKEN=""
INSTALL_DIR="/opt/pulse-agent"
INTERVAL="2"
NET_INCLUDE=""
NET_EXCLUDE=""
DISABLE_AUTO_UPDATE=0
ENABLE_GPU=0
UNINSTALL=0
# 是否显式给了运行期选项。没给、也没给 --server/--token 时，升级会原样保留现有配置
CONF_FLAGS=0
# 探针二进制在**探针仓库**的 Release 里，面板仓库不发探针
DOWNLOAD_BASE="${PULSE_DOWNLOAD_BASE:-https://github.com/pulse-monitor/pulse-agent/releases/download}"
# agent 自更新的下载源。留空 = 自更新关闭（capabilities.self_update 如实报 false）。
UPDATE_BASE="${PULSE_UPDATE_BASE:-}"
CA_CERT="${PULSE_CA_CERT:-}"

RED=''; GRN=''; YEL=''; RST=''
if [ -t 1 ]; then RED=$(printf '\033[31m'); GRN=$(printf '\033[32m'); YEL=$(printf '\033[33m'); RST=$(printf '\033[0m'); fi
say() { printf '%s==>%s %s\n' "$GRN" "$RST" "$1"; }
warn() { printf '%s警告:%s %s\n' "$YEL" "$RST" "$1" >&2; }
die() { printf '%s错误:%s %s\n' "$RED" "$RST" "$1" >&2; exit 1; }

# 错误不吞：任何一步失败都要留下痕迹，而不是静默退出。
# （不打行号 —— trap 里的 $LINENO 是 trap 自己的位置，反而误导）
TMP=""
# trap 处理函数：shellcheck 静态看不到调用点，会把函数本身报成「未被调用」
# （SC2329）、把函数体报成「不可达」（SC2317）。调用点是下面的
# `trap on_exit EXIT`。
# shellcheck disable=SC2329,SC2317
on_exit() {
    st=$?
    [ -n "$TMP" ] && rm -rf "$TMP"
    if [ "$st" -ne 0 ]; then
        printf '%s安装未完成%s（退出码 %s）。系统未被改动或已回到可重跑的状态。\n' \
            "$RED" "$RST" "$st" >&2
    fi
    exit "$st"
}
trap on_exit EXIT

# ── 参数 ───────────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
    case "$1" in
        --server)       SERVER="${2:?--server 需要一个值}"; shift 2 ;;
        --token)        TOKEN="${2:?--token 需要一个值}"; shift 2 ;;
        --install-dir)  INSTALL_DIR="${2:?}"; shift 2 ;;
        --version)      VERSION="${2:?--version 需要一个值，如 0.0.4}"; shift 2 ;;
        --interval)     INTERVAL="${2:?}"; CONF_FLAGS=1; shift 2 ;;
        --net-include)  NET_INCLUDE="${2:?}"; CONF_FLAGS=1; shift 2 ;;
        --net-exclude)  NET_EXCLUDE="${2:?}"; CONF_FLAGS=1; shift 2 ;;
        --disable-auto-update) DISABLE_AUTO_UPDATE=1; CONF_FLAGS=1; shift ;;
        --enable-gpu)   ENABLE_GPU=1; CONF_FLAGS=1; shift ;;
        # 自建镜像源时用。**必须是参数而不能只靠 PULSE_DOWNLOAD_BASE** ——
        # 安装命令是 `curl … | sudo bash` 的形式，sudo 默认会清掉环境变量，
        # 环境变量那条路在真机上根本走不通（实测踩过）。
        --download-base) DOWNLOAD_BASE="${2:?}"; shift 2 ;;
        # 自更新的下载源。不设则 agent 的自更新不可用，面板会如实显示。
        --update-base)  UPDATE_BASE="${2:?}"; CONF_FLAGS=1; shift 2 ;;
        # 面板用私有 CA / 自签证书时，额外信任的根证书（PEM）。
        # 只是往信任集里**加**一条，不会放松其它连接的校验。
        --ca-cert)      CA_CERT="${2:?}"; CONF_FLAGS=1; shift 2 ;;
        --uninstall)    UNINSTALL=1; shift ;;
        -h|--help)
            sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
            exit 0 ;;
        *) die "未知参数: $1（用 --help 看用法）" ;;
    esac
done

[ "$(id -u)" -eq 0 ] || die "需要 root 权限。请用 root 或 sudo/doas 运行 —— 但装完之后 agent 是以非特权用户 $RUN_USER 跑的。"

# CA 证书路径当场校验。等到探针起来连不上才报「读不到 CA 证书」，
# 用户还得去翻 journalctl —— 装的时候就该拦住。
if [ -n "$CA_CERT" ]; then
    [ -f "$CA_CERT" ] || die "--ca-cert 指向的文件不存在: $CA_CERT"
    grep -q "BEGIN CERTIFICATE" "$CA_CERT" 2>/dev/null \
        || die "--ca-cert 不像是 PEM 证书（找不到 BEGIN CERTIFICATE）: $CA_CERT"
fi

# ── 检测优于假设：init 系统 ─────────────────────────────────────────────────
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

# ── 卸载（幂等）─────────────────────────────────────────────────────────────
if [ "$UNINSTALL" -eq 1 ]; then
    say "卸载 $SERVICE"
    if [ "$INIT" = systemd ]; then
        systemctl stop "$SERVICE" 2>/dev/null || true
        systemctl disable "$SERVICE" 2>/dev/null || true
        rm -f "/etc/systemd/system/$SERVICE.service"
        systemctl daemon-reload 2>/dev/null || true
    elif [ "$INIT" = openrc ]; then
        rc-service "$SERVICE" stop 2>/dev/null || true
        rc-update del "$SERVICE" default 2>/dev/null || true
        rm -f "/etc/init.d/$SERVICE" "/var/log/$SERVICE.log"
    fi
    rm -rf "$STATE_DIR" "$CONF_DIR" "$INSTALL_DIR"
    if id "$RUN_USER" >/dev/null 2>&1; then
        # userdel 来自 shadow 套件，Alpine 上没有 —— busybox 提供的是 deluser。
        # 只写 userdel 的话卸载会留下一个用不掉的用户，而且不报错（实测踩到）。
        userdel "$RUN_USER" 2>/dev/null \
          || deluser "$RUN_USER" 2>/dev/null \
          || warn "删除用户 $RUN_USER 失败，可手动清理"
        delgroup "$RUN_USER" 2>/dev/null || groupdel "$RUN_USER" 2>/dev/null || true
    fi
    say "已卸载。系统中不应再有残留 —— 可用 'id $RUN_USER' 与 'ls $STATE_DIR' 确认。"
    trap - EXIT
    exit 0
fi

# ── 升级：没给 --server / --token 就沿用这台机器现有的配置 ──────────────────
# token 只在生成安装命令时显示一次，而且面板上重新生成会让旧 token 当场作废
# （机器先掉线）。只是想升级探针的话，不该逼人去面板再要一把新钥匙。
# 什么都没给 → 配置文件原样保留（连同 --update-base、--ca-cert 这些当初的选项）；
# 给了任何一项 → 以给的为准，缺的 server/token 从现有配置补。
REUSE_CONF=0
if [ -r "$CONF_DIR/env" ]; then
    [ -z "$SERVER" ] && [ -z "$TOKEN" ] && [ "$CONF_FLAGS" -eq 0 ] && REUSE_CONF=1
    [ -n "$SERVER" ] || SERVER=$(sed -n 's/^PULSE_SERVER=//p' "$CONF_DIR/env" | tail -1)
    [ -n "$TOKEN" ]  || TOKEN=$(sed -n 's/^PULSE_TOKEN=//p' "$CONF_DIR/env" | tail -1)
fi
[ -n "$SERVER" ] || die "缺少 --server（首次安装需要；已装过的机器不带参数重跑就是升级）"
[ -n "$TOKEN" ]  || die "缺少 --token"
case "$INIT" in
    systemd|openrc) ;;
    *) die "不支持的 init（检测到: $INIT）。目前支持 systemd 与 OpenRC。" ;;
esac

# ── 平台 ───────────────────────────────────────────────────────────────────
case "$(uname -m)" in
    x86_64|amd64)  ARCH=x86_64 ;;
    aarch64|arm64) ARCH=aarch64 ;;
    *) die "不支持的架构: $(uname -m)" ;;
esac
ASSET="pulse-agent-${ARCH}-unknown-linux-musl"
say "平台: linux/$ARCH，init: $INIT"

# ── 下载工具（原生 Alpine 没有 curl，只有 busybox 的 wget）─────────────────
fetch() {
    if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then wget -qO "$2" "$1"
    else die "需要 curl 或 wget"; fi
}
fetch_out() {
    if command -v curl >/dev/null 2>&1; then curl -fsSL "$1"
    elif command -v wget >/dev/null 2>&1; then wget -qO- "$1"
    else die "需要 curl 或 wget"; fi
}

# ── 版本：没指定就取最新。在动系统之前定下来，查不到就什么都不改 ──────────────
if [ -z "$VERSION" ]; then
    VERSION=$(fetch_out "https://api.github.com/repos/$AGENT_REPO/releases/latest" 2>/dev/null \
        | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -1)
    [ -n "$VERSION" ] || die "查不到探针的最新版本（GitHub API 不通或被限流）。用 --version 指定，如 --version 0.0.4"
fi
VERSION=${VERSION#v}
say "探针版本: v$VERSION"

# ── 幂等：已安装则走升级路径 ────────────────────────────────────────────────
UPGRADE=0
if [ -x "$BIN_DIR/pulse-agent" ]; then
    UPGRADE=1
    say "检测到已安装，走升级路径（保留现有配置）"
fi

# ── 用户（幂等）─────────────────────────────────────────────────────────────
if id "$RUN_USER" >/dev/null 2>&1; then
    say "用户 $RUN_USER 已存在"
else
    say "创建非登录系统用户 $RUN_USER"
    # nologin 的路径各发行版不一样：Debian 系在 /usr/sbin，Alpine 在 /sbin
    NOLOGIN=$(command -v nologin 2>/dev/null || true)
    if [ -z "$NOLOGIN" ]; then
        for c in /usr/sbin/nologin /sbin/nologin /bin/false; do
            [ -x "$c" ] && { NOLOGIN=$c; break; }
        done
    fi

    if command -v useradd >/dev/null 2>&1; then
        # shadow 套件（Debian / RHEL / Arch…）
        useradd --system --no-create-home --shell "$NOLOGIN" \
          --home-dir "$STATE_DIR" "$RUN_USER" || die "创建用户失败"
    elif adduser --help 2>&1 | grep -q -- '-S'; then
        # busybox 的 adduser（Alpine）。**参数语法和 Debian 版完全不同** ——
        # 没有 --system / --no-create-home 这些长选项，照搬会直接报错。
        addgroup -S "$RUN_USER" 2>/dev/null || true
        adduser -S -H -s "$NOLOGIN" -h "$STATE_DIR" -G "$RUN_USER" "$RUN_USER" \
          || die "创建用户失败"
    else
        # Debian 的 adduser（perl 脚本版）
        # 必须带 --group：不带的话用户会被放进 nogroup，下面的 install -g 就找不到组
        adduser --system --group --no-create-home --shell "$NOLOGIN" \
          --home "$STATE_DIR" "$RUN_USER" || die "创建用户失败"
    fi
fi

install -d -m 0750 -o "$RUN_USER" -g "$RUN_USER" "$STATE_DIR" "$BIN_DIR"
install -d -m 0750 "$CONF_DIR"

# ── 下载并校验 ─────────────────────────────────────────────────────────────
TMP=$(mktemp -d)

say "下载 $ASSET"
fetch "$DOWNLOAD_BASE/v$VERSION/$ASSET" "$TMP/agent" \
  || die "下载失败: $DOWNLOAD_BASE/v$VERSION/$ASSET"

# 校验 SHA-256。清单缺失时**明确拒绝**而不是静默跳过 ——
# 「校验不了」和「校验通过」是两回事。
if fetch "$DOWNLOAD_BASE/v$VERSION/SHA256SUMS" "$TMP/sums" 2>/dev/null; then
    WANT=$(grep " $ASSET\$" "$TMP/sums" | awk '{print $1}' || true)
    if [ -n "$WANT" ]; then
        if command -v sha256sum >/dev/null 2>&1; then GOT=$(sha256sum "$TMP/agent" | awk '{print $1}')
        elif command -v shasum   >/dev/null 2>&1; then GOT=$(shasum -a 256 "$TMP/agent" | awk '{print $1}')
        else GOT=""; warn "没有 sha256sum/shasum，跳过校验"; fi
        if [ -n "$GOT" ] && [ "$GOT" != "$WANT" ]; then
            die "SHA-256 不匹配！期望 $WANT，实际 $GOT。已中止，不会安装这个文件。"
        fi
        [ -n "$GOT" ] && say "SHA-256 校验通过"
    else
        warn "清单里没有 $ASSET 的条目，跳过校验"
    fi
else
    warn "取不到 SHA256SUMS，跳过校验（生产环境不该出现这种情况）"
fi

if [ -f "$BIN_DIR/pulse-agent" ] && cmp -s "$TMP/agent" "$BIN_DIR/pulse-agent" 2>/dev/null; then
    say "已经是 v$VERSION，二进制没有变化（服务定义照样刷新并重启）"
fi

# ── 危险操作前备份 ─────────────────────────────────────────────────────────
STAMP=$(date +%Y%m%d%H%M%S)
if [ -f "$BIN_DIR/pulse-agent" ]; then
    cp -p "$BIN_DIR/pulse-agent" "$BIN_DIR/pulse-agent.bak.$STAMP"
    say "已备份旧二进制: pulse-agent.bak.$STAMP"
    # 只留最近 3 份。每次重装都留一个 2.3 MB 的备份且从不清理的话，
    # 反复升级的机器上会攒出一堆没人会去看的旧二进制（真机实测攒到过 2 份）。
    # 文件名里的 STAMP 是 %Y%m%d%H%M%S，按名字倒序就是按时间倒序 ——
    # 不必去解析 ls 的输出（文件名里有空格时那是不安全的）
    printf '%s\n' "$BIN_DIR"/pulse-agent.bak.* | sort -r | tail -n +4 | while read -r old; do
        # glob 没匹配到东西时 printf 会原样吐出这个模式，挡一下
        [ -e "$old" ] || continue
        rm -f "$old" && say "清理过期备份: $(basename "$old")"
    done
fi
if [ -f "$CONF_DIR/env" ]; then
    cp -p "$CONF_DIR/env" "$CONF_DIR/env.bak.$STAMP"
    say "已备份旧配置: env.bak.$STAMP"
fi

install -m 0755 -o "$RUN_USER" -g "$RUN_USER" "$TMP/agent" "$BIN_DIR/pulse-agent"

# ── 配置。token 写文件不进命令行 —— 命令行对同机任何用户 ps 可见 ──────────
umask 077
if [ "$REUSE_CONF" -eq 1 ]; then
    say "沿用现有配置 $CONF_DIR/env（token 与当初的选项都不变）"
else
{
    echo "PULSE_SERVER=$SERVER"
    echo "PULSE_TOKEN=$TOKEN"
    echo "PULSE_LOG=info"
    [ "$DISABLE_AUTO_UPDATE" -eq 1 ] && echo "PULSE_AUTO_UPDATE=0"
    [ "$ENABLE_GPU" -eq 1 ] && echo "PULSE_GPU=1"
    [ -n "$NET_INCLUDE" ] && echo "PULSE_NET_INCLUDE=$NET_INCLUDE"
    [ -n "$NET_EXCLUDE" ] && echo "PULSE_NET_EXCLUDE=$NET_EXCLUDE"
    [ -n "$UPDATE_BASE" ] && echo "PULSE_UPDATE_BASE=$UPDATE_BASE"
    [ -n "$CA_CERT" ] && echo "PULSE_CA_CERT=$CA_CERT"
    echo "PULSE_INTERVAL=$INTERVAL"
} > "$CONF_DIR/env"
fi
chmod 0600 "$CONF_DIR/env"

# 探针以非特权用户运行，读不到 CA 证书一样连不上。装完当场验一次，
# 别让用户到运行期才发现（root 能读不代表 agent 能读）。
if [ -n "$CA_CERT" ] && ! su -s /bin/sh -c "test -r '$CA_CERT'" "$RUN_USER" 2>/dev/null; then
    warn "CA 证书 $CA_CERT 对运行用户 $RUN_USER 不可读，探针会连不上。"
    warn "  修：chmod a+r '$CA_CERT'（CA 证书是公开信息，可读没有风险）"
fi
chown root:"$RUN_USER" "$CONF_DIR/env" 2>/dev/null || true

# 上面这些运行期选项只是**首次配置**的冗余：真正的来源是面板数据库，
# agent 连上后会立刻收到一份下发的配置并以它为准。

# ── 服务定义 ───────────────────────────────────────────────────────────────
if [ "$INIT" = systemd ]; then
# 每条硬化指令的理由改动前请先读那一篇：
# ProtectHome / ProcSubset / ProtectProc 三条改错会**静默地**弄坏采集。
cat > "/etc/systemd/system/$SERVICE.service" <<UNIT
[Unit]
Description=Pulse monitoring agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$BIN_DIR/pulse-agent
EnvironmentFile=$CONF_DIR/env
Restart=always
RestartSec=5s
StartLimitIntervalSec=600
StartLimitBurst=5

User=$RUN_USER
Group=$RUN_USER
StateDirectory=pulse-agent
StateDirectoryMode=0750

CapabilityBoundingSet=
AmbientCapabilities=
NoNewPrivileges=yes
# 真机实测发现的两处白捡分（systemd-analyze security 2.1 → 1.9）：
# 探针创建的文件不该 world-readable，也不用 SysV IPC。
# 两条都不影响任何采集功能。
UMask=0077
RemoveIPC=yes

ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=yes
ReadWritePaths=$STATE_DIR

ProcSubset=all
ProtectProc=default

ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
RestrictNamespaces=yes
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallErrorNumber=EPERM
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK

[Install]
WantedBy=multi-user.target
UNIT

    systemctl daemon-reload
    systemctl enable "$SERVICE" >/dev/null 2>&1 || true
    systemctl restart "$SERVICE"
else
    # OpenRC（Alpine 等）。
    #
    # 说明白一点：**OpenRC 给不了 systemd 那套硬化**（ProtectSystem、
    # SystemCallFilter、CapabilityBoundingSet 这些都是 systemd 特有的）。
    # 这里能保证的还是最要紧的那条 —— **以非 root 专用用户运行**，
    # 其余的命名空间与系统调用限制在 OpenRC 下没有等价物。
    #
    # supervise-daemon 而不是 start-stop-daemon：前者会盯着进程、
    # 挂了自动拉起，等价于 systemd 的 Restart=always。
cat > "/etc/init.d/$SERVICE" <<'RCSCRIPT'
#!/sbin/openrc-run

name="pulse-agent"
description="Pulse monitoring agent"

: ${cfgfile:=/etc/pulse-agent/env}

command="__BIN_DIR__/pulse-agent"
command_user="__RUN_USER__:__RUN_USER__"
command_background=false
supervisor=supervise-daemon
respawn_delay=5
respawn_max=0
# 这两条 OpenRC 给得了，对应 systemd 的 NoNewPrivileges=yes 与 UMask=0077
no_new_privs=yes
umask=0077
pidfile="/run/${RC_SVCNAME}.pid"
output_log="/var/log/${RC_SVCNAME}.log"
error_log="/var/log/${RC_SVCNAME}.log"

depend() {
    need net
    after firewall
}

# 环境变量必须在**脚本顶层**读，不能放进 start_pre。
#
# start_pre 跑在另一个 shell 里，它 export 的变量**不会**传给
# supervise-daemon 起的子进程 —— 实测过：start_pre 里 export MYVAR=x，
# 子进程拿到的是空的。结果就是 agent 用默认的 ws://127.0.0.1:25774 去连，
# 401 之后不断重连，而 rc-service status 显示 "started"（supervise-daemon
# 自己活着），从外面完全看不出哪里错了。
#
# OpenRC 没有 systemd 的 EnvironmentFile，顶层 source 是唯一可靠的做法。
if [ -r "$cfgfile" ]; then
    set -a
    # shellcheck disable=SC1090
    . "$cfgfile"
    set +a
fi

start_pre() {
    checkpath -d -m 0750 -o __RUN_USER__:__RUN_USER__ "__STATE_DIR__"
    checkpath -f -m 0640 -o __RUN_USER__:__RUN_USER__ "/var/log/${RC_SVCNAME}.log"
    if [ ! -r "$cfgfile" ]; then
        eerror "配置文件不存在或不可读: $cfgfile"
        return 1
    fi
    if [ -z "$PULSE_TOKEN" ]; then
        eerror "$cfgfile 里没有 PULSE_TOKEN"
        return 1
    fi
}
RCSCRIPT
    # 上面用了带引号的 heredoc（避免 $RC_SVCNAME 这类被提前展开），
    # 所以我们自己的变量要在这里替换回去
    sed -i \
        -e "s|__BIN_DIR__|$BIN_DIR|g" \
        -e "s|__RUN_USER__|$RUN_USER|g" \
        -e "s|__STATE_DIR__|$STATE_DIR|g" \
        "/etc/init.d/$SERVICE"
    chmod 0755 "/etc/init.d/$SERVICE"

    rc-update add "$SERVICE" default >/dev/null 2>&1 || true
    rc-service "$SERVICE" restart
fi


# ── 副作用后验证：轮询到 active，失败时自动打诊断 ─────────────────────────
say "等待服务启动…"
# 探测「服务是否已在运行」。两套 init 的问法不一样，抽出来免得下面重复判断。
is_running() {
    if [ "$INIT" = systemd ]; then
        systemctl is-active --quiet "$SERVICE"
    else
        # 只看 status 不够：supervise-daemon 自己活着就报 started，哪怕 agent
        # 一起来就崩、正在 respawn_delay 里等重启。所以还要确认 agent 进程在，
        # 且隔两秒还是同一个 pid —— 没有陷在崩溃循环里。
        rc-service "$SERVICE" status 2>/dev/null | grep -q started || return 1
        p1=$(agent_pid)
        [ -n "$p1" ] || return 1
        sleep 2
        [ "$(agent_pid)" = "$p1" ]
    fi
}

# supervise-daemon 把被监管进程的 pid 记在 OpenRC 的服务状态里
agent_pid() {
    p=$(cat "/run/openrc/options/$SERVICE/child_pid" 2>/dev/null || true)
    [ -n "$p" ] && [ -d "/proc/$p" ] && echo "$p"
    return 0
}

i=0
while [ "$i" -lt 30 ]; do
    if is_running; then
        rm -rf "$TMP"; TMP=""; trap - EXIT
        printf '\n%s✔ 安装完成%s\n' "$GRN" "$RST"
        [ "$UPGRADE" -eq 1 ] && printf '  （升级到 v%s，已备份原二进制与配置）\n' "$VERSION"
        [ "$UPGRADE" -eq 0 ] && printf '  版本    : v%s\n' "$VERSION"
        printf '  运行身份: %s（非 root）\n' "$RUN_USER"
        printf '  二进制  : %s\n' "$BIN_DIR/pulse-agent"
        printf '  配置    : %s (0600)\n' "$CONF_DIR/env"
        if [ "$INIT" = systemd ]; then
            printf '\n  查看日志: journalctl -u %s -f\n' "$SERVICE"
            printf '  加固评分: systemd-analyze security %s\n' "$SERVICE"
        else
            printf '\n  查看日志: tail -f /var/log/%s.log\n' "$SERVICE"
            printf '  服务状态: rc-service %s status\n' "$SERVICE"
            # 说清楚代价，别让人以为两边一样安全
            printf '  %s注意%s: OpenRC 没有 systemd 那套沙箱（ProtectSystem / SystemCallFilter\n' "$YEL" "$RST"
            printf '         等都是 systemd 特有）。这里保证的是**非 root 专用用户运行**，\n'
            printf '         其余的命名空间与系统调用限制在 OpenRC 下没有等价物。\n'
        fi
        # Alpine 默认没有 iproute2 的 ss，busybox 的 netstat 能做同样的事
        if command -v ss >/dev/null 2>&1; then
            printf '  确认无端口: ss -lntp | grep pulse   （应无输出）\n'
        else
            printf '  确认无端口: netstat -lntp | grep pulse   （应无输出）\n'
        fi
        # 用管道装的机器上**没有 install.sh 这个文件**，原先提示的
        # `sh install.sh --uninstall` 照着敲只会报找不到文件。给能直接复制的完整命令。
        case "$SERVER" in
            wss://*) PANEL="https://${SERVER#wss://}" ;;
            ws://*)  PANEL="http://${SERVER#ws://}" ;;
            *)       PANEL="$SERVER" ;;
        esac
        U="${PANEL%/}/install.sh"
        # shellcheck disable=SC2016
        printf '\n  升级    : (curl -fsSL %s || wget -qO- %s) | $(command -v sudo) sh -s --\n' "$U" "$U"
        # shellcheck disable=SC2016
        printf '  卸载    : (curl -fsSL %s || wget -qO- %s) | $(command -v sudo) sh -s -- --uninstall\n' "$U" "$U"
        exit 0
    fi
    i=$((i + 1))
    sleep 1
done

# 失败时把诊断直接打出来，不要让用户自己去翻
printf '\n%s服务在 30 秒内没有进入 active 状态。%s\n' "$RED" "$RST" >&2
printf '最近的日志：\n' >&2
if [ "$INIT" = systemd ]; then
    journalctl -u "$SERVICE" -n 30 --no-pager >&2 || true
else
    tail -n 30 "/var/log/$SERVICE.log" >&2 2>/dev/null || rc-service "$SERVICE" status >&2 2>/dev/null || true
fi
exit 1
