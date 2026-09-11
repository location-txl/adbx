//! `adbx info` 子命令：按关键词模糊匹配包名，展示完整包名及版本信息。

use crate::adb;
use crate::matching::{PackageMatch, match_failure, match_packages};
use anyhow::Result;

/// `info` 子命令入口。
///
/// * `serial` - 目标设备 serial（可为 None，由 adb 自行选择）
/// * `keywords` - 包名关键词（AND 关系），由 clap 保证至少一个
///
/// 与 stop / clear 不同：多命中时**不报错**，逐个展示所有命中包的信息；
/// 仅零命中时报错（main 统一以退出码 1 结束）。
pub fn run(serial: Option<&str>, keywords: &[String]) -> Result<()> {
    let packages = adb::list_packages(serial)?;
    match match_packages(&packages, keywords) {
        PackageMatch::Exact(pkg) => print_info(serial, &pkg),
        PackageMatch::Unique(pkg) => print_info(serial, &pkg),
        // info 是只读命令，多命中时全部展示而不是要求细化关键词
        PackageMatch::Multiple(pkgs) => {
            for pkg in &pkgs {
                print_info(serial, pkg)?;
            }
            Ok(())
        }
        PackageMatch::None => Err(match_failure(keywords, PackageMatch::None)),
    }
}

/// 拉取单个包的 dumpsys 并打印包名 + 版本信息。
fn print_info(serial: Option<&str>, package: &str) -> Result<()> {
    let dump = adb::dump_package(serial, package)?;
    let info = parse_version_info(&dump);
    println!("✓ {package}");
    // 版本字段缺失（老 ROM / 厂商定制输出格式异常）时降级显示"未知"，不报错
    println!(
        "    versionName: {}",
        info.version_name.as_deref().unwrap_or("未知")
    );
    println!(
        "    versionCode: {}",
        info.version_code.as_deref().unwrap_or("未知")
    );
    Ok(())
}

/// 从 `dumpsys package` 输出中解析出的版本字段。
struct VersionInfo {
    version_code: Option<String>,
    version_name: Option<String>,
}

/// 解析 dumpsys 输出中的 versionCode / versionName（纯函数，可单测）。
///
/// 目标行形如（自 API 24 起格式稳定，取首次出现的行）：
/// ```text
///     versionCode=123 minSdk=24 targetSdk=33
///     versionName=1.2.3
/// ```
/// versionCode 后面还跟着其他字段，只取到下一个空白为止；
/// debug 包常见 `versionName=null`，原样保留由调用方展示。
fn parse_version_info(dump: &str) -> VersionInfo {
    let mut version_code = None;
    let mut version_name = None;
    for line in dump.lines() {
        let line = line.trim();
        if version_code.is_none()
            && let Some(rest) = line.strip_prefix("versionCode=")
        {
            version_code = Some(
                rest.split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
        if version_name.is_none()
            && let Some(rest) = line.strip_prefix("versionName=")
        {
            version_name = Some(
                rest.split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
    }
    VersionInfo {
        version_code,
        version_name,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_version_info;

    /// 典型 Android 10+ 真机输出片段（节选，字段顺序与真实输出一致）
    const TYPICAL_DUMP: &str = "\
Package [com.example.app] (…):
    userId=10086
    versionCode=2600 minSdk=24 targetSdk=33
    versionName=1.2.3
    ";

    #[test]
    fn parses_typical_dump() {
        let info = parse_version_info(TYPICAL_DUMP);
        assert_eq!(info.version_code.as_deref(), Some("2600"));
        assert_eq!(info.version_name.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn keeps_null_version_name_as_is() {
        // debug 包常见：versionName=null，原样保留
        let dump = "    versionCode=1\n    versionName=null\n";
        let info = parse_version_info(dump);
        assert_eq!(info.version_name.as_deref(), Some("null"));
    }

    #[test]
    fn missing_fields_yield_none() {
        // 极老 ROM / 厂商定制格式异常：两行都没有，降级为未知
        let info = parse_version_info("Package [com.x] (…):\n    userId=1\n");
        assert_eq!(info.version_code, None);
        assert_eq!(info.version_name, None);
        assert_eq!(parse_version_info("").version_name, None);
    }
}
