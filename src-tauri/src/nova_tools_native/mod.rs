//! 与 native/nova-tools-napi 共享的同一份 Rust 原生工具实现。
//! napi crate 通过 #[path] 逐文件引用；nova 本体（含 Lyra agent）从这里直接调用。

pub mod context;
pub mod edit;
pub mod read;
