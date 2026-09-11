//! 事件循环、批量拉取与渲染：`browse` 的 TUI 层。
//!
//! adb 调用只发生在批量拉取路径（`du -s` 统计与逐项 pull）与新建文件夹
//! （`shell mkdir`，经 [`crate::adb`]），状态变更全部走 [`super::app`] 的方法。

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use super::app::{App, StatusLine};
use super::model::{Entry, EntryKind, SortKey};
use super::paths::{base_name, dup_base_names, filter_covered, join_path, validate_dir_name};
use crate::adb;

/// 是否退出/取消键（q/Esc/Ctrl-C），与底部帮助栏文案保持一致。
fn is_quit_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('q') | KeyCode::Esc)
        || (code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL))
}

/// 非阻塞检查用户是否按了取消键：读掉所有就绪事件，只认 [`is_quit_key`]，
/// 其余（方向键、Resize 等）静默丢弃。终端事件读取出错按未取消处理并停止轮询。
fn cancel_requested() -> bool {
    while let Ok(true) = event::poll(Duration::ZERO) {
        match event::read() {
            Ok(Event::Key(k))
                if k.kind == KeyEventKind::Press && is_quit_key(k.code, k.modifiers) =>
            {
                return true;
            }
            Ok(_) => {}
            // 事件源损坏时继续轮询只会空转，直接视为未取消
            Err(_) => return false,
        }
    }
    false
}

/// TUI 主循环：绘制一帧 → 阻塞等按键 → 分发。
/// 只有终端 I/O 本身失败（终端不可用）才向上返回 Err。
pub(super) fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    out_dir: &Path,
) -> Result<()> {
    loop {
        terminal.draw(|frame| ui(frame, app))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        // 只响应按下事件：kitty 键盘协议下 Release/Repeat 也会上报
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // 筛选输入模式：可打印字符进筛选词（q 不再退出），Space 仍标记光标条目，
        // Esc 清除筛选返回，仅 Ctrl-C 整体退出；→/← 不响应（先 Enter/Esc 回浏览模式）
        if app.searching {
            match key.code {
                KeyCode::Esc => app.search_cancel(),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Char(' ') => app.toggle_mark(),
                KeyCode::Char(c) => app.search_input(c),
                KeyCode::Backspace => app.search_del_char(),
                KeyCode::Enter => app.search_commit(),
                KeyCode::Up => app.move_cursor(-1),
                KeyCode::Down => app.move_cursor(1),
                _ => {}
            }
            continue;
        }
        // 新建文件夹输入模式：可打印字符（含空格）进名字，Enter 创建，Esc 取消；
        // q 等可打印字符均为名字字符，仅 Ctrl-C 整体退出
        if app.creating {
            match key.code {
                KeyCode::Esc => app.create_cancel(),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Char(c) => app.create_input(c),
                KeyCode::Backspace => app.create_del_char(),
                KeyCode::Enter => mkdir_confirm(app),
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Up => app.move_cursor(-1),
            KeyCode::Down => app.move_cursor(1),
            KeyCode::Right => app.enter_selected(),
            KeyCode::Left | KeyCode::Backspace => app.go_parent(),
            KeyCode::Char(' ') => app.toggle_mark(),
            KeyCode::Char('/') => app.search_begin(),
            KeyCode::Char('M' | 'm') => app.create_begin(),
            KeyCode::Char('S' | 's') => app.cycle_sort(),
            KeyCode::Char('O' | 'o') => app.toggle_desc(),
            KeyCode::Enter => pull_marked(terminal, app, out_dir)?,
            _ if is_quit_key(key.code, key.modifiers) => return Ok(()),
            _ => {}
        }
    }
}

/// 拉取全部已标记条目到 out_dir（逐个阻塞 adb pull，进度按本地落盘字节估算）。
///
/// 拉取前 `du -s` 一次性统计全部目标大小（查不到的项为 0 → 不显示百分比），
/// 并检测落点冲突（同批同名、本地已存在同名），⚠ 提示但不阻断；
/// 拉取中轮询本地落点大小换算 0-100 百分比，重绘"拉取中 (i/N) 名字 · P%"，
/// 同时轮询按键，q/Esc/Ctrl-C 立即杀掉当前 adb pull 并中止批量（未完成项的
/// 标记保留，可再次 Enter 重试；被杀项本地可能残留半截文件）；
/// 每项结束后追加 ✓/✗ 到 status；单项失败不中断整体。
/// 全部完成后清空标记（标记是"待办清单"语义，取消中止时不清空），
/// 并丢弃拉取期间积压的按键，防止恢复后光标乱跳。
fn pull_marked(terminal: &mut DefaultTerminal, app: &mut App, out_dir: &Path) -> Result<()> {
    if app.marked.is_empty() {
        app.status = vec![StatusLine::Err(
            "未标记任何条目，先按 Space 标记".to_owned(),
        )];
        return Ok(());
    }
    let targets = filter_covered(&app.marked);
    app.status.clear();
    app.error = None;
    // 落点冲突提示（只提示不阻断），同样计入 8 条上限，防止挤占列表区
    for name in dup_base_names(&targets) {
        app.push_status(StatusLine::Warn(format!(
            "⚠ 多个同名条目 {name}，将合并/覆盖到同一路径"
        )));
    }
    let mut warned_local = HashSet::new();
    for name in targets.iter().map(|p| base_name(p)) {
        if out_dir.join(name).exists() && warned_local.insert(name.to_owned()) {
            app.push_status(StatusLine::Warn(format!(
                "⚠ 本地已存在 {name}，将被覆盖/合并"
            )));
        }
    }
    let total = targets.len();
    // 一次性统计所有目标大小（du，KB 粒度），拉取中据此换算百分比
    app.pulling = Some("正在统计待拉取内容大小…".to_owned());
    terminal.draw(|frame| ui(frame, app))?;
    let totals = adb::du_totals(app.serial.as_deref(), &targets);
    let mut cancelled = false;
    for (i, remote) in targets.iter().enumerate() {
        let name = base_name(remote);
        app.pulling = Some(format!("({i}/{total}) {name}"));
        terminal.draw(|frame| ui(frame, app))?;
        let local_path = out_dir.join(name);
        // serial 先拷出，避免首参对 app 的不可变借用与闭包里的可变借用冲突
        let serial = app.serial.clone();
        let result = adb::pull_with_progress(
            serial.as_deref(),
            remote,
            out_dir,
            totals[i],
            cancel_requested,
            |pct| {
                app.pulling = Some(format!("({i}/{total}) {name} · {pct}%"));
                // 闭包里无法 ?，draw 失败先吞掉，pull 结束后的外层重绘会统一报
                let _ = terminal.draw(|frame| ui(frame, app));
            },
        );
        match result {
            Ok(true) => {
                app.push_status(StatusLine::Ok(format!(
                    "{remote} → {}",
                    local_path.display()
                )));
                app.pulled_total += 1;
                // 已完成项即时移出标记；取消中止时未完成项因此得以保留
                app.marked.remove(remote);
            }
            Ok(false) => {
                app.push_status(StatusLine::Warn(format!(
                    "已取消 {remote}，剩余 {} 项未拉取，标记已保留（被杀项本地可能残留半截文件）",
                    total - i - 1
                )));
                cancelled = true;
                break;
            }
            Err(err) => {
                app.push_status(StatusLine::Err(format!("{remote}：{err}")));
                app.failed_total += 1;
            }
        }
    }
    app.pulling = None;
    if !cancelled {
        app.marked.clear();
    }
    // 丢弃阻塞期间积压的按键（含触发取消的那次按键，防止其退出后误触发浏览操作）
    while event::poll(Duration::ZERO)? {
        let _ = event::read()?;
    }
    Ok(())
}

/// Enter（新建模式）：校验名字 → adb shell mkdir → 刷新当前目录并定位新
/// 文件夹。名字非法或与现有条目同名时发中文提示、停留输入模式以便改名；
/// adb 失败同样停留可重试；仅成功或 Esc 才退出输入模式。
/// 不做"创建中"中间帧：mkdir 是单次 adb 往返（du -s 统计才需要先画一帧）。
fn mkdir_confirm(app: &mut App) {
    let name = app.create_name.clone();
    if let Err(msg) = validate_dir_name(&name) {
        app.push_status(StatusLine::Err(msg));
        return;
    }
    // 同名预检：文件或目录都会让 mkdir 失败，本地一轮检查省一次 adb 往返
    if app.entries.iter().any(|e| e.name == name) {
        app.push_status(StatusLine::Err(format!("当前目录已存在同名条目「{name}」")));
        return;
    }
    let path = join_path(&app.cwd, &name);
    match adb::shell_mkdir(app.serial.as_deref(), &path) {
        Ok(()) => {
            app.creating = false;
            app.create_name.clear();
            if app.reload(&name) {
                app.push_status(StatusLine::Ok(format!("已创建 {path}")));
            }
            // reload 失败：switch_dir 已写 error 横幅，列表停留原地
        }
        Err(err) => app.push_status(StatusLine::Err(format!("创建 {path} 失败：{err}"))),
    }
}

/// 渲染一帧：列表区（自适应）/ 状态区（仅有内容时出现）/ 帮助栏（固定 1 行）。
fn ui(frame: &mut Frame, app: &mut App) {
    let status = status_content(app);
    let status_h: u16 = if status.is_empty() {
        0
    } else {
        (status.len() + 2) as u16
    };
    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(status_h),
        Constraint::Length(1),
    ])
    .split(frame.area());

    render_list(frame, app, chunks[0]);
    if !status.is_empty() {
        render_status(frame, status, chunks[1], app.error.is_some());
    }
    render_help(frame, chunks[2], help_text(app.creating, app.searching));
}

/// 汇总状态区内容：错误横幅、逐条状态行（按 [`StatusLine`] 变体定前缀/颜色）、
/// 拉取中提示。返回行列表，为空则状态区整体不渲染；
/// 是否含错误由调用方读 `app.error` 判断。
fn status_content(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(err) = &app.error {
        lines.push(Line::from(vec![
            Span::styled("✗ ", Style::new().fg(Color::Red)),
            Span::raw(err.clone()),
        ]));
    }
    for line in &app.status {
        match line {
            StatusLine::Warn(msg) => lines.push(Line::from(Span::styled(
                msg.clone(),
                Style::new().fg(Color::Yellow),
            ))),
            StatusLine::Ok(msg) => lines.push(Line::from(vec![
                Span::styled("✓ ", Style::new().fg(Color::Green)),
                Span::raw(msg.clone()),
            ])),
            StatusLine::Err(msg) => lines.push(Line::from(vec![
                Span::styled("✗ ", Style::new().fg(Color::Red)),
                Span::raw(msg.clone()),
            ])),
        }
    }
    if let Some(name) = &app.pulling {
        lines.push(Line::from(format!("拉取中 {name}…")));
    }
    lines
}

/// 渲染列表区：边框标题为当前路径、已标记数、筛选态（筛选词/匹配数）与排序档；
/// 空目录与无匹配给行内提示。
fn render_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let vis = app.visible();
    let mut title = format!(" {} ── 已标记 {}", app.cwd, app.marked.len());
    if app.creating {
        title.push_str(&format!(" · 新建:{}▏", app.create_name));
    }
    if app.searching || app.filter.is_some() {
        let caret = if app.searching { "▏" } else { "" };
        let word = app.filter.as_deref().unwrap_or("");
        title.push_str(&format!(
            " · 筛选:{word}{caret} ({}/{})",
            vis.len(),
            app.entries.len()
        ));
    }
    title.push_str(&format!(
        " · 排序:{}{}",
        app.sort.label(),
        if app.desc { "↓" } else { "↑" }
    ));
    let block = Block::bordered().title(title);
    if vis.is_empty() {
        let hint = if app.filter.as_deref().is_some_and(|w| !w.trim().is_empty()) {
            "(无匹配条目)"
        } else {
            "(空目录)"
        };
        frame.render_widget(
            Paragraph::new(hint)
                .style(Style::new().fg(Color::DarkGray))
                .block(block),
            area,
        );
        return;
    }
    let sel = app.state.selected();
    let show_date = app.sort == SortKey::Date;
    let items: Vec<ListItem> = vis
        .iter()
        .enumerate()
        .map(|(pos, &i)| {
            let entry = &app.entries[i];
            let marked = app.marked.contains(&join_path(&app.cwd, &entry.name));
            ListItem::new(format_row(entry, sel == Some(pos), marked, show_date))
        })
        .collect();
    frame.render_stateful_widget(List::new(items).block(block), area, &mut app.state);
}

/// 渲染状态区：标题按内容区分（有错误显示"错误"，否则"拉取结果"）。
fn render_status(frame: &mut Frame, lines: Vec<Line<'static>>, area: Rect, is_error: bool) {
    let title = if is_error {
        " 错误 "
    } else {
        " 拉取结果 "
    };
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        area,
    );
}

/// 帮助栏文案：浏览 / 筛选输入 / 新建输入三版（纯函数，可单测）。
/// 事件循环保证 creating 与 searching 互斥，creating 判断在前。
fn help_text(creating: bool, searching: bool) -> &'static str {
    if creating {
        "输入新文件夹名（可含空格） · Enter 创建 · Esc 取消 · Ctrl-C 退出"
    } else if searching {
        "输入筛选词（大小写不敏感） · ↑↓ 移动 · Enter 保留筛选 · Esc 清除 · Ctrl-C 退出"
    } else {
        "↑↓ 移动 · → 进入 · ←/⌫ 返回 · / 筛选 · S/s 排序 · O/o 方向 · M/m 新建 · Space 标记 · Enter 拉取 · q/Esc 退出（拉取中=取消）"
    }
}

/// 渲染底部帮助栏，文案由 [`help_text`] 按当前模式选择。
fn render_help(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(
        Paragraph::new(text).style(Style::new().fg(Color::DarkGray)),
        area,
    );
}

/// 生成列表行 `▸ [✓] name/  1.2 MB[ · 2024-06-01 08:15]`（纯函数，可单测）。
///
/// `▸` 标光标行，`[✓]`/`[ ]` 标标记状态，`/` 与 `@` 为目录与符号链接后缀；
/// 前两段黄色高亮，大小与日期淡灰色；日期仅在日期排序时展示（`show_date`）。
fn format_row(entry: &Entry, selected: bool, marked: bool, show_date: bool) -> Line<'static> {
    let accent = Style::new().fg(Color::Yellow);
    let cursor = if selected { "▸ " } else { "  " };
    let check = if marked { "[✓] " } else { "[ ] " };
    let suffix = match entry.kind {
        EntryKind::Dir => "/",
        EntryKind::Symlink => "@",
        _ => "",
    };
    let gray = Style::new().fg(Color::DarkGray);
    let mut spans = vec![
        Span::styled(cursor, accent),
        Span::styled(check, accent),
        Span::raw(format!("{}{suffix}", entry.name)),
        Span::styled(format!("  {}", human_size(entry.size)), gray),
    ];
    if show_date {
        spans.push(Span::styled(format!(" · {}", entry.date), gray));
    }
    Line::from(spans)
}

/// 字节数转人类可读：`12 B`、`1.2 KB`、`1.4 GB`（≥1024 进位，保留 1 位小数）。
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::super::app::app_with_entries;
    use super::super::model::mk;
    use super::*;

    #[test]
    fn quit_keys_match_help_text() {
        // 帮助栏承诺的退出/取消键：q、Esc、Ctrl-C；裸 c 与其他键不算
        assert!(is_quit_key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(is_quit_key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(is_quit_key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!is_quit_key(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(!is_quit_key(KeyCode::Char('x'), KeyModifiers::NONE));
    }

    #[test]
    fn help_text_covers_three_input_modes() {
        // 浏览版含全部按键提示（含新建）
        let browse = help_text(false, false);
        assert!(browse.contains("M/m 新建"));
        assert!(browse.contains("Enter 拉取"));
        // 筛选版
        assert!(help_text(false, true).contains("筛选词"));
        // 新建版优先于筛选（事件循环保证两者互斥，此处为防御性排序）
        assert!(help_text(true, false).contains("新文件夹名"));
        assert!(help_text(true, true).contains("新文件夹名"));
    }

    #[test]
    fn format_row_shows_marks_and_suffixes() {
        let text = |line: &Line<'_>| -> String {
            line.spans.iter().map(|s| s.content.to_string()).collect()
        };
        let dir = mk("DCIM", EntryKind::Dir, 4096, 0);
        let row = text(&format_row(&dir, true, true, false));
        assert!(row.contains("▸"));
        assert!(row.contains("[✓]"));
        assert!(row.contains("DCIM/"));

        let file = mk("notes.txt", EntryKind::File, 12, 0);
        let row = text(&format_row(&file, false, false, false));
        assert!(row.contains("[ ]"));
        assert!(!row.contains("notes.txt/"));
        assert!(row.contains("12 B"));
        // 日期列仅在日期排序时展示
        let file = Entry {
            date: "2024-06-01 08:15".into(),
            ..file
        };
        assert!(!text(&format_row(&file, false, false, false)).contains("2024-"));
        assert!(text(&format_row(&file, false, false, true)).contains(" · 2024-06-01 08:15"));
    }

    #[test]
    fn formats_human_sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1048576), "1.0 MB");
        assert_eq!(human_size(1_500_000_000), "1.4 GB");
    }

    #[test]
    fn status_renders_warning_lines_in_yellow() {
        // 直接构造 App（同模块可见私有字段），避免为渲染测试发起 adb 调用
        let mut app = app_with_entries(vec![]);
        app.status = vec![
            StatusLine::Ok("ok".into()),
            StatusLine::Warn("⚠ 本地已存在 DCIM，将被覆盖/合并".into()),
        ];
        let lines = status_content(&app);
        assert_eq!(lines.len(), 2);
        // 成败行：✓/✗ 前缀 + 正文，两个 Span
        assert_eq!(lines[0].spans.len(), 2);
        // 警告行按 Warn 变体整行黄色，单 Span，不叠加 ✓/✗ 前缀
        assert_eq!(lines[1].spans.len(), 1);
        assert!(lines[1].spans[0].content.contains("⚠"));
        assert_eq!(lines[1].spans[0].style, Style::new().fg(Color::Yellow));
    }
}
