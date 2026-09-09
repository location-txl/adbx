//! adbx：adb 扩展 CLI 的实现库。
//!
//! 命令行入口见 `src/main.rs`（clap 解析 + 分发），
//! 各 adb 包装命令按"一个命令一个模块"放在 [`commands`] 下。

pub mod adb;
pub mod commands;
