//! Linux 采集。
//!
//! 两块都刻意做成**跨平台可编译**，好让它们的测试在任何开发机上跑：
//! - `parse`：纯函数，任何平台
//! - `imp`：读文件 + `statvfs`，没有一处是 Linux 专有的，所以在全部 unix 上编译
//!
//! 只有「把它选为当前平台的采集器」这一步是 Linux 专属（见 `collect/mod.rs`）。
//! 这样真机需要回答的问题就只剩一个：**这些文件读不读得到**。

// 在非 Linux 平台上这里的代码没有运行时使用者（`Platform` 不指向它），
// 但**仍然编译** —— 这样在 macOS 上 `cargo check` 就能抓到 Linux 代码的
// 类型错误，而不是等到交叉编译时才发现。代价只是要显式允许 dead_code。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

pub mod parse;

#[cfg(unix)]
mod imp;
// `Roots` 只被 imp 内部的 fixture 测试用到；`LinuxCollector` 在非 Linux 的
// unix 上也没有使用者（`Platform` 不会指向它）。两者都导出是为了让
// fixture 测试能在任何开发机上编译并运行。
#[cfg(unix)]
#[allow(unused_imports)]
pub use imp::{LinuxCollector, Roots};
