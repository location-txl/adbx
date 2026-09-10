//! adbx 命令行入口：clap 参数定义与子命令分发。

use anyhow::Result;
use clap::{Parser, Subcommand};

use adbx::commands;

/// adb 扩展 CLI：在 adb 之上提供模糊包名匹配等增强能力
#[derive(Parser)]
#[command(name = "adbx", version)]
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
    /// 检查 adb 是否已安装并显示版本
    Doctor,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("✗ {err}");
        std::process::exit(1);
    }
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
        // doctor 检查的是 adb 本身而非设备，serial 无意义，忽略
        Command::Doctor => commands::doctor::run(),
    }
}
