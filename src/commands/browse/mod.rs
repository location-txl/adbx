//! `adbx browse` 子命令：TUI 浏览设备文件系统，支持键盘/鼠标操作、Space 标记、
//! Enter 批量拉取到本地，`v` 预览、`e` 编辑 UTF-8 文本，`/` 现场筛选、
//! `S`/`O`（大小写均可）切换排序键与方向，`M`/`m` 新建文件夹。
//!
//! 模块分层（自底向上，与设备无关的纯函数在前）：
//!
//! * [`model`] —— 目录条目数据模型与 `ls -l` 解析、排序、筛选（纯函数）
//! * [`paths`] —— 设备端路径运算与拉取清单整理（纯函数）
//! * [`app`] —— App 状态机与目录加载（唯一的 adb ls 出口）
//! * [`tui`] —— 事件循环、批量拉取与渲染
//!
//! adb 调用统一经 [`crate::adb`]，本模块不直接 spawn 进程。

mod app;
mod model;
mod paths;
mod tui;

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

/// `browse` 子命令入口。
///
/// * `serial` - 目标设备 serial（可为 None，由 adb 自行选择唯一设备）
/// * `path` - 设备端起始目录（clap 默认 `/sdcard`），须为以 `/` 开头的绝对路径
/// * `output` - 本地输出目录；None 表示当前工作目录，指定时不存在会自动创建
///
/// 非交互终端（stdout 被重定向）直接返回 Err；起始目录加载失败直接返回 Err
/// （不进 TUI）。副作用：会执行 adb shell ls / adb pull，并在本地目录写文件；
/// 本地同名文件被覆盖、同名目录合并。
pub fn run(serial: Option<&str>, path: &str, output: Option<&str>) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!("browse 需要交互式终端，请在真实终端中运行");
    }
    // 相对路径只是碰巧依赖 adbd 工作目录在 /，明确拒绝，保证 cwd 恒为绝对路径
    if !path.starts_with('/') {
        bail!("起始目录须为设备端绝对路径（以 / 开头），如 /sdcard");
    }
    // 解析输出目录：-o 指定则自动创建，默认当前工作目录
    let out_dir: PathBuf = match output {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("创建输出目录失败：{}", dir.display()))?;
            dir.canonicalize()
                .with_context(|| format!("解析输出目录失败：{}", dir.display()))?
        }
        None => std::env::current_dir().context("获取当前工作目录失败")?,
    };

    let mut app = app::App::new(serial, &paths::normalize_path(path))?;
    // ratatui::run 自动完成 raw mode、备用屏、退出恢复与 panic hook
    ratatui::run(|terminal| tui::event_loop(terminal, &mut app, &out_dir))?;

    // 终端已恢复为普通模式，按会话累计结果打印摘要
    if app.pulled_total > 0 || app.failed_total > 0 {
        let mark = if app.failed_total == 0 { "✓" } else { "✗" };
        println!(
            "{mark} 拉取完成：成功 {} 项、失败 {} 项，输出目录 {}",
            app.pulled_total,
            app.failed_total,
            out_dir.display()
        );
    }
    Ok(())
}
