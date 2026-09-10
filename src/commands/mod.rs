//! 子命令模块：每个 adb 包装命令一个模块，便于按功能就近维护。

pub mod clear;
pub mod doctor;
pub mod info;
pub mod restart;
pub mod stop;
pub mod uninstall;
