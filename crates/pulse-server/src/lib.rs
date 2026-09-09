//! Pulse 服务端的可复用部分。
//!
//! 拆成 lib 不只是为了单元测试：`crates/pulse-loadgen` 压测时必须走**真实的存储层
//! 代码路径**，否则测出来的数字验证的是压测程序自己，不是实际实现。

pub mod api;
pub mod auth;
pub mod domain;
pub mod install;
pub mod notify;
pub mod state;
pub mod store;
pub mod tasks;
pub mod tls;
