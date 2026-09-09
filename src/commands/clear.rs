//! `adbx clear` 子命令：按关键词模糊匹配包名并清除应用数据。

use crate::adb;
use crate::matching::{PackageMatch, match_failure, match_packages};
use anyhow::Result;

/// `clear` 子命令入口。
///
/// * `serial` - 目标设备 serial（可为 None，由 adb 自行选择）
/// * `keywords` - 包名关键词（AND 关系），由 clap 保证至少一个
///
/// 唯一/精确命中时执行 `pm clear` 清除应用数据（会同时强制停止应用）并打印结果；
/// 多命中或零命中时返回 Err（main 统一以退出码 1 结束）。
pub fn run(serial: Option<&str>, keywords: &[String]) -> Result<()> {
    let packages = adb::list_packages(serial)?;
    match match_packages(&packages, keywords) {
        PackageMatch::Exact(pkg) | PackageMatch::Unique(pkg) => {
            adb::clear_data(serial, &pkg)?;
            println!("✓ 已清除 {pkg} 数据");
            Ok(())
        }
        // 多命中列候选、零命中报错，统一由 match_failure 生成文案
        other => Err(match_failure(keywords, other)),
    }
}
