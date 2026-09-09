#!/bin/sh
# ---------------------------------------------------------------------------
# 三平台交叉检查（L1 + L2）。
#
# agent 要在 Linux / Windows / macOS 上跑，但开发机只有一个。
# 这个脚本把「另外两个平台的代码能不能编译、有没有 lint 问题」变成一条命令。
#
# 为什么 Linux 目标要用 cargo-zigbuild：
#   agent 依赖 rustls 的 ring provider，而 ring 的 build script 需要
#   **目标平台的 C 编译器**。zig 自带全套交叉编译工具链，比装 musl-cross 轻得多。
#
# 前置（一次性）：
#   brew install zig            # 或你的包管理器
#   cargo install cargo-zigbuild
#   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl \
#                     x86_64-pc-windows-msvc
# ---------------------------------------------------------------------------
set -eu

FAIL=0
RED=''; GRN=''; RST=''
if [ -t 1 ]; then RED=$(printf '\033[31m'); GRN=$(printf '\033[32m'); RST=$(printf '\033[0m'); fi

run() {
    label=$1; shift
    printf '  %-42s ' "$label"
    if out=$("$@" 2>&1); then
        printf '%s✔%s\n' "$GRN" "$RST"
    else
        FAIL=$((FAIL + 1))
        printf '%s✘%s\n' "$RED" "$RST"
        printf '%s\n' "$out" | grep -E '^error' -A5 | head -12 | sed 's/^/      /'
    fi
}

printf '=== 本机（macOS/当前平台）===\n'
run "clippy --workspace"        cargo clippy --workspace --all-targets -- -D warnings
run "fmt --check"               cargo fmt --all --check
run "test --workspace"          cargo test --workspace

printf '\n=== Linux 目标（经 zig）===\n'
if command -v cargo-zigbuild >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
    for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
        # 注意是直接调 cargo-zigbuild 而不是 `cargo zigbuild` ——
        # 后者会把 "clippy" 当成 zigbuild 的参数
        run "clippy agent → $t" cargo-zigbuild clippy -p pulse-agent --target "$t" -- -D warnings
    done
else
    printf '  %s跳过%s：未安装 zig / cargo-zigbuild（见本文件头部）\n' "$RED" "$RST"
    FAIL=$((FAIL + 1))
fi

printf '\n=== Windows 目标（经 zig，用 gnu ABI）===\n'
# 用 windows-gnu 而不是 windows-msvc：ring 的 build script 需要目标平台的
# C 编译器，zig 能提供 gnu ABI 的，msvc ABI 则需要真正的 MSVC 工具链。
# 两者的 Rust 代码路径完全相同，所以这仍然能验到 Windows 的条件编译分支。
if command -v cargo-zigbuild >/dev/null 2>&1; then
    run "clippy agent → x86_64-pc-windows-gnu" \
        cargo-zigbuild clippy -p pulse-agent --target x86_64-pc-windows-gnu -- -D warnings
else
    printf '  跳过：未安装 cargo-zigbuild\n'
    FAIL=$((FAIL + 1))
fi

printf '\n'
if [ "$FAIL" -eq 0 ]; then
    printf '%s全部通过%s\n' "$GRN" "$RST"
    exit 0
fi
printf '%s%d 项未通过%s\n' "$RED" "$FAIL" "$RST"
exit 1
