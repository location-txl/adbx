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
    let (stdout, _) = run_adb_raw(serial, args)?;
    Ok(stdout)
}

/// 执行一条 adb 命令，返回 (stdout, stderr)。退出码非 0 时返回 Err。
///
/// 供需要检查 stderr 的调用方使用（如 [`uninstall`]：部分 Android 的
/// `pm uninstall` 失败时退出码仍为 0，只把报错打在 stderr），其余命令用 [`run_adb`] 即可。
fn run_adb_raw(serial: Option<&str>, args: &[&str]) -> Result<(String, String)> {
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
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// 从 `adb version` 输出中解析版本号。
///
/// adb 首行输出形如 `Android Debug Bridge version 1.0.41`，
/// 取 `version ` 之后的部分；无法识别时返回 None。
fn parse_adb_version(output: &str) -> Option<String> {
    let first_line = output.lines().next()?.trim();
    first_line
        .strip_prefix("Android Debug Bridge version ")
        .map(str::to_owned)
}

/// 获取本机 adb 版本号（`adb version`）。
///
/// 与设备无关，不使用 serial。adb 不存在时返回 Err（提示安装）；
/// 版本行格式异常时退回首行原文，不视为错误。
pub fn adb_version() -> Result<String> {
    let out = run_adb(None, &["version"])?;
    Ok(parse_adb_version(&out)
        .unwrap_or_else(|| out.lines().next().unwrap_or_default().trim().to_owned()))
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

/// 启动指定包名的应用入口 Activity。
///
/// 通过 `monkey -p <pkg> -c android.intent.category.LAUNCHER 1` 拉起，
/// 一次调用即可，无需先解析入口 Activity 名。
///
/// * `package` - 完整包名；应用须有 launcher Activity（纯服务类应用没有），
///   否则 monkey 以非 0 退出，错误透传给调用方
pub fn launch_app(serial: Option<&str>, package: &str) -> Result<()> {
    run_adb(
        serial,
        &["shell", "monkey", "-p", package, "-c", "android.intent.category.LAUNCHER", "1"],
    )?;
    Ok(())
}

/// 获取指定包名的 dumpsys 信息（`dumpsys package <pkg>`）。
///
/// * `package` - 完整包名；包不存在时输出可能为空，由调用方解析降级
///
/// 返回 dumpsys 原文，版本字段解析由调用方（info 命令）完成。
/// 设备断开等失败场景返回 Err 并透传 adb 的 stderr。
pub fn dump_package(serial: Option<&str>, package: &str) -> Result<String> {
    run_adb(serial, &["shell", "dumpsys", "package", package])
}

/// 清除指定包名的应用数据（`pm clear`）。
///
/// * `package` - 完整包名
///
/// 该操作会同时强制停止应用。失败（包不存在、设备断开等）时返回 Err 并透传 adb 的 stderr。
pub fn clear_data(serial: Option<&str>, package: &str) -> Result<()> {
    run_adb(serial, &["shell", "pm", "clear", package])?;
    Ok(())
}

/// 卸载指定包名的应用（`pm uninstall`）。
///
/// * `package` - 完整包名
///
/// **该操作不可恢复**：应用及其全部数据会被删除。
/// 除退出码外还需校验输出：部分 Android 的 pm uninstall 失败时退出码仍为 0
/// （实测 Android 7.1：stderr 打 Java 异常、stdout 为空），只把报错打在输出里。
/// 校验不通过时返回 Err 并透传 pm 的报错原文。
pub fn uninstall(serial: Option<&str>, package: &str) -> Result<()> {
    let (stdout, stderr) = run_adb_raw(serial, &["shell", "pm", "uninstall", package])?;
    if !is_uninstall_success(&stdout, &stderr) {
        // 优先报 stderr（异常栈），stdout 无内容时兜底（Failure 场景）
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        anyhow::bail!("卸载 {package} 失败：{detail}");
    }
    Ok(())
}

/// 判断 pm uninstall 的输出是否表示成功（纯函数，可单测）。
///
/// 成功时 stdout 为 `Success` 且 stderr 为空。失败时要么 stderr 非空
/// （Java 异常），要么 stdout 含 `Failure`，二者任一出现即视为失败。
fn is_uninstall_success(stdout: &str, stderr: &str) -> bool {
    stderr.trim().is_empty() && !stdout.contains("Failure")
}

#[cfg(test)]
mod tests {
    use super::{is_uninstall_success, parse_adb_version};

    #[test]
    fn parses_standard_version_output() {
        let out = "Android Debug Bridge version 1.0.41\nVersion 35.0.0-11465562\n";
        assert_eq!(parse_adb_version(out).as_deref(), Some("1.0.41"));
    }

    #[test]
    fn returns_none_for_unrecognized_output() {
        assert_eq!(parse_adb_version(""), None);
        assert_eq!(parse_adb_version("unexpected format\n"), None);
    }

    #[test]
    fn uninstall_success_output_should_pass() {
        // 真机实测：成功时 stdout 为 Success，stderr 为空
        assert!(is_uninstall_success("Success\n", ""));
    }

    #[test]
    fn uninstall_exception_on_stderr_should_fail() {
        // 真机实测（Android 7.1，包不存在）：stderr 打 Java 异常栈、退出码 0、stdout 为空
        let stderr = "Exception occurred while dumping:\njava.lang.IllegalArgumentException: Unknown package: x\n";
        assert!(!is_uninstall_success("", stderr));
    }

    #[test]
    fn uninstall_failure_on_stdout_should_fail() {
        // 部分版本的 pm 把 Failure 打在 stdout（同样退出码 0）
        assert!(!is_uninstall_success("Failure [DELETE_FAILED_INTERNAL_ERROR]\n", ""));
    }

    /// 真机回归：不存在的包卸载失败时，uninstall 必须返回 Err（而非误报成功）。
    /// 仅验证失败判定路径，不会卸载任何真实应用。手动运行：
    /// `cargo test uninstall_failure_on_real_device -- --ignored`
    #[test]
    #[ignore = "需要连接一台 adb 设备"]
    fn uninstall_failure_on_real_device() {
        assert!(super::uninstall(None, "com.nonexistent.adbx.regression").is_err());
    }
}
