//! doctor 子命令：检查 adb 环境是否可用。

use anyhow::Result;

use crate::adb;

/// 检查 adb 是否已安装并打印版本号。
///
/// 与设备无关，忽略 `-s` 参数。adb 未安装或不可执行时错误冒泡，
/// 由 main 统一打印 `✗` 并以退出码 1 结束。
pub fn run() -> Result<()> {
    let version = adb::adb_version()?;
    println!("✓ adb 已安装，版本 {version}");
    Ok(())
}
