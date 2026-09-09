//! adb 命令调用封装。
//!
//! 统一处理设备选择（-s）、错误透传（带出 adb 的 stderr），
//! 供各子命令复用，避免散落各处的 Command 拼装。

use anyhow::{Context, Result};
use std::process::Command;

/// 执行一条 adb 命令，返回 stdout。
///
/// * `serial` - 目标设备 serial，会转发为 `adb -s <serial>`；传 `None` 时由 adb
///   自行选择唯一连接的设备（无设备或多设备时 adb 会报错并透传给调用方）
/// * `args` - adb 之后的参数，如 `["shell", "pm", "list", "packages"]`
///
/// 返回 stdout 内容（UTF-8 容错解码）。adb 不存在或退出码非 0 时返回 Err，
/// 错误信息附带 adb 的 stderr 原文。
///
/// # 示例
///
/// ```ignore
/// let out = run_adb(None, &["shell", "pm", "list", "packages"])?;
/// ```
pub fn run_adb(serial: Option<&str>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("adb");
    if let Some(serial) = serial {
        cmd.args(["-s", serial]);
    }
    let output = cmd
        .args(args)
        .output()
        .context("无法启动 adb，请确认已安装并在 PATH 中")?;
    if !output.status.success() {
        anyhow::bail!(
            "adb {} 失败：{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// 获取设备上已安装的全部包名。
///
/// 基于 `pm list packages`，已去掉每行的 `package:` 前缀。
/// 返回包名列表（顺序与设备输出一致，通常为字典序）。
pub fn list_packages(serial: Option<&str>) -> Result<Vec<String>> {
    let out = run_adb(serial, &["shell", "pm", "list", "packages"])?;
    Ok(out
        .lines()
        // 过滤掉空行和异常输出，只保留 package: 前缀的行
        .filter_map(|line| line.strip_prefix("package:"))
        .map(str::to_owned)
        .collect())
}

/// 强制停止指定包名的应用（`am force-stop`）。
///
/// * `package` - 完整包名；应用未在运行时该命令也会成功，调用方无需预检查
///
/// 失败（包不存在、设备断开等）时返回 Err 并透传 adb 的 stderr。
pub fn force_stop(serial: Option<&str>, package: &str) -> Result<()> {
    run_adb(serial, &["shell", "am", "force-stop", package])?;
    Ok(())
}
