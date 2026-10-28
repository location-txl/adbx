//! adbx 命令行入口：clap 参数定义与子命令分发。
//!
//! clap 解析前先做透传路由（[`commands::passthrough`]）：未被 adbx
//! 命中的子命令/旗标原样转发给 adb，adbx 是 adb 的超集。

use std::ffi::OsString;

use anyhow::{Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, parser::ValueSource};

use adbx::commands;
use adbx::commands::passthrough::Route;

/// adb 扩展 CLI：在 adb 之上提供模糊包名匹配等增强能力；
/// 未被识别的子命令/旗标会原样转发给 adb
#[derive(Parser)]
#[command(
    name = "adbx",
    version,
    after_help = "未被识别的子命令/旗标将静默转发给 adb（stdio 与退出码一致）"
)]
struct Cli {
    /// 目标设备 serial，转发给 adb -s；缺省时读取 ANDROID_SERIAL 环境变量
    #[arg(short = 's', long, global = true, env = "ANDROID_SERIAL")]
    serial: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 按关键词模糊匹配包名并强制停止应用（am force-stop），多关键词为 AND 关系
    Stop {
        /// 包名关键词，要求包名同时包含所有关键词
        #[arg(required = true)]
        keywords: Vec<String>,
    },
    /// 按关键词模糊匹配包名并清除应用数据（pm clear），多关键词为 AND 关系
    Clear {
        /// 包名关键词，要求包名同时包含所有关键词
        #[arg(required = true)]
        keywords: Vec<String>,
    },
    /// 按关键词模糊匹配包名并重启应用（force-stop 后重新拉起），多关键词为 AND 关系
    Restart {
        /// 包名关键词，要求包名同时包含所有关键词
        #[arg(required = true)]
        keywords: Vec<String>,
    },
    /// 按关键词模糊匹配包名并卸载应用（pm uninstall），多关键词为 AND 关系
    Uninstall {
        /// 跳过二次确认，匹配到后直接卸载
        #[arg(short, long)]
        yes: bool,
        /// 包名关键词，要求包名同时包含所有关键词
        #[arg(required = true)]
        keywords: Vec<String>,
    },
    /// 按关键词模糊匹配包名并启用应用（pm enable），多关键词为 AND 关系
    Enable {
        /// 包名关键词，要求包名同时包含所有关键词
        #[arg(required = true)]
        keywords: Vec<String>,
    },
    /// 按关键词模糊匹配包名并禁用应用（pm disable-user，固定主用户 0），多关键词为 AND 关系
    Disable {
        /// 包名关键词，要求包名同时包含所有关键词
        #[arg(required = true)]
        keywords: Vec<String>,
    },
    /// 按关键词模糊匹配包名并展示完整包名及版本信息（dumpsys package），多命中时全部展示
    Info {
        /// 包名关键词，要求包名同时包含所有关键词
        #[arg(required = true)]
        keywords: Vec<String>,
    },
    /// TUI 浏览设备文件系统：键盘导航、Space 标记、Enter 批量拉取到本地目录
    Browse {
        /// 设备端起始目录
        #[arg(default_value = "/sdcard")]
        path: String,
        /// 本地输出目录，默认当前工作目录
        #[arg(short, long)]
        output: Option<String>,
    },
    /// 绑定设备到当前终端：之后本终端内的 adbx 和 adb 命令默认作用于该设备（需终端集成）
    Use {
        /// 设备 serial 或唯一片段（如端口号 5554）；缺省时自动选择唯一可用设备
        device: Option<String>,
    },
    /// 解除当前终端的设备绑定（需终端集成）
    Unuse,
    /// 显示当前终端绑定的设备及连接状态
    Current,
    /// 输出终端集成脚本（zsh / bash / powershell），用于 adbx use / unuse
    Init {
        /// 目标 shell：zsh、bash 或 powershell
        shell: String,
    },
    /// 检查 adb 是否已安装并显示版本
    Doctor,
    /// adb 缺失时下载并安装官方 Android SDK Platform-Tools
    #[command(name = "adb-install")]
    AdbInstall,
    /// 解析设备标识并输出 serial（终端集成内部使用，不出现在帮助中）
    #[command(name = "__resolve-device", hide = true)]
    ResolveDevice {
        /// 设备 serial 或唯一片段（如端口号 5554）；缺省时自动选择唯一可用设备
        device: Option<String>,
    },
}

fn main() {
    // clap 解析前先路由：未命中 adbx 子命令的调用原样透传给 adb
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let result = match commands::passthrough::classify(&args, is_known_subcommand) {
        Route::Forward { serial, rest } => commands::passthrough::run(serial.as_deref(), &rest),
        Route::Own => run(),
    };
    if let Err(err) = result {
        // {err:#} 以 ": " 连接整个错误链，让底层 io::Error（如 os error 5/32）可见
        eprintln!("✗ {err:#}");
        std::process::exit(1);
    }
}

/// 判断 token 是否为 adbx 已知子命令（含 clap 内建的 help 及其别名）。
///
/// 从 clap 定义运行时自省，新增子命令后无需维护第二份清单。
fn is_known_subcommand(token: &str) -> bool {
    Cli::command()
        .get_subcommands()
        .any(|sc| sc.get_name() == token || sc.get_all_aliases().any(|alias| alias == token))
}

/// 归一 clap 解析出的 serial：空环境变量按未设置处理，显式空参数报错。
///
/// 保留参数来源，避免脚本的空 `-s "$DEVICE"` 意外触发自动设备选择。
fn normalize_serial(serial: Option<String>, source: Option<ValueSource>) -> Result<Option<String>> {
    if serial.as_deref() == Some("") && source == Some(ValueSource::CommandLine) {
        bail!("-s/--serial 不能为空，请提供有效的设备 serial");
    }
    Ok(serial.filter(|s| !s.is_empty()))
}

/// 解析命令行参数并分发到对应子命令。
fn run() -> Result<()> {
    let matches = Cli::command().get_matches();
    let mut cli = Cli::from_arg_matches(&matches)?;
    cli.serial = normalize_serial(cli.serial, matches.value_source("serial"))?;
    // 设备类命令的 serial 统一解析：显式 -s / ANDROID_SERIAL 优先，
    // 均缺省时自动选择唯一可用设备（多台/零台时列出清单报错，见 device::auto_serial）。
    // 采用设备命令白名单而非排除清单：新增命令默认不需要设备（安全方向），
    // 忘记登记的设备命令只退回 adb 自行选择，不会让主机命令凭空要求设备。
    let serial = match cli.command {
        Command::Stop { .. }
        | Command::Clear { .. }
        | Command::Restart { .. }
        | Command::Uninstall { .. }
        | Command::Enable { .. }
        | Command::Disable { .. }
        | Command::Info { .. }
        | Command::Browse { .. } => commands::device::auto_serial(cli.serial.as_deref())?,
        _ => cli.serial.clone(),
    };
    let serial = serial.as_deref();
    match cli.command {
        Command::Stop { keywords } => commands::stop::run(serial, &keywords),
        Command::Clear { keywords } => commands::clear::run(serial, &keywords),
        Command::Restart { keywords } => commands::restart::run(serial, &keywords),
        Command::Uninstall { yes, keywords } => commands::uninstall::run(serial, &keywords, yes),
        Command::Enable { keywords } => commands::enable::run(serial, &keywords),
        Command::Disable { keywords } => commands::disable::run(serial, &keywords),
        Command::Info { keywords } => commands::info::run(serial, &keywords),
        Command::Browse { path, output } => commands::browse::run(serial, &path, output.as_deref()),
        // use/unuse 需要修改当前 Shell 的环境变量，只能由终端集成函数完成；
        // 二进制侧给出安装指引（装好集成后这两个调用会被函数拦截，不会走到这里）
        Command::Use { .. } => commands::device::integration_hint("use"),
        Command::Unuse => commands::device::integration_hint("unuse"),
        Command::Current => commands::device::current(serial),
        Command::Init { shell } => commands::init::run(&shell),
        // doctor 检查的是 adb 本身而非设备，serial 无意义，忽略
        Command::Doctor => commands::doctor::run(),
        // adb-install 只修改本机 adb 环境，serial 无意义，忽略
        Command::AdbInstall => commands::adb_install::run(),
        Command::ResolveDevice { device } => commands::device::resolve(device.as_deref()),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_serial;
    use clap::parser::ValueSource;

    #[test]
    fn serial_normalization_distinguishes_empty_flag_from_empty_env() {
        assert!(normalize_serial(Some(String::new()), Some(ValueSource::CommandLine)).is_err());
        assert_eq!(
            normalize_serial(Some(String::new()), Some(ValueSource::EnvVariable)).unwrap(),
            None
        );
        assert_eq!(
            normalize_serial(Some("emu".into()), Some(ValueSource::CommandLine))
                .unwrap()
                .as_deref(),
            Some("emu")
        );
        assert_eq!(normalize_serial(None, None).unwrap(), None);
    }
}
