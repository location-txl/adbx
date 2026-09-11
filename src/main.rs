//! adbx 命令行入口：clap 参数定义与子命令分发。
//!
//! clap 解析前先做透传路由（[`commands::passthrough`]）：未被 adbx
//! 命中的子命令/旗标原样转发给 adb，adbx 是 adb 的超集。

use std::ffi::OsString;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

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
    /// 目标设备 serial，转发给 adb -s
    #[arg(short = 's', long, global = true)]
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
    /// 检查 adb 是否已安装并显示版本
    Doctor,
}

fn main() {
    // clap 解析前先路由：未命中 adbx 子命令的调用原样透传给 adb
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let result = match commands::passthrough::classify(&args, is_known_subcommand) {
        Route::Forward { serial, rest } => commands::passthrough::run(serial.as_deref(), &rest),
        Route::Own => run(),
    };
    if let Err(err) = result {
        eprintln!("✗ {err}");
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

/// 解析命令行参数并分发到对应子命令。
fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Stop { keywords } => commands::stop::run(cli.serial.as_deref(), &keywords),
        Command::Clear { keywords } => commands::clear::run(cli.serial.as_deref(), &keywords),
        Command::Restart { keywords } => commands::restart::run(cli.serial.as_deref(), &keywords),
        Command::Uninstall { yes, keywords } => {
            commands::uninstall::run(cli.serial.as_deref(), &keywords, yes)
        }
        Command::Info { keywords } => commands::info::run(cli.serial.as_deref(), &keywords),
        Command::Browse { path, output } => {
            commands::browse::run(cli.serial.as_deref(), &path, output.as_deref())
        }
        // doctor 检查的是 adb 本身而非设备，serial 无意义，忽略
        Command::Doctor => commands::doctor::run(),
    }
}
