//! 未知调用透传：adbx 未命中的子命令/旗标原样转发给 adb。
//!
//! adbx 定位是 adb 的超集：自有子命令（stop/clear/…）走 clap 流程，
//! 其余调用（shell、devices、logcat、adb 旗标 -d/-e 等）静默转发给
//! adb，stdio、信号、退出码与直接调 adb 完全一致，用户可以永远只敲 adbx。
//! 路由只解释 serial 旗标之前的 token，其余参数一律原样透传、不二次解释。

use std::ffi::{OsStr, OsString};

use anyhow::Result;

use crate::adb;

/// 参数路由结果：由 main 在 clap 解析之前判定。
#[derive(Debug, PartialEq)]
pub enum Route {
    /// adbx 自己处理：交给 clap 解析（含报错，如缺子命令、缺参数）
    Own,
    /// 转发 adb：serial 归一化为 `-s <serial>` 前置（见 [`adb::exec_adb`]），
    /// rest 为从首个未命中 token 起的原始参数
    Forward {
        /// 捕获的 `-s/--serial` 值，四种写法（`-s v` / `-sv` / `--serial v` /
        /// `--serial=v`）统一归一；adb 只认 `-s` 短旗标，不认 `--serial`
        serial: Option<OsString>,
        /// 原样透传的参数（OsString，不二次解释，兼容非 UTF-8）
        rest: Vec<OsString>,
    },
}

/// 把 adbx 的命令行参数路由为「adbx 自有」或「转发 adb」（纯函数，可单测）。
///
/// 只解释 serial 旗标之前的 token：首个其余 token 决定去向——是已知子命令
/// （由 `is_known` 判定，main 传入 clap 定义的自省闭包）或 adbx 自身旗标
/// （`-h/--help`、`-V/--version`，clap 内建）则 [`Route::Own`]，否则
/// [`Route::Forward`] 并从该 token 起原样透传。据此 adbx 子命令的参数错误
/// （如 `adbx stop` 缺关键词）仍由 clap 报出，不会被误转发。
/// `--` 作为分隔符被消费，不随转发传递；扫描完仍没有子命令 token
/// （空参数、仅 `-s`、`-s` 缺值）返回 [`Route::Own`]，由 clap 报错。
///
/// # 示例
///
/// ```
/// use std::ffi::OsString;
/// use adbx::commands::passthrough::{Route, classify};
///
/// let args: Vec<OsString> = ["shell", "ls"].iter().map(OsString::from).collect();
/// let Route::Forward { serial, rest } = classify(&args, |tok| tok == "stop") else {
///     panic!("未知子命令应转发");
/// };
/// assert_eq!(serial, None);
/// assert_eq!(rest, ["shell", "ls"].iter().map(OsString::from).collect::<Vec<_>>());
/// ```
pub fn classify(args: &[OsString], is_known: impl Fn(&str) -> bool) -> Route {
    let mut serial = None;
    let mut i = 0;
    while i < args.len() {
        // 非 UTF-8 token 不可能是 adbx 子命令，按 adb 调用透传
        let Some(tok) = args[i].to_str() else {
            return forward(serial, &args[i..]);
        };
        if tok == "-s" || tok == "--serial" {
            // 空格分隔形式：值在下一个 token；缺失时交回 clap 报「需要值」错误
            let Some(value) = args.get(i + 1) else {
                return Route::Own;
            };
            serial = Some(value.clone());
            i += 2;
        } else if let Some(value) = tok.strip_prefix("--serial=") {
            serial = Some(OsString::from(value));
            i += 1;
        } else if let Some(value) = tok.strip_prefix("-s").filter(|v| !v.is_empty()) {
            // 短旗标连写形式（-semu），与 clap 对短旗标取值的行为一致
            serial = Some(OsString::from(value));
            i += 1;
        } else if tok == "--" {
            // 分隔符：只影响路由判断，不随转发传递
            i += 1;
        } else if matches!(tok, "-h" | "--help" | "-V" | "--version") {
            // adbx 自身的帮助/版本旗标（clap 内建），由 clap 处理
            return Route::Own;
        } else if is_known(tok) {
            // adbx 已知子命令（含 clap 内建 help）：走 clap 流程
            return Route::Own;
        } else {
            // 其余一律视为 adb 调用：未知子命令（shell/devices…）或 adb 旗标（-d/-e…）
            return forward(serial, &args[i..]);
        }
    }
    // 扫描完没有任何子命令 token（空参数或仅 -s）：由 clap 报「缺子命令」
    Route::Own
}

/// 组装 [`Route::Forward`]：serial 已捕获，rest 从 `start` 起原样复制。
fn forward(serial: Option<OsString>, rest: &[OsString]) -> Route {
    Route::Forward {
        serial,
        rest: rest.to_vec(),
    }
}

/// 透传执行入口：以 adb 替换本进程运行，行为见 [`adb::exec_adb`]。
///
/// 成功时本函数不返回（unix exec 进程替换），stdio/信号/退出码与直接
/// 调 adb 一致；adb 缺失时返回 Err。
pub fn run(serial: Option<&OsStr>, args: &[OsString]) -> Result<()> {
    adb::exec_adb(serial, args)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{Route, classify};

    /// 以与 main.rs 一致的已知子命令集合构造路由结果（含 clap 内建 help）。
    fn route(args: &[&str]) -> Route {
        let os: Vec<OsString> = args.iter().map(OsString::from).collect();
        classify(&os, |tok| {
            matches!(
                tok,
                "stop" | "clear" | "restart" | "uninstall" | "info" | "browse" | "doctor" | "help"
            )
        })
    }

    fn os_vec(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn empty_args_route_to_own() {
        assert_eq!(route(&[]), Route::Own);
    }

    #[test]
    fn serial_only_without_subcommand_routes_to_own() {
        assert_eq!(route(&["-s", "emulator-5554"]), Route::Own);
    }

    #[test]
    fn missing_serial_value_routes_to_own() {
        // 缺值时交回 clap，报出「需要值」而非透传给 adb
        assert_eq!(route(&["-s"]), Route::Own);
        assert_eq!(route(&["--serial"]), Route::Own);
    }

    #[test]
    fn known_subcommand_routes_to_own() {
        assert_eq!(route(&["stop", "wechat"]), Route::Own);
    }

    #[test]
    fn builtin_help_subcommand_routes_to_own() {
        assert_eq!(route(&["help"]), Route::Own);
    }

    #[test]
    fn help_and_version_flags_route_to_own() {
        for flag in ["-h", "--help", "-V", "--version"] {
            assert_eq!(route(&[flag]), Route::Own, "旗标 {flag} 应由 adbx 处理");
        }
    }

    #[test]
    fn unknown_subcommand_forwards_rest_verbatim() {
        assert_eq!(
            route(&["shell", "ls", "-l"]),
            Route::Forward {
                serial: None,
                rest: os_vec(&["shell", "ls", "-l"])
            }
        );
    }

    #[test]
    fn spaced_serial_is_captured_and_normalized() {
        assert_eq!(
            route(&["-s", "emu", "devices"]),
            Route::Forward {
                serial: Some(OsString::from("emu")),
                rest: os_vec(&["devices"])
            }
        );
    }

    #[test]
    fn long_serial_with_equals_is_normalized() {
        assert_eq!(
            route(&["--serial=emu", "devices"]),
            Route::Forward {
                serial: Some(OsString::from("emu")),
                rest: os_vec(&["devices"])
            }
        );
    }

    #[test]
    fn joined_short_serial_is_normalized() {
        assert_eq!(
            route(&["-semu", "devices"]),
            Route::Forward {
                serial: Some(OsString::from("emu")),
                rest: os_vec(&["devices"])
            }
        );
    }

    #[test]
    fn leading_adb_flag_forwards_verbatim() {
        // -d 是 adb 的旗标，adbx 不认识，按「未命中」透传
        assert_eq!(
            route(&["-d", "shell", "echo", "hi"]),
            Route::Forward {
                serial: None,
                rest: os_vec(&["-d", "shell", "echo", "hi"])
            }
        );
    }

    #[test]
    fn adb_flag_after_serial_forwards_with_normalized_serial() {
        assert_eq!(
            route(&["-s", "emu", "-d", "shell"]),
            Route::Forward {
                serial: Some(OsString::from("emu")),
                rest: os_vec(&["-d", "shell"])
            }
        );
    }

    #[test]
    fn double_dash_is_consumed_not_forwarded() {
        assert_eq!(
            route(&["--", "shell", "ls"]),
            Route::Forward {
                serial: None,
                rest: os_vec(&["shell", "ls"])
            }
        );
    }

    #[test]
    fn tokens_after_first_command_are_not_reinterpreted() {
        // shell 之后的 --serial 是 adb 侧的参数，必须原样透传
        assert_eq!(
            route(&["shell", "--serial", "x"]),
            Route::Forward {
                serial: None,
                rest: os_vec(&["shell", "--serial", "x"])
            }
        );
    }
}
