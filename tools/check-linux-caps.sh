#!/bin/sh
# ---------------------------------------------------------------------------
# Pulse 探针 · Linux 非 root 采集能力实测脚本
#
# 用途：逐项验证「承重假设 A2 —— 全部指标都能在非特权用户下采到」。
#      在你的真实 VPS 上运行，它会打印每个指标的实际可读性与样本值。
#
# 用法：
#   1) 普通非 root 用户直接跑（最快，覆盖 90% 情况）：
#        curl -fsSL <url>/check-linux-caps.sh -o /tmp/c.sh && sh /tmp/c.sh
#
#   2) 在真实 systemd 沙箱里跑（金标准，会连硬化配置一起验）：
#        sh /tmp/c.sh --print-sandbox-cmd     # 打印命令，自己看过再执行
#
# 本脚本只读，不写任何文件，不改任何配置，不需要 root。
# ---------------------------------------------------------------------------
set -u

RED=''; GRN=''; YEL=''; DIM=''; RST=''
if [ -t 1 ] && [ "$(tput colors 2>/dev/null || echo 0)" -ge 8 ] 2>/dev/null; then
  RED=$(printf '\033[31m'); GRN=$(printf '\033[32m'); YEL=$(printf '\033[33m')
  DIM=$(printf '\033[2m');  RST=$(printf '\033[0m')
fi

N_OK=0; N_NO=0; N_WARN=0
# 不做列对齐：中文是双宽字符，awk/printf 的按字节补位在 busybox/mawk/BSD awk 上
# 行为不一致，为了排版引入可移植性 bug 不划算。用分隔符代替。
ok()   { N_OK=$((N_OK+1));     printf '  %s✔%s %s%s\n' "$GRN" "$RST" "$1" "${2:+  ·  $2}"; }
no()   { N_NO=$((N_NO+1));     printf '  %s✘%s %s%s\n' "$RED" "$RST" "$1" "${2:+  ·  $2}"; }
warn() { N_WARN=$((N_WARN+1)); printf '  %s!%s %s%s\n' "$YEL" "$RST" "$1" "${2:+  ·  $2}"; }
note() { printf '    %s%s%s\n' "$DIM" "$1" "$RST"; }
hdr()  { printf '\n%s── %s %s\n' "$DIM" "$1" "$RST"; }

# 读一个文件的首行样本，成功返回 0
peek() { head -n1 "$1" 2>/dev/null | cut -c1-58; }

# check <标签> <路径> [required|optional]
check() {
  _label=$1; _path=$2; _req=${3:-required}
  if [ ! -e "$_path" ]; then
    if [ "$_req" = required ]; then no "$_label" "路径不存在: $_path"
    else warn "$_label" "路径不存在（本机无此特性）: $_path"; fi
    return 1
  fi
  _s=$(peek "$_path")
  if [ -n "$_s" ]; then ok "$_label" "$_s"; return 0
  else
    if [ "$_req" = required ]; then no "$_label" "存在但读不到内容（权限？）: $_path"
    else warn "$_label" "存在但为空: $_path"; fi
    return 1
  fi
}

# ── 沙箱命令 ───────────────────────────────────────────────────────────────
if [ "${1:-}" = "--print-sandbox-cmd" ]; then
  cat <<'SANDBOX'
在 systemd 硬化沙箱里跑同一个脚本（金标准）。先看一眼再执行：

sudo systemd-run --pipe --wait --collect \
  -p DynamicUser=yes \
  -p CapabilityBoundingSet= \
  -p AmbientCapabilities= \
  -p NoNewPrivileges=yes \
  -p ProtectSystem=strict \
  -p ProtectHome=read-only \
  -p PrivateTmp=yes \
  -p ProtectProc=default \
  -p ProcSubset=all \
  -p ProtectKernelTunables=yes \
  -p ProtectKernelModules=yes \
  -p ProtectKernelLogs=yes \
  -p ProtectControlGroups=yes \
  -p ProtectClock=yes \
  -p ProtectHostname=yes \
  -p LockPersonality=yes \
  -p MemoryDenyWriteExecute=yes \
  -p RestrictRealtime=yes \
  -p RestrictSUIDSGID=yes \
  -p RestrictNamespaces=yes \
  -p SystemCallArchitectures=native \
  -p SystemCallFilter=@system-service \
  -p RestrictAddressFamilies="AF_INET AF_INET6 AF_UNIX AF_NETLINK" \
  /bin/sh /tmp/c.sh

说明：这里用 DynamicUser=yes 是**最严格**的情形（随机 UID/GID）。生产默认用的是固定用户
pulse，比这更宽松 —— 所以这里能过的，固定用户下一定能过。唯一会不同的是第 8 项
「非特权 ICMP」：它依赖 GID 是否落在 ping_group_range 内，固定用户可以事先配好，
随机 GID 不行。这一项以固定用户下的结果为准。

两次结果如有差异，差的那几项就是被硬化配置挡住的 —— 那才是真正要改的地方。
SANDBOX
  exit 0
fi

# ── 环境 ───────────────────────────────────────────────────────────────────
printf '%s\n' "=========================================================="
printf '  Pulse 探针 · 非 root 采集能力实测\n'
printf '%s\n' "=========================================================="
printf '  用户    : %s (uid=%s gid=%s)\n' "$(id -un)" "$(id -u)" "$(id -g)"
printf '  内核    : %s\n' "$(uname -srm)"
printf '  发行版  : %s\n' "$( . /etc/os-release 2>/dev/null && echo "$PRETTY_NAME" || echo unknown )"
printf '  时间    : %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
[ "$(id -u)" -eq 0 ] && printf '\n  %s警告：你正在以 root 运行，这测不出非 root 的真实情况。%s\n' "$YEL" "$RST"
[ "$(id -u)" -eq 0 ] && printf '  %s请改用普通用户，或用 --print-sandbox-cmd 的沙箱方式。%s\n' "$YEL" "$RST"

# ── 1. CPU / 负载 / 内存 ───────────────────────────────────────────────────
hdr "1. CPU / 负载 / 内存"
check "CPU 时间片"        /proc/stat
check "负载"              /proc/loadavg
check "CPU 型号"          /proc/cpuinfo
if [ -r /proc/meminfo ]; then
  _mt=$(awk '/^MemTotal:/{print $2}'     /proc/meminfo)
  _ma=$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)
  _mf=$(awk '/^MemFree:/{print $2}'      /proc/meminfo)
  _sw=$(awk '/^SwapTotal:/{print $2}'    /proc/meminfo)
  ok "内存 MemTotal"     "$((_mt/1024)) MiB"
  if [ -n "$_ma" ]; then ok "内存 MemAvailable" "$((_ma/1024)) MiB  ← 「不含缓冲」口径依赖此项"
  else no "内存 MemAvailable" "内核过旧（<3.14），需退化为 free+buffers+cached 估算"; fi
  ok "内存 MemFree"      "$((_mf/1024)) MiB  ← 「含缓冲」口径依赖此项"
  ok "Swap"              "$((_sw/1024)) MiB"
else
  no "内存" "/proc/meminfo 不可读"
fi

# ── 2. 磁盘 ────────────────────────────────────────────────────────────────
hdr "2. 磁盘（statvfs 需要对挂载点有搜索权限）"
check "挂载表"            /proc/mounts
check "磁盘 IO 计数"      /proc/diskstats optional
if [ -r /proc/mounts ]; then
  # 只看真实块设备文件系统，跳过 tmpfs/overlay/proc 等
  awk '$3 ~ /^(ext[234]|xfs|btrfs|zfs|f2fs|reiserfs|jfs|vfat|ntfs3?)$/ {print $2}' /proc/mounts 2>/dev/null \
  | while read -r _m; do
      if df -P -k "$_m" >/dev/null 2>&1; then
        printf '  %s✔%s 可 statvfs  ·  %s\n' "$GRN" "$RST" "$_m"
      else
        printf '  %s✘%s 不可 statvfs  ·  %s（权限不足或不可搜索）\n' "$RED" "$RST" "$_m"
      fi
    done
  note "若某挂载点在 /home 或 /root 下，且 unit 用了 ProtectHome=yes，这里会失败 —— 应改为 read-only"
fi

# ── 3. 网络 ────────────────────────────────────────────────────────────────
hdr "3. 网络"
check "网卡累计计数"      /proc/net/dev
_if=$(awk -F: 'NR>2 && $1 !~ /lo/ {gsub(/ /,"",$1); print $1; exit}' /proc/net/dev 2>/dev/null)
if [ -n "${_if:-}" ] && [ -r "/sys/class/net/$_if/statistics/rx_bytes" ]; then
  ok "sysfs 网卡统计" "$_if rx=$(cat "/sys/class/net/$_if/statistics/rx_bytes")"
else
  warn "sysfs 网卡统计" "不可读，退化为只用 /proc/net/dev（够用）"
fi
check "TCP 连接（廉价）"  /proc/net/sockstat  optional
check "TCP 连接 v6"       /proc/net/sockstat6 optional
if [ -r /proc/net/tcp ]; then
  ok "TCP 连接（精确）" "$(( $(wc -l < /proc/net/tcp) - 1 )) 条 ipv4"
  note "大量连接时逐行读 /proc/net/tcp 很贵，优先用 /proc/net/sockstat 的 inuse 值"
else
  no "TCP 连接（精确）" "/proc/net/tcp 不可读（罕见，可能是加固内核）"
fi

# ── 4. 进程数（hidepid 是这里唯一的坑）─────────────────────────────────────
hdr "4. 进程数"
_hp=$(awk '$2=="/proc"{for(i=1;i<=NF;i++) if($i ~ /hidepid/) print $i}' /proc/mounts 2>/dev/null)
_cnt=$(ls -d /proc/[0-9]* 2>/dev/null | wc -l)
if [ -n "$_hp" ] && [ "$(id -u)" -ne 0 ]; then
  no "进程数" "$_cnt 个可见，但 /proc 挂了 $_hp → 非 root 只能看到自己的进程"
  note "对策：上报 capabilities.proc_count=false，前端隐藏该字段（不要显示错误的数字）"
elif [ ! -d /proc/1 ]; then
  no "进程数" "$_cnt 个可见，但看不到 PID 1 → 疑似 hidepid 或 ProtectProc=invisible"
else
  ok "进程数" "$_cnt 个（可见 PID 1，无 hidepid）"
fi

# ── 5. 运行时间 / 虚拟化 ───────────────────────────────────────────────────
hdr "5. 运行时间 / 虚拟化"
check "uptime"            /proc/uptime
[ -r /proc/stat ] && ok "开机时刻 btime" "$(awk '/^btime/{print $2}' /proc/stat)"
if [ -r /sys/class/dmi/id/product_name ]; then
  ok "DMI product_name" "$(cat /sys/class/dmi/id/product_name)"
  ok "DMI sys_vendor"   "$(cat /sys/class/dmi/id/sys_vendor 2>/dev/null || echo '(不可读)')"
  note "注意 product_serial / product_uuid 是 0400 只有 root 能读 —— 我们不需要它们"
else
  warn "DMI" "无 /sys/class/dmi（常见于 aarch64 VPS 与容器），改用 CPUID/cgroup 判断"
fi
if grep -q '^flags.*hypervisor' /proc/cpuinfo 2>/dev/null; then
  ok "CPUID hypervisor 位" "存在 → 虚拟机"
else
  warn "CPUID hypervisor 位" "不存在 → 物理机 或 非 x86"
fi
[ -f /.dockerenv ] && warn "容器" "检测到 /.dockerenv"
grep -qE '(docker|lxc|containerd|kubepods)' /proc/1/cgroup 2>/dev/null && warn "容器" "cgroup 显示在容器内"

# ── 6. 容器内的真实规格（关键：否则会显示宿主机的规格）─────────────────────
hdr "6. cgroup 限额（容器/LXC 里必须用它，否则读到的是宿主机规格）"
if [ -r /sys/fs/cgroup/cgroup.controllers ]; then
  ok "cgroup 版本" "v2 (unified)"
  check "内存上限 memory.max"  /sys/fs/cgroup/memory.max optional
  check "CPU 配额 cpu.max"     /sys/fs/cgroup/cpu.max    optional
elif [ -d /sys/fs/cgroup/memory ]; then
  ok "cgroup 版本" "v1"
  check "内存上限"  /sys/fs/cgroup/memory/memory.limit_in_bytes optional
  check "CPU 配额"  /sys/fs/cgroup/cpu/cpu.cfs_quota_us          optional
else
  warn "cgroup" "未挂载或不可见"
fi
if grep -q lxcfs /proc/mounts 2>/dev/null; then
  ok "lxcfs" "已挂载 → /proc/meminfo 与 /proc/stat 已是容器视角，可直接信任"
else
  warn "lxcfs" "未挂载 → 若这是 LXC/OpenVZ 容器，/proc/meminfo 显示的是宿主机内存！"
  note "对策：优先读 cgroup 限额，与 /proc/meminfo 取较小值"
fi

# ── 7. 温度 ────────────────────────────────────────────────────────────────
hdr "7. 温度（VPS 上通常本来就没有，与权限无关）"
_t=0
for f in /sys/class/hwmon/hwmon*/temp*_input; do
  [ -r "$f" ] || continue
  ok "hwmon 温度" "$f = $(( $(cat "$f") / 1000 ))°C"; _t=1; break
done
if [ "$_t" -eq 0 ]; then
  for f in /sys/class/thermal/thermal_zone*/temp; do
    [ -r "$f" ] || continue
    ok "thermal_zone 温度" "$f = $(( $(cat "$f") / 1000 ))°C"; _t=1; break
  done
fi
[ "$_t" -eq 0 ] && warn "温度" "本机无任何温度传感器（KVM/容器常见）→ capabilities.temperature=false"

# ── 8. 非特权 ICMP（决定延迟监控能否用 ping 而非 TCP）───────────────────────
hdr "8. 非特权 ICMP"
if [ -r /proc/sys/net/ipv4/ping_group_range ]; then
  _r=$(cat /proc/sys/net/ipv4/ping_group_range)
  _lo=$(echo "$_r" | awk '{print $1}'); _hi=$(echo "$_r" | awk '{print $2}')
  _hit=0
  for g in $(id -G); do
    [ "$g" -ge "$_lo" ] 2>/dev/null && [ "$g" -le "$_hi" ] 2>/dev/null && _hit=1
  done
  if [ "$_hit" -eq 1 ]; then
    ok "非特权 ICMP" "ping_group_range = $_r，当前用户在范围内 → 可用真 ICMP"
  else
    warn "非特权 ICMP" "ping_group_range = $_r，当前用户不在范围内 → 回落 TCP ping"
    note "如需启用（由你决定，探针不会自己改）："
    note "  echo 'net.ipv4.ping_group_range = 0 2147483647' | sudo tee /etc/sysctl.d/99-pulse.conf && sudo sysctl --system"
  fi
else
  warn "非特权 ICMP" "无法读取 ping_group_range → 回落 TCP ping"
fi

# ── 9. GPU ─────────────────────────────────────────────────────────────────
hdr "9. GPU（仅当你要开 GPU 监控时才需要关心）"
if [ -e /dev/nvidiactl ]; then
  _p=$(ls -l /dev/nvidiactl | awk '{print $1" "$3":"$4}')
  if [ -r /dev/nvidiactl ] && [ -w /dev/nvidiactl ]; then
    ok "NVIDIA 设备节点" "$_p → 非 root 可访问"
  else
    no "NVIDIA 设备节点" "$_p → 当前用户无权访问，NVML 会失败"
    note "对策：把运行用户加入设备所属组，或在 unit 里加 DeviceAllow=/dev/nvidiactl rw"
  fi
  for l in /usr/lib/x86_64-linux-gnu/libnvidia-ml.so.1 /usr/lib64/libnvidia-ml.so.1 /usr/lib/libnvidia-ml.so.1; do
    [ -e "$l" ] && { ok "libnvidia-ml" "$l"; break; }
  done
else
  warn "NVIDIA" "无 /dev/nvidiactl（本机无 N 卡或未装驱动）→ capabilities.gpu_nvml=false"
fi

# ── 10. 可选增强 ───────────────────────────────────────────────────────────
hdr "10. 可选增强指标"
check "PSI 压力指标"      /proc/pressure/cpu optional
check "文件描述符上限"    /proc/sys/fs/file-nr optional

# ── 汇总 ───────────────────────────────────────────────────────────────────
printf '\n%s\n' "=========================================================="
printf '  可用 %s%d%s   不可用 %s%d%s   降级/不适用 %s%d%s\n' \
  "$GRN" "$N_OK" "$RST" "$RED" "$N_NO" "$RST" "$YEL" "$N_WARN" "$RST"
printf '%s\n' "=========================================================="
if [ "$N_NO" -eq 0 ]; then
  printf '  %s结论：全部必需指标在非 root 下均可采集，A2 假设在本机成立。%s\n' "$GRN" "$RST"
  exit 0
else
  printf '  %s结论：有 %d 项必需指标不可用，需要按上面的「对策」降级处理。%s\n' "$RED" "$N_NO" "$RST"
  printf '  把完整输出贴回来，我按实际情况调整采集实现与 capabilities 上报。\n'
  exit 1
fi
