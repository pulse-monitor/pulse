#!/bin/sh
# ---------------------------------------------------------------------------
# R18 的机器可验断言：探针不得具备执行外部命令 / 监听端口的能力。
#
# 这是 里那张验收清单中能自动化的部分。
# 放进 CI，任何一条不过就 fail —— 靠人记得是靠不住的。
#
# 用法：sh tools/check-agent-hardening.sh
# ---------------------------------------------------------------------------
set -eu

SRC="crates/pulse-agent/src"
FAIL=0

RED=''; GRN=''; RST=''
if [ -t 1 ]; then RED=$(printf '\033[31m'); GRN=$(printf '\033[32m'); RST=$(printf '\033[0m'); fi

ok() { printf '  %s✔%s %s\n' "$GRN" "$RST" "$1"; }
bad() { FAIL=$((FAIL+1)); printf '  %s✘%s %s\n' "$RED" "$RST" "$1"; shift; [ $# -gt 0 ] && printf '%s\n' "$@"; }

# 只看**非测试**的生产代码，且去掉注释。
#
# 去注释：否则「我们不调 nvidia-smi」这句注释自己就会让检查失败。
# 去测试：测试里合法地 bind 监听器来测 TCP 探测，那不是探针的能力。
#   （依赖「测试模块写在文件末尾」这个约定 —— 本仓库一直如此）
code() {
  # 必须是 `\;` 而不是 `+`：后者把多个文件塞进同一次 sed 调用，
  # 于是删除范围末尾的 `$` 指的是整个流的最后一行 ——
  # 碰到第一个 #[cfg(test)] 就会把后面所有文件一起删光，
  # 让检查静默地变成「什么都没查」。
  find "$SRC" -name '*.rs' -exec sed -E '/^#\[cfg\(test\)\]/,$d; s#//.*$##' {} \; 2>/dev/null
}

printf '=== R18 探针加固断言 ===\n\n'

# ── 1. 不得执行外部命令 ──
if code | grep -nE 'process::Command|Command::new' >/dev/null 2>&1; then
  bad "发现执行外部命令的代码路径" "$(code | grep -nE 'process::Command|Command::new' | head -5)"
else
  ok "无 process::Command —— 不存在执行外部命令的路径"
fi

# ── 1b. process::exit 只允许出现在更新模块 ──
#
# 退出当前进程不是「执行外部命令」，自更新换完二进制后必须靠它让服务管理器
# 拉起新版本。但它也是唯一能绕过正常关闭流程的东西，所以限定出现位置：
# 别的模块里冒出一个 exit，通常意味着某处在用它掩盖错误。
_bad_exit=$(grep -rnE 'process::(exit|abort)' "$SRC" --include='*.rs' \
            | grep -v '/update/' || true)
if [ -n "$_bad_exit" ]; then
  bad "更新模块之外出现了 process::exit/abort" "$_bad_exit"
else
  _n=$(grep -rcE 'process::(exit|abort)' "$SRC/update" --include='*.rs' 2>/dev/null \
       | awk -F: '{s+=$2} END{print s+0}')
  ok "process::exit 仅出现在更新模块（$_n 处，用于换完二进制后让服务管理器重启）"
fi

# ── 2. 不得监听端口 ──
if code | grep -nE 'TcpListener|UdpSocket::bind|UnixListener' >/dev/null 2>&1; then
  bad "发现监听端口的代码" "$(code | grep -nE 'TcpListener|UdpSocket::bind|UnixListener' | head -5)"
else
  ok "无 TcpListener/UnixListener —— 不监听任何端口"
fi

# ── 3. 不得读取敏感的机器指纹 ──
# product_serial / product_uuid 是 0400 只有 root 能读，我们既不需要也不该采集
if code | grep -nE 'product_serial|product_uuid|board_serial|chassis_serial|machine-id' >/dev/null 2>&1; then
  bad "发现读取机器指纹的代码" "$(code | grep -nE 'product_serial|product_uuid|board_serial' | head -5)"
else
  ok "不读取 product_serial / product_uuid 等机器指纹"
fi

# ── 4. 不得上报进程列表或命令行 ──
# 命令行里常有密码，是典型的敏感信息泄露面
if code | grep -nE '\.cmd\(\)|\.exe\(\)|cmdline' >/dev/null 2>&1; then
  bad "发现读取进程命令行的代码" "$(code | grep -nE '\.cmd\(\)|cmdline' | head -5)"
else
  ok "不读取进程命令行（命令行里常含密码）"
fi

# ── 5. unsafe 必须有 SAFETY 说明 ──
UNSAFE=$(code | grep -c 'unsafe ' | tr -d ' ')
SAFETY=$(grep -rh 'SAFETY:' "$SRC" --include='*.rs' | wc -l | tr -d ' ')
if [ "$UNSAFE" -gt "$SAFETY" ]; then
  bad "有 $UNSAFE 处 unsafe 但只有 $SAFETY 条 SAFETY 说明 —— 每处都必须写明为什么安全" \
      "$(grep -rn 'unsafe ' "$SRC" --include='*.rs' | grep -v '^\s*//' | head -8)"
else
  ok "unsafe 用量 $UNSAFE 处，SAFETY 说明 $SAFETY 处（每处都有说明）"
fi

# ── 6. server 下发的配置必须经过 sanitize ──
if grep -q 'ServerMsg::Config(c)' "$SRC/main.rs" && ! grep -q 'c.sanitize' "$SRC/main.rs"; then
  bad "收到 server 配置后没有调用 sanitize —— 数值与模式长度未做校验"
else
  ok "server 下发的配置经过 sanitize 校验"
fi

# ── 7. token 不得出现在命令行参数里 ──
if grep -nE '"--token"|"-t"' "$SRC/main.rs" >/dev/null 2>&1; then
  bad "token 出现在命令行参数中 —— 同机任何用户都能通过 ps 看到"
else
  ok "token 只从环境变量读，不接受命令行参数"
fi

printf '\n'
if [ "$FAIL" -eq 0 ]; then
  printf '%s全部通过%s —— R18 的可自动化部分成立。\n' "$GRN" "$RST"
  printf '仍需真机验证的部分的验收清单（非 root 运行、\n'
  printf 'getpcaps 为空、systemd-analyze security 评分、ss 无监听）。\n'
  exit 0
else
  printf '%s%d 项未通过%s\n' "$RED" "$FAIL" "$RST"
  exit 1
fi
