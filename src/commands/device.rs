//! 设备会话子命令：按终端绑定设备（`use`/`unuse` 的解析与提示）、
//! `current` 查询绑定状态，以及供终端集成调用的 `__resolve-device`。
//!
//! 绑定机制复用 adb 官方的 `ANDROID_SERIAL` 环境变量：Shell 集成函数
//! （见 [`crate::commands::init`]）拦截 `adbx use/unuse` 修改当前终端的
//! 环境变量，adbx 与 adb 的其余调用自动遵循它；终端之间互不影响。

use anyhow::{Result, bail};

use crate::adb::{self, DeviceEntry};

/// 从可用设备中自动选择唯一一台（纯函数，可单测）。
///
/// * `devices` - `adb devices -l` 的解析结果
///
/// 恰有一台 `device` 状态的设备时返回它；零台或多台时返回 Err：
/// 零台时区分「设备未授权」与「无设备」，多台时列出清单并提示
/// 用 `-s` 或 `adbx use` 指定，不自动选第一台。
fn select_only_usable(devices: &[DeviceEntry]) -> Result<&DeviceEntry> {
    let ready: Vec<&DeviceEntry> = devices.iter().filter(|d| d.is_ready()).collect();
    match ready.as_slice() {
        [only] => Ok(only),
        [] => {
            if devices.iter().any(|d| d.state == "unauthorized") {
                bail!("没有可用的设备：设备尚未授权 USB 调试，请在设备上允许本机调试后重试");
            }
            if devices.is_empty() {
                bail!("未检测到已连接的设备，请确认设备已连接并授权（可用 adbx devices 查看）");
            }
            bail!("没有可用的设备：\n{}", format_device_lines(devices));
        }
        _ => bail!(
            "连接了多台可用设备，请用 -s <serial> 指定，或先绑定：adbx use <serial>\n{}",
            format_device_lines(devices)
        ),
    }
}

/// 按精确 serial 或唯一片段匹配一台设备（纯函数，可单测）。
///
/// * `devices` - 设备列表
/// * `token` - 用户输入的设备标识；先精确匹配 serial，再取唯一包含片段
///   （模拟器用户习惯记端口号，如 `5554` 命中 `emulator-5554`）
///
/// 命中且状态可用时返回该设备；命中但不可用（未授权/离线等）时返回
/// 带状态说明的 Err；片段命中多台或零台时返回列出候选的 Err。
fn match_token<'a>(devices: &'a [DeviceEntry], token: &str) -> Result<&'a DeviceEntry> {
    if let Some(exact) = devices.iter().find(|d| d.serial == token) {
        return ensure_ready(exact);
    }
    // 包含匹配而非仅前缀：serial 片段（如端口号 5554）同样能唯一定位设备
    let matched: Vec<&DeviceEntry> = devices
        .iter()
        .filter(|d| d.serial.contains(token))
        .collect();
    match matched.len() {
        0 => bail!(
            "没有匹配 {token:?} 的设备。当前设备：\n{}",
            format_device_lines(devices)
        ),
        1 => ensure_ready(matched[0]),
        _ => bail!(
            "{token:?} 命中多台设备，请补全 serial：\n{}",
            format_device_lines(matched.iter().copied())
        ),
    }
}

/// 校验设备状态可用，不可用时按状态给出针对性提示（纯函数，可单测）。
fn ensure_ready(device: &DeviceEntry) -> Result<&DeviceEntry> {
    if device.is_ready() {
        return Ok(device);
    }
    match device.state.as_str() {
        "unauthorized" => bail!(
            "设备 {} 尚未授权 USB 调试，请在设备上允许本机调试",
            device.serial
        ),
        "offline" => bail!("设备 {} 当前离线，请重新连接", device.serial),
        other => bail!("设备 {} 状态为 {other}，暂不可用", device.serial),
    }
}

/// 把设备列表格式化为对齐的多行文本（serial 左对齐补空格 + 状态 + 型号），
/// 用于错误提示与 `current` 的设备清单展示。
fn format_device_lines<'a>(devices: impl IntoIterator<Item = &'a DeviceEntry>) -> String {
    let devices: Vec<&DeviceEntry> = devices.into_iter().collect();
    let width = devices.iter().map(|d| d.serial.len()).max().unwrap_or(0);
    devices
        .iter()
        .map(|d| {
            let model = if d.model.is_empty() {
                String::new()
            } else {
                format!("  {}", d.model)
            };
            format!("  {:<width$}  {}{}", d.serial, d.state, model)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `__resolve-device` 入口：解析设备标识并向 stdout 输出一行 serial。
///
/// * `token` - 设备 serial 或唯一片段（如端口号 5554）；缺省时自动选择唯一可用设备
///
/// 只输出 serial 本身（供 shell 集成捕获），所有报错走 stderr；
/// 解析失败由 main 统一以退出码 1 结束。
pub fn resolve(token: Option<&str>) -> Result<()> {
    let devices = adb::list_devices()?;
    let device = match token {
        Some(token) => match_token(&devices, token)?,
        None => select_only_usable(&devices)?,
    };
    println!("{}", device.serial);
    Ok(())
}

/// `current` 入口：显示当前终端绑定的设备及连接状态。
///
/// * `serial` - clap 解析的目标设备（显式 `-s` 或 `ANDROID_SERIAL`）；
///   `None` 表示两者均未设置，即当前终端未绑定
///
/// 绑定存在时查询该设备的型号与状态；未绑定时提示并列出当前设备，
/// 方便直接挑选。设备查询失败（adb 缺失等）返回 Err。
pub fn current(serial: Option<&str>) -> Result<()> {
    let devices = adb::list_devices()?;
    match serial {
        Some(serial) => match devices.iter().find(|d| d.serial == serial) {
            Some(device) if device.is_ready() => {
                let model = if device.model.is_empty() {
                    String::new()
                } else {
                    format!("  {}", device.model)
                };
                println!("✓ 当前设备：{}  [{}]{}", device.serial, device.state, model);
                Ok(())
            }
            Some(device) => bail!("设备 {} 当前不可用（{}）", device.serial, device.state),
            None => bail!("设备 {serial} 已不在当前设备列表中，可能已断开"),
        },
        None => {
            println!("当前终端未绑定设备（ANDROID_SERIAL 未设置）");
            if devices.is_empty() {
                println!("未检测到已连接的设备");
            } else {
                println!("当前设备：");
                println!("{}", format_device_lines(&devices));
            }
            Ok(())
        }
    }
}

/// `use`/`unuse` 入口：未安装终端集成时输出安装指引并失败退出。
///
/// 这两个命令需要修改当前 Shell 的环境变量，只能由终端集成函数完成；
/// 直接运行二进制（如通过 `command adbx`）时给出一次性安装方法。
pub fn integration_hint(command: &str) -> Result<()> {
    bail!(
        "adbx {command} 需要终端集成才能修改当前 Shell 的环境变量。\n\
         一次性安装：\n\
         \u{20}\u{20} zsh:        echo 'eval \"$(adbx init zsh)\"' >> ~/.zshrc\n\
         \u{20}\u{20} bash:       echo 'eval \"$(adbx init bash)\"' >> ~/.bashrc\n\
         \u{20}\u{20} PowerShell: adbx init powershell | Add-Content $PROFILE\n\
         安装后重新打开终端，即可使用 adbx {command} <serial> 绑定设备"
    )
}

/// 设备类命令的 serial 统一解析（供 main 分发调用）。
///
/// * `explicit` - 显式 `-s` 或 `ANDROID_SERIAL` 的值（clap 已合并，前者优先）
///
/// 有显式值时原样透传；缺省时列出设备并自动选择唯一可用设备，
/// 零台或多台可用时返回列出清单的 Err，不静默退回 adb 的英文报错。
pub fn auto_serial(explicit: Option<&str>) -> Result<Option<String>> {
    match explicit {
        Some(serial) => Ok(Some(serial.to_owned())),
        None => Ok(Some(
            select_only_usable(&adb::list_devices()?)?.serial.clone(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceEntry, ensure_ready, format_device_lines, match_token, select_only_usable};

    fn device(serial: &str, state: &str, model: &str) -> DeviceEntry {
        DeviceEntry {
            serial: serial.into(),
            state: state.into(),
            model: model.into(),
        }
    }

    #[test]
    fn auto_select_returns_the_only_ready_device() {
        // 一台可用 + 一台未授权：只统计 device 状态，唯一可用即中选
        let devices = [
            device("emu", "device", "M"),
            device("phone", "unauthorized", ""),
        ];
        assert_eq!(select_only_usable(&devices).unwrap().serial, "emu");
    }

    #[test]
    fn auto_select_fails_without_ready_device() {
        // 只有未授权设备：给出针对性提示而非笼统的「无设备」
        let devices = [device("phone", "unauthorized", "")];
        let err = select_only_usable(&devices).unwrap_err().to_string();
        assert!(err.contains("授权"), "应提示授权问题：{err}");

        let empty: [DeviceEntry; 0] = [];
        let err = select_only_usable(&empty).unwrap_err().to_string();
        assert!(err.contains("未检测到"), "空列表应提示无设备：{err}");
    }

    #[test]
    fn auto_select_fails_with_multiple_ready_devices() {
        // 多台可用：报错并列出清单，不自动选第一台
        let devices = [
            device("emu", "device", "M1"),
            device("phone", "device", "M2"),
        ];
        let err = select_only_usable(&devices).unwrap_err().to_string();
        assert!(
            err.contains("emu") && err.contains("phone"),
            "应列出设备：{err}"
        );
    }

    #[test]
    fn token_matches_exact_serial_and_unique_substring() {
        let devices = [
            device("emulator-5554", "device", "M1"),
            device("emulator-5556", "device", "M2"),
            device("phone", "device", "M3"),
        ];
        // 精确匹配
        assert_eq!(match_token(&devices, "phone").unwrap().serial, "phone");
        // 唯一片段：端口号 5554 只出现在一台设备的 serial 中（模拟器常用记法）
        assert_eq!(
            match_token(&devices, "5554").unwrap().serial,
            "emulator-5554"
        );
        // 完整 serial 本身也是精确匹配
        assert_eq!(
            match_token(&devices, "emulator-5554").unwrap().serial,
            "emulator-5554"
        );
    }

    #[test]
    fn token_ambiguous_substring_fails_with_candidates() {
        let devices = [
            device("emulator-5554", "device", ""),
            device("emulator-5556", "device", ""),
            device("phone", "device", ""),
        ];
        // 片段 emulator 命中两台：报错列候选，不选第一个
        let err = match_token(&devices, "emulator").unwrap_err().to_string();
        assert!(err.contains("emulator-5554") && err.contains("emulator-5556"));
    }

    #[test]
    fn token_without_match_fails_with_device_list() {
        let devices = [device("phone", "device", "")];
        let err = match_token(&devices, "tablet").unwrap_err().to_string();
        assert!(err.contains("phone"), "零命中应列出当前设备：{err}");
    }

    #[test]
    fn matched_device_must_be_ready() {
        // 精确命中但设备不可用：按状态给出针对性提示
        let unauthorized = device("phone", "unauthorized", "");
        let err = ensure_ready(&unauthorized).unwrap_err().to_string();
        assert!(err.contains("授权"), "应提示授权：{err}");

        let offline = device("phone", "offline", "");
        assert!(
            ensure_ready(&offline)
                .unwrap_err()
                .to_string()
                .contains("离线")
        );

        let devices = [device("phone", "offline", "")];
        assert!(match_token(&devices, "phone").is_err());

        // 非常见状态（bootloader 等）原样报出
        let bootloader = device("fast", "bootloader", "");
        assert!(
            ensure_ready(&bootloader)
                .unwrap_err()
                .to_string()
                .contains("bootloader")
        );
    }

    #[test]
    fn device_lines_align_serial_and_show_model() {
        let devices = [
            device("emu", "device", "Pixel 5"),
            device("p", "offline", ""),
        ];
        let lines = format_device_lines(&devices);
        assert_eq!(lines, "  emu  device  Pixel 5\n  p    offline");
    }
}
