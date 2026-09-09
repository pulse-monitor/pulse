//! 构建期注入两个常量。
//!
//! - `PULSE_TARGET`：本次编译的目标三元组。发布产物按它命名，
//!   agent 自更新时要用它去清单里找自己那一行 —— 用 `std::env::consts`
//!   拼是拼不出 musl/gnu 这种区别的。
//! - `PULSE_UPDATE_PUBKEY`：minisign 公钥，**编译期内置**。
//!   换公钥必须重新编译并重装 agent，这是第 2 道防线的前提。
//!   没设 = 自更新整个功能关闭（`capabilities.self_update` 如实报 false）。
fn main() {
    println!("cargo:rerun-if-env-changed=PULSE_UPDATE_PUBKEY");
    let target = std::env::var("TARGET").expect("cargo 一定会设置 TARGET");
    println!("cargo:rustc-env=PULSE_TARGET={target}");
    let pubkey = std::env::var("PULSE_UPDATE_PUBKEY").unwrap_or_default();
    println!("cargo:rustc-env=PULSE_UPDATE_PUBKEY={pubkey}");
}
