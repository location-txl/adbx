//! `adbx uninstall` 子命令：按关键词模糊匹配包名并卸载应用。

use std::io::{self, Write};

use crate::adb;
use crate::matching::{PackageMatch, match_failure, match_packages};
use anyhow::Result;

/// `uninstall` 子命令入口。
///
/// * `serial` - 目标设备 serial（可为 None，由 adb 自行选择）
/// * `keywords` - 包名关键词（AND 关系），由 clap 保证至少一个
/// * `yes` - true 时跳过二次确认直接卸载（对应 `-y/--yes`）
///
/// 唯一/精确命中时先二次确认（除非 `yes`），确认后执行 `pm uninstall` 并打印结果；
/// 用户取消时打印提示、以退出码 0 正常结束；多命中或零命中时返回 Err（main 统一以退出码 1 结束）。
pub fn run(serial: Option<&str>, keywords: &[String], yes: bool) -> Result<()> {
    let packages = adb::list_packages(serial)?;
    match match_packages(&packages, keywords) {
        PackageMatch::Exact(pkg) | PackageMatch::Unique(pkg) => {
            if !yes && !confirm_uninstall(&pkg)? {
                println!("已取消卸载 {pkg}");
                return Ok(());
            }
            adb::uninstall(serial, &pkg)?;
            println!("✓ 已卸载 {pkg}");
            Ok(())
        }
        // 多命中列候选、零命中报错，统一由 match_failure 生成文案
        other => Err(match_failure(keywords, other)),
    }
}

/// 提示用户确认是否卸载 `package`。
///
/// EOF / 读失败以外的输入交给 [`is_confirm`] 判定；stdout 需先 flush 提示才会显示。
fn confirm_uninstall(package: &str) -> Result<bool> {
    print!("即将卸载 {package}，确认？[y/N] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(is_confirm(&input))
}

/// 确认输入判定：仅 y / yes（大小写不敏感、忽略首尾空白）算确认，其余一律取消。
fn is_confirm(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_input_should_accept_y_and_yes_case_insensitive() {
        // read_line 读到的输入带换行，trim 后判定
        for input in ["y", "Y", "yes", "YES", "Yes", "y\n", " yes \n"] {
            assert!(is_confirm(input), "应确认：{input:?}");
        }
    }

    #[test]
    fn confirm_input_should_reject_everything_else_as_cancel() {
        // 回车、明确拒绝、任何其他输入都按取消处理（安全默认）
        for input in ["", "\n", "n", "no", "N", "yeah", "y es", "是"] {
            assert!(!is_confirm(input), "应取消：{input:?}");
        }
    }
}
