//! 事件循环、批量拉取与渲染：`browse` 的 TUI 层。
//!
//! adb 调用只发生在批量拉取路径（`du -s` 统计与逐项 pull）、新建文件夹
//! （`shell mkdir`）、文件预览和文本编辑保存；所有设备 I/O 均经 [`crate::adb`]，
//! 状态变更全部走 [`super::app`] 的方法。

use std::collections::HashSet;
use std::io::{self, Cursor};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use image::io::Reader as ImageReader;
use image::{ImageFormat, RgbImage, imageops};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use tui_textarea::{CursorMove, TextArea};
use unicode_width::UnicodeWidthChar;

use super::app::{App, StatusLine};
use super::model::{Entry, EntryKind, SortKey};
use super::paths::{base_name, dup_base_names, filter_covered, join_path, validate_dir_name};
use crate::adb;

/// 预览和编辑最多读取的字节数；多读取一个字节用于识别超限文件。
const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;

/// 控件滚动坐标使用 u16；行尾光标的 `cursor + 1` 还需保留一列。
const MAX_EDITOR_COLUMNS: usize = u16::MAX as usize - 1;
/// 保证最后一行的光标及其后一行边界都能用 u16 表示。
const MAX_EDITOR_LINES: usize = u16::MAX as usize;
/// 容量校验与控件显示共用的制表位间隔。
const EDITOR_TAB_LENGTH: u8 = 4;

/// 图片预览允许的单边最大尺寸；先于解码检查，避免声明尺寸过大时分配像素缓冲。
const MAX_PREVIEW_IMAGE_DIMENSION: u32 = 4096;

/// 图片预览允许的最大像素数；限制宽高乘积，覆盖高宽比极端的图片。
const MAX_PREVIEW_IMAGE_PIXELS: u64 = 4 * 1024 * 1024;

/// 图片解码器允许的最大累计分配；为解码中间缓冲和 RGB 转换保留有限空间。
const MAX_PREVIEW_IMAGE_ALLOC: u64 = 64 * 1024 * 1024;

/// 右侧预览面板的内容状态。
enum Preview {
    /// 正在从设备读取文件时显示的占位状态。
    Loading { name: String, path: String },
    /// 已解码的纯文本内容和当前垂直滚动位置。
    Text(TextPreview),
    /// 已解码的图片内容，绘制时按右侧面板尺寸缩放。
    Image(ImagePreview),
}

/// 文本预览的显示数据。
struct TextPreview {
    /// 文件名，用于右侧面板标题。
    name: String,
    /// 设备端绝对路径，用于判断再次按 `v` 是否关闭当前预览。
    path: String,
    /// 已通过 UTF-8 与控制字符检查的文本。
    content: String,
    /// 跳过的文本行数。
    scroll: u16,
}

/// 图片预览的显示数据。
struct ImagePreview {
    /// 文件名，用于右侧面板标题。
    name: String,
    /// 设备端绝对路径，用于判断再次按 `v` 是否关闭当前预览。
    path: String,
    /// 转换为 RGB 后的图片像素。
    image: RgbImage,
    /// 原始图片宽度，用于标题显示。
    width: u32,
    /// 原始图片高度，用于标题显示。
    height: u32,
}

impl Preview {
    /// 返回预览对应的设备端路径，供 `v` 更新或关闭预览面板。
    fn path(&self) -> &str {
        match self {
            Self::Loading { path, .. }
            | Self::Text(TextPreview { path, .. })
            | Self::Image(ImagePreview { path, .. }) => path,
        }
    }

    /// 按页移动文本预览位置；图片和加载状态不需要滚动。
    fn scroll_page(&mut self, direction: i32) {
        let Self::Text(text) = self else {
            return;
        };
        const PAGE_LINES: u16 = 10;
        if direction < 0 {
            text.scroll = text.scroll.saturating_sub(PAGE_LINES);
        } else {
            text.scroll = text.scroll.saturating_add(PAGE_LINES);
        }
    }

    /// 把文本预览移动到首行或末行。
    fn scroll_edge(&mut self, end: bool) {
        let Self::Text(text) = self else {
            return;
        };
        text.scroll = if end {
            text.content
                .lines()
                .count()
                .saturating_sub(1)
                .min(u16::MAX as usize) as u16
        } else {
            0
        };
    }
}

/// 编辑器保存时使用的换行符格式；混合换行文件按检测到的主格式写回。
#[derive(Clone, Copy)]
enum LineEnding {
    /// Unix 风格换行符。
    Lf,
    /// Windows 风格换行符。
    CrLf,
}

impl LineEnding {
    /// 返回写回文件时使用的换行符字节。
    fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

/// 一个远端文本文件的编辑会话。
///
/// 编辑器只保存打开时的原始快照，不在本地创建副本；写回前重新读取远端内容，
/// 快照不一致时交给事件循环显示覆盖确认，避免静默覆盖其他端的修改。
struct RemoteEditor {
    /// 设备端绝对路径，用于重新读取和写回文件。
    path: String,
    /// 成熟的多行编辑控件，负责光标、Unicode、撤销/重做和鼠标滚动/定位。
    textarea: TextArea<'static>,
    /// 打开时或上次保存后的远端字节快照。
    original_bytes: Vec<u8>,
    /// 写回时保留的常见换行格式。
    line_ending: LineEnding,
    /// 本地编辑内容是否发生过修改。
    dirty: bool,
    /// 当前是否正在等待用户处理未保存或远端冲突提示。
    prompt: Option<EditorPrompt>,
}

/// 编辑器需要用户确认的状态。
#[derive(Clone, Copy)]
enum EditorPrompt {
    /// 按 Esc 离开时仍有本地修改。
    Unsaved,
    /// 保存前发现远端内容已变化。
    RemoteChanged,
}

impl RemoteEditor {
    /// 将编辑器当前内容按原换行格式序列化为待写回字节。
    fn bytes(&self) -> Vec<u8> {
        self.textarea
            .lines()
            .join(self.line_ending.as_str())
            .into_bytes()
    }
}

/// 一帧界面中可被鼠标命中的区域。
#[derive(Clone, Copy, Default)]
struct UiLayout {
    /// 文件列表区域；编辑模式下为空。
    list: Option<Rect>,
    /// 编辑器区域；非编辑模式下为空。
    editor: Option<Rect>,
}

/// 鼠标捕获的生命周期守卫，确保退出 browse 时恢复终端的鼠标模式。
struct MouseCaptureGuard;

impl MouseCaptureGuard {
    /// 启用 crossterm 鼠标事件捕获。
    fn enable() -> Result<Self> {
        execute!(io::stdout(), EnableMouseCapture).context("启用鼠标捕获失败")?;
        Ok(Self)
    }
}

impl Drop for MouseCaptureGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
    }
}

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
    let _mouse_capture = MouseCaptureGuard::enable()?;
    let mut preview: Option<Preview> = None;
    let mut editor: Option<RemoteEditor> = None;
    loop {
        let mut layout = UiLayout::default();
        terminal.draw(|frame| {
            layout = ui(frame, app, preview.as_ref(), editor.as_ref());
        })?;

        let input = event::read()?;
        if let Some(current) = editor.as_mut() {
            let action = handle_editor_event(input, app, current, layout)?;
            if matches!(action, EditorAction::Close) {
                editor = None;
                preview = None;
            }
            continue;
        }

        match input {
            Event::Mouse(mouse) => handle_browse_mouse(mouse, app, layout),
            Event::Key(key) => {
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
                    KeyCode::Right => {
                        app.enter_selected();
                        preview = None;
                    }
                    KeyCode::Left | KeyCode::Backspace => {
                        app.go_parent();
                        preview = None;
                    }
                    KeyCode::Char(' ') => app.toggle_mark(),
                    KeyCode::Char('/') => app.search_begin(),
                    KeyCode::Char('M' | 'm') => app.create_begin(),
                    KeyCode::Char('S' | 's') => app.cycle_sort(),
                    KeyCode::Char('O' | 'o') => app.toggle_desc(),
                    KeyCode::Char('v') => toggle_preview(terminal, app, &mut preview)?,
                    KeyCode::Char('e') => {
                        open_editor(app, &mut editor)?;
                        if editor.is_some() {
                            preview = None;
                        }
                    }
                    KeyCode::PageUp => {
                        if let Some(preview) = preview.as_mut() {
                            preview.scroll_page(-1);
                        }
                    }
                    KeyCode::PageDown => {
                        if let Some(preview) = preview.as_mut() {
                            preview.scroll_page(1);
                        }
                    }
                    KeyCode::Home => {
                        if let Some(preview) = preview.as_mut() {
                            preview.scroll_edge(false);
                        }
                    }
                    KeyCode::End => {
                        if let Some(preview) = preview.as_mut() {
                            preview.scroll_edge(true);
                        }
                    }
                    KeyCode::Enter => pull_marked(terminal, app, out_dir, &preview)?,
                    _ if is_quit_key(key.code, key.modifiers) => return Ok(()),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// 编辑器事件处理结果。
enum EditorAction {
    /// 编辑器继续保持打开。
    Stay,
    /// 用户已保存或确认丢弃修改，可以回到文件列表。
    Close,
}

/// 将终端事件交给编辑器；保存和退出等应用级快捷键由本模块处理，
/// 普通按键与滚轮交给成熟的 `tui-textarea-2` 控件。
fn handle_editor_event(
    input: Event,
    app: &mut App,
    editor: &mut RemoteEditor,
    layout: UiLayout,
) -> Result<EditorAction> {
    match input {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            Ok(handle_editor_key(app, editor, key))
        }
        Event::Mouse(mouse) => {
            handle_editor_mouse(editor, mouse, layout);
            Ok(EditorAction::Stay)
        }
        _ => Ok(EditorAction::Stay),
    }
}

/// 处理编辑器的保存、冲突确认、退出和普通文字输入。
fn handle_editor_key(
    app: &mut App,
    editor: &mut RemoteEditor,
    key: ratatui::crossterm::event::KeyEvent,
) -> EditorAction {
    if let Some(prompt) = editor.prompt {
        match prompt {
            EditorPrompt::Unsaved => match key.code {
                KeyCode::Char('s') => save_editor_with_prompt(app, editor, false),
                KeyCode::Char('d') => return EditorAction::Close,
                KeyCode::Esc => editor.prompt = None,
                _ => {}
            },
            EditorPrompt::RemoteChanged => match key.code {
                KeyCode::Char('o') => save_editor_with_prompt(app, editor, true),
                KeyCode::Esc => editor.prompt = None,
                _ => {}
            },
        }
        return EditorAction::Stay;
    }

    if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
        save_editor_with_prompt(app, editor, false);
        return EditorAction::Stay;
    }
    if key.code == KeyCode::Esc {
        // 撤销可能把内容恢复成快照；只在离开时做一次序列化，避免每次输入都复制 10 MiB 文本。
        if editor.dirty && editor.bytes() == editor.original_bytes {
            editor.dirty = false;
        }
        if editor.dirty {
            editor.prompt = Some(EditorPrompt::Unsaved);
            return EditorAction::Stay;
        }
        return EditorAction::Close;
    }

    if apply_editor_input(editor, key) {
        editor.dirty = true;
    }
    EditorAction::Stay
}

/// 在编辑器控件接受按键前限制文本容量。
///
/// 普通字符、Tab 和换行可以直接根据当前位置计算上界；删除、粘贴、撤销/重做等
/// 可能受选区或历史影响的操作则先在克隆控件中执行并校验。这样拒绝的操作不会
/// 进入真实控件的 undo/redo 历史，也不会在下一帧渲染前留下越界状态。
fn apply_editor_input(editor: &mut RemoteEditor, key: KeyEvent) -> bool {
    if let Some(fits) = simple_editor_input_fits(editor, &key) {
        return fits && editor.textarea.input(key);
    }
    if !editor_input_may_modify_text(&key) {
        return editor.textarea.input(key);
    }

    let mut candidate = editor.textarea.clone();
    if !candidate.input(key) || validate_editor_lines(candidate.lines()).is_err() {
        return false;
    }
    editor.textarea = candidate;
    true
}

/// 返回普通字符、Tab 或换行是否仍在编辑器容量内；其他按键返回 `None`。
fn simple_editor_input_fits(editor: &RemoteEditor, key: &KeyEvent) -> Option<bool> {
    let lines = editor.textarea.lines();
    let (row, column) = editor.textarea.cursor();
    let line = lines.get(row)?;
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Enter => Some(lines.len() < MAX_EDITOR_LINES),
        KeyCode::Char('m') if control && !alt => Some(lines.len() < MAX_EDITOR_LINES),
        KeyCode::Char('\n' | '\r') if !control && !alt => Some(lines.len() < MAX_EDITOR_LINES),
        KeyCode::Char(ch) if !control && !alt => {
            let prefix_width = editor_display_width(line.chars().take(column));
            let added_width = if ch == '\t' {
                let tab_length = usize::from(EDITOR_TAB_LENGTH);
                tab_length - prefix_width % tab_length
            } else {
                ch.width().unwrap_or(0)
            };
            let width = editor_display_width(line.chars());
            Some(
                line.chars().count() < MAX_EDITOR_COLUMNS
                    && width.saturating_add(added_width) <= MAX_EDITOR_COLUMNS,
            )
        }
        KeyCode::Tab if !control && !alt => {
            let prefix_width = editor_display_width(line.chars().take(column));
            let tab_length = usize::from(EDITOR_TAB_LENGTH);
            let added_width = tab_length - prefix_width % tab_length;
            let width = editor_display_width(line.chars());
            Some(
                line.chars().count().saturating_add(added_width) <= MAX_EDITOR_COLUMNS
                    && width.saturating_add(added_width) <= MAX_EDITOR_COLUMNS,
            )
        }
        _ => None,
    }
}

/// 判断按键是否可能修改文本但不能用当前位置直接计算结果。
fn editor_input_may_modify_text(key: &KeyEvent) -> bool {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Backspace | KeyCode::Delete => !control,
        KeyCode::Char('d' | 'h') if control || alt => true,
        KeyCode::Char('j' | 'k' | 'r' | 'u' | 'w' | 'x' | 'y') if control && !alt => true,
        _ => false,
    }
}

/// 处理编辑器内的鼠标点击与滚轮；坐标命中和 Unicode 宽度换算由 textarea 库完成。
fn handle_editor_mouse(editor: &mut RemoteEditor, mouse: MouseEvent, layout: UiLayout) {
    let Some(area) = layout.editor else {
        return;
    };
    let position = Position::new(mouse.column, mouse.row);
    if !area.contains(position) {
        return;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((row, column)) = editor.textarea.cursor_at_position(position) {
                editor.textarea.move_cursor(CursorMove::Jump(
                    row.min(u16::MAX as usize) as u16,
                    column.min(u16::MAX as usize) as u16,
                ));
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            editor.textarea.input(mouse);
        }
        _ => {}
    }
}

/// 处理浏览模式下列表的单击选中和滚轮移动。
fn handle_browse_mouse(mouse: MouseEvent, app: &mut App, layout: UiLayout) {
    let Some(area) = layout.list else {
        return;
    };
    let position = Position::new(mouse.column, mouse.row);
    if !area.contains(position) {
        return;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // 列表有一行边框，鼠标首行/末行不对应条目。
            let first_row = area.y.saturating_add(1);
            let last_row = area.y.saturating_add(area.height.saturating_sub(1));
            if position.y < first_row || position.y >= last_row {
                return;
            }
            let row = usize::from(position.y - first_row);
            let index = app.state.offset().saturating_add(row);
            if index < app.visible().len() {
                app.state.select(Some(index));
            }
        }
        MouseEventKind::ScrollUp => app.move_cursor(-3),
        MouseEventKind::ScrollDown => app.move_cursor(3),
        _ => {}
    }
}

/// 打开当前选中的远端 UTF-8 文本文件。
///
/// 目录、特殊文件、图片和含控制字符的二进制文件保持预览/浏览语义，不进入编辑器；
/// 远端读取失败只写入状态区，继续留在文件列表中。
fn open_editor(app: &mut App, editor: &mut Option<RemoteEditor>) -> Result<()> {
    let Some(index) = app.selected_entry_index() else {
        app.push_status(StatusLine::Err("没有选中的条目，无法编辑".to_owned()));
        return Ok(());
    };
    let Some(entry) = app.entries.get(index) else {
        return Ok(());
    };
    if !matches!(entry.kind, EntryKind::File | EntryKind::Symlink) {
        app.push_status(StatusLine::Err(
            "编辑只支持普通文件和文件符号链接".to_owned(),
        ));
        return Ok(());
    }
    let name = entry.name.clone();
    let path = join_path(&app.cwd, &name);
    let bytes = match adb::read_file(app.serial.as_deref(), &path, MAX_FILE_BYTES) {
        Ok(bytes) => bytes,
        Err(err) => {
            app.push_status(StatusLine::Err(format!("读取 {path} 失败：{err}")));
            return Ok(());
        }
    };
    if bytes.len() > MAX_FILE_BYTES {
        app.push_status(StatusLine::Err(format!(
            "文件超过 {} MiB 编辑上限",
            MAX_FILE_BYTES / (1024 * 1024)
        )));
        return Ok(());
    }
    let Some(content) = decode_plain_text(&bytes) else {
        app.push_status(StatusLine::Err(
            "编辑只支持没有控制字符的 UTF-8 纯文本".to_owned(),
        ));
        return Ok(());
    };

    let line_ending = if content.contains("\r\n") {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    };
    let lines = text_lines(&content);
    if let Err(err) = validate_editor_lines(&lines) {
        app.push_status(StatusLine::Err(err.to_string()));
        return Ok(());
    }
    let mut textarea = TextArea::new(lines);
    textarea.set_tab_length(EDITOR_TAB_LENGTH);
    textarea.set_block(Block::bordered().title(format!(" 编辑 · {name} ")));
    *editor = Some(RemoteEditor {
        path,
        textarea,
        original_bytes: bytes,
        line_ending,
        dirty: false,
        prompt: None,
    });
    Ok(())
}

/// 将文本拆为 textarea 行，同时保留文件末尾换行并去除 CRLF 中的 CR。
fn text_lines(content: &str) -> Vec<String> {
    content
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect()
}

/// 校验已拆分、去除 CRLF 中 CR 的文本行，防止控件的 u16 坐标溢出。
///
/// 超过行数、单行字符数或显示列数上限时返回可直接展示的错误；不修改内容。
fn validate_editor_lines(lines: &[String]) -> Result<()> {
    anyhow::ensure!(
        lines.len() <= MAX_EDITOR_LINES,
        "文本超过 {MAX_EDITOR_LINES} 行编辑上限（含末尾空行）"
    );
    for (row, line) in lines.iter().enumerate() {
        let character_count = line.chars().count();
        let width = editor_display_width(line.chars());
        anyhow::ensure!(
            character_count <= MAX_EDITOR_COLUMNS && width <= MAX_EDITOR_COLUMNS,
            "第 {} 行超过 {MAX_EDITOR_COLUMNS} 字符或显示列编辑上限",
            row + 1
        );
    }
    Ok(())
}

/// 按编辑器的制表位和 Unicode 宽度规则计算字符显示列数。
fn editor_display_width(chars: impl Iterator<Item = char>) -> usize {
    chars.fold(0, |width, ch| {
        width.saturating_add(if ch == '\t' {
            let tab_length = usize::from(EDITOR_TAB_LENGTH);
            tab_length - width % tab_length
        } else {
            ch.width().unwrap_or(0)
        })
    })
}

/// 保存结果；远端快照冲突时不触碰写回动作。
enum SaveResult {
    /// 已成功写回或本地内容本来就与快照一致。
    Saved,
    /// 远端内容与打开时快照不同，需要用户明确确认覆盖。
    RemoteChanged,
}

/// 重新读取远端并按快照检测冲突，确认后再覆盖写回。
fn save_editor(app: &App, editor: &mut RemoteEditor, overwrite: bool) -> Result<SaveResult> {
    let remote = adb::read_file(app.serial.as_deref(), &editor.path, MAX_FILE_BYTES)?;
    if !overwrite && remote != editor.original_bytes {
        return Ok(SaveResult::RemoteChanged);
    }
    if !editor.dirty && !overwrite {
        return Ok(SaveResult::Saved);
    }

    let bytes = editor.bytes();
    if bytes.len() > MAX_FILE_BYTES {
        anyhow::bail!(
            "编辑内容超过 {} MiB 写入上限",
            MAX_FILE_BYTES / (1024 * 1024)
        );
    }
    if bytes == editor.original_bytes && !overwrite {
        editor.dirty = false;
        return Ok(SaveResult::Saved);
    }

    adb::write_file(app.serial.as_deref(), &editor.path, &bytes)?;
    editor.original_bytes = bytes;
    editor.dirty = false;
    Ok(SaveResult::Saved)
}

/// 执行保存并把冲突/错误转成编辑器内可见提示，不关闭编辑器。
fn save_editor_with_prompt(app: &mut App, editor: &mut RemoteEditor, overwrite: bool) {
    match save_editor(app, editor, overwrite) {
        Ok(SaveResult::Saved) => {
            editor.prompt = None;
            app.push_status(StatusLine::Ok(format!("已保存 {}", editor.path)));
        }
        Ok(SaveResult::RemoteChanged) => {
            editor.prompt = Some(EditorPrompt::RemoteChanged);
            app.push_status(StatusLine::Warn(format!(
                "远端文件 {} 已被修改：按 o 覆盖写回，Esc 取消",
                editor.path
            )));
        }
        Err(err) => app.push_status(StatusLine::Err(format!("保存 {} 失败：{err}", editor.path))),
    }
}

/// 按当前选中条目加载、更新或关闭右侧预览面板。
///
/// 目录和设备特殊文件不读取；普通文件与符号链接通过 `adb exec-out` 读取，
/// 成功后按图片格式和纯文本规则选择对应的面板内容。读取失败只进入状态区，
/// 不会退出浏览器。
fn toggle_preview(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    preview: &mut Option<Preview>,
) -> Result<()> {
    let Some(index) = app.selected_entry_index() else {
        *preview = None;
        app.push_status(StatusLine::Err("没有选中的条目，无法预览".to_owned()));
        return Ok(());
    };
    let Some(entry) = app.entries.get(index) else {
        return Ok(());
    };
    if !matches!(entry.kind, EntryKind::File | EntryKind::Symlink) {
        *preview = None;
        app.push_status(StatusLine::Err(
            "预览只支持普通文件和文件符号链接".to_owned(),
        ));
        return Ok(());
    }
    let name = entry.name.clone();
    let path = join_path(&app.cwd, &name);
    if preview
        .as_ref()
        .is_some_and(|current| current.path() == path)
    {
        *preview = None;
        return Ok(());
    }

    *preview = Some(Preview::Loading {
        name: name.clone(),
        path: path.clone(),
    });
    terminal.draw(|frame| {
        ui(frame, app, preview.as_ref(), None);
    })?;

    let loaded = adb::read_file(app.serial.as_deref(), &path, MAX_FILE_BYTES)
        .and_then(|bytes| build_preview(name, path.clone(), bytes));
    match loaded {
        Ok(value) => *preview = Some(value),
        Err(err) => {
            *preview = None;
            app.push_status(StatusLine::Err(format!("预览 {path} 失败：{err}")));
        }
    }
    Ok(())
}

/// 将设备端文件字节转换为文本或图片预览数据。
fn build_preview(name: String, path: String, bytes: Vec<u8>) -> Result<Preview> {
    if bytes.len() > MAX_FILE_BYTES {
        anyhow::bail!("文件超过 {} MiB 预览上限", MAX_FILE_BYTES / (1024 * 1024));
    }
    if let Some((image, width, height)) = decode_image(&bytes)? {
        return Ok(Preview::Image(ImagePreview {
            name,
            path,
            width,
            height,
            // 消费 DynamicImage；RGB 输入可直接复用像素缓冲，避免 to_rgb8 的额外复制。
            image: image.into_rgb8(),
        }));
    }
    if let Some(content) = decode_plain_text(&bytes) {
        return Ok(Preview::Text(TextPreview {
            name,
            path,
            content,
            scroll: 0,
        }));
    }
    anyhow::bail!("只支持纯文本和 PNG/JPEG/GIF/BMP/WebP 图片")
}

/// 按格式识别并受限解码图片；无法识别为支持格式时返回 None，交给文本规则继续判断。
///
/// 先读取图片头部尺寸并检查宽高、像素数，再创建带分配上限的解码器。这样压缩数据很小
/// 但声明为超大画布的图片不会先分配完整像素缓冲。
fn decode_image(bytes: &[u8]) -> Result<Option<(image::DynamicImage, u32, u32)>> {
    let Ok(format) = image::guess_format(bytes) else {
        return Ok(None);
    };
    if !matches!(
        format,
        ImageFormat::Bmp
            | ImageFormat::Gif
            | ImageFormat::Jpeg
            | ImageFormat::Png
            | ImageFormat::WebP
    ) {
        return Ok(None);
    }

    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format).into_dimensions()?;
    validate_image_dimensions(width, height)?;

    let mut limits = image::io::Limits::default();
    limits.max_image_width = Some(MAX_PREVIEW_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_PREVIEW_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_PREVIEW_IMAGE_ALLOC);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    Ok(Some((reader.decode()?, width, height)))
}

/// 校验图片声明尺寸，确保后续解码和 RGB 转换都落在预览资源预算内。
fn validate_image_dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 {
        anyhow::bail!("图片尺寸无效");
    }
    if width > MAX_PREVIEW_IMAGE_DIMENSION || height > MAX_PREVIEW_IMAGE_DIMENSION {
        anyhow::bail!(
            "图片尺寸超过 {}×{} 预览上限",
            MAX_PREVIEW_IMAGE_DIMENSION,
            MAX_PREVIEW_IMAGE_DIMENSION
        );
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PREVIEW_IMAGE_PIXELS {
        anyhow::bail!("图片像素数超过 {} 像素预览上限", MAX_PREVIEW_IMAGE_PIXELS);
    }
    Ok(())
}

/// 解码没有终端控制字符的 UTF-8 文本，避免原样渲染二进制或 ANSI 转义序列。
fn decode_plain_text(bytes: &[u8]) -> Option<String> {
    let content = std::str::from_utf8(bytes).ok()?;
    if content
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return None;
    }
    Some(content.to_owned())
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
fn pull_marked(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    out_dir: &Path,
    preview: &Option<Preview>,
) -> Result<()> {
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
    terminal.draw(|frame| {
        ui(frame, app, preview.as_ref(), None);
    })?;
    let totals = adb::du_totals(app.serial.as_deref(), &targets);
    let mut cancelled = false;
    for (i, remote) in targets.iter().enumerate() {
        let name = base_name(remote);
        app.pulling = Some(format!("({i}/{total}) {name}"));
        terminal.draw(|frame| {
            ui(frame, app, preview.as_ref(), None);
        })?;
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
                let _ = terminal.draw(|frame| {
                    ui(frame, app, preview.as_ref(), None);
                });
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

/// 渲染一帧：编辑模式独占上方区域；否则无预览时列表占满上方区域，
/// 有预览时左右各半。返回区域供鼠标事件使用。
fn ui(
    frame: &mut Frame,
    app: &mut App,
    preview: Option<&Preview>,
    editor: Option<&RemoteEditor>,
) -> UiLayout {
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

    let layout = if let Some(editor) = editor {
        render_editor(frame, editor, chunks[0]);
        UiLayout {
            editor: Some(chunks[0]),
            ..UiLayout::default()
        }
    } else if let Some(preview) = preview {
        let panes = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[0]);
        render_list(frame, app, panes[0]);
        render_preview(frame, preview, panes[1]);
        UiLayout {
            list: Some(panes[0]),
            ..UiLayout::default()
        }
    } else {
        render_list(frame, app, chunks[0]);
        UiLayout {
            list: Some(chunks[0]),
            ..UiLayout::default()
        }
    };
    if !status.is_empty() {
        render_status(frame, status, chunks[1], app.error.is_some());
    }
    let help = editor.map_or_else(
        || help_text(app.creating, app.searching).to_owned(),
        editor_help_text,
    );
    render_help(frame, chunks[2], &help);
    layout
}

/// 渲染成熟 textarea 控件，并把其计算出的真实光标位置交给 ratatui。
fn render_editor(frame: &mut Frame, editor: &RemoteEditor, area: Rect) {
    frame.render_widget(&editor.textarea, area);
    if let Some(position) = editor.textarea.rendered_cursor_position() {
        frame.set_cursor_position(position);
    }
}

/// 渲染右侧预览面板：文本使用 Paragraph 滚动，图片使用每个终端单元格上下
/// 两种颜色的半块字符，加载中显示状态文字。
fn render_preview(frame: &mut Frame, preview: &Preview, area: Rect) {
    match preview {
        Preview::Loading { name, path } => {
            frame.render_widget(
                Paragraph::new(format!("读取 {path}…"))
                    .style(Style::new().fg(Color::DarkGray))
                    .block(Block::bordered().title(format!(" 预览 · {name} "))),
                area,
            );
        }
        Preview::Text(text) => {
            let title = format!(" 预览 · 文本 · {} ", text.name);
            frame.render_widget(
                Paragraph::new(text.content.as_str())
                    .scroll((text.scroll, 0))
                    .block(Block::bordered().title(title)),
                area,
            );
        }
        Preview::Image(image) => render_image_preview(frame, image, area),
    }
}

/// 在指定区域内按终端字符比例缩放图片并绘制。
fn render_image_preview(frame: &mut Frame, preview: &ImagePreview, area: Rect) {
    let block = Block::bordered().title(format!(
        " 预览 · 图片 · {} ({}×{}) ",
        preview.name, preview.width, preview.height
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // 一个终端单元格用上半块字符承载上下两个像素，补偿字符通常高于宽的比例。
    let target_width = u32::from(inner.width);
    let target_height = u32::from(inner.height).saturating_mul(2);
    let thumbnail = imageops::thumbnail(&preview.image, target_width, target_height);
    let image_width = thumbnail.width() as u16;
    let image_height = thumbnail.height().div_ceil(2) as u16;
    let start_x = inner.x + inner.width.saturating_sub(image_width) / 2;
    let start_y = inner.y + inner.height.saturating_sub(image_height) / 2;
    let buffer = frame.buffer_mut();

    for y in 0..image_height {
        for x in 0..image_width {
            let top = thumbnail.get_pixel(u32::from(x), u32::from(y) * 2);
            let top_color = Color::Rgb(top[0], top[1], top[2]);
            let cell = &mut buffer[(start_x + x, start_y + y)];
            cell.set_symbol("▀").set_fg(top_color);
            if u32::from(y) * 2 + 1 < thumbnail.height() {
                let bottom = thumbnail.get_pixel(u32::from(x), u32::from(y) * 2 + 1);
                cell.set_bg(Color::Rgb(bottom[0], bottom[1], bottom[2]));
            } else {
                cell.set_bg(Color::Reset);
            }
        }
    }
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
        "↑↓ 移动 · 鼠标单击/滚轮 · → 进入 · ←/⌫ 返回 · v 预览/关闭 · e 编辑 · PageUp/Down 滚动 · / 筛选 · S/s 排序 · O/o 方向 · M/m 新建 · Space 标记 · Enter 拉取 · q/Esc 退出（拉取中=取消）"
    }
}

/// 编辑器底部帮助栏文案，区分普通编辑和两个确认提示。
fn editor_help_text(editor: &RemoteEditor) -> String {
    match editor.prompt {
        Some(EditorPrompt::Unsaved) => {
            "存在未保存修改：s 保存 · d 丢弃并退出 · Esc 继续编辑".to_owned()
        }
        Some(EditorPrompt::RemoteChanged) => {
            "远端文件已修改：o 覆盖写回 · Esc 取消保存并继续编辑".to_owned()
        }
        None if editor.dirty => {
            "Ctrl-S 保存 · Esc 退出（有修改） · 鼠标单击定位 · 滚轮滚动 · ↑↓←→ 移动".to_owned()
        }
        None => "Ctrl-S 保存 · Esc 退出 · 鼠标单击定位 · 滚轮滚动 · ↑↓←→ 移动".to_owned(),
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
    use ratatui::{Terminal, backend::TestBackend};

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
        assert!(browse.contains("v 预览/关闭"));
        assert!(browse.contains("e 编辑"));
        assert!(browse.contains("鼠标单击/滚轮"));
        assert!(browse.contains("PageUp/Down 滚动"));
        // 筛选版
        assert!(help_text(false, true).contains("筛选词"));
        // 新建版优先于筛选（事件循环保证两者互斥，此处为防御性排序）
        assert!(help_text(true, false).contains("新文件夹名"));
        assert!(help_text(true, true).contains("新文件夹名"));
    }

    #[test]
    fn text_lines_preserve_trailing_newline_and_normalize_crlf() {
        assert_eq!(text_lines("第一行\r\n第二行\r\n"), ["第一行", "第二行", ""]);
        assert_eq!(text_lines("第一行\n第二行"), ["第一行", "第二行"]);
        assert_eq!(text_lines(""), [""]);
    }

    #[test]
    fn editor_serializes_original_line_ending() {
        let content = "第一行\r\n第二行\r\n";
        let mut textarea = TextArea::new(text_lines(content));
        textarea.set_block(Block::bordered().title(" 编辑 "));
        let editor = RemoteEditor {
            path: "/sdcard/notes.txt".into(),
            textarea,
            original_bytes: content.as_bytes().to_vec(),
            line_ending: LineEnding::CrLf,
            dirty: false,
            prompt: None,
        };
        assert_eq!(editor.bytes(), content.as_bytes());
    }

    #[test]
    fn editor_rejects_reported_70000_character_line() {
        let lines = text_lines(&"a".repeat(70_000));
        assert!(validate_editor_lines(&lines).is_err());
    }

    #[test]
    fn editor_rejects_column_without_room_for_caret_boundary() {
        assert!(validate_editor_lines(&["a".repeat(65_535)]).is_err());
    }

    #[test]
    fn editor_rejects_character_after_column_capacity() {
        let mut textarea = TextArea::new(vec!["a".repeat(MAX_EDITOR_COLUMNS)]);
        textarea.move_cursor(CursorMove::End);
        let mut editor = RemoteEditor {
            path: "/sdcard/notes.txt".into(),
            textarea,
            original_bytes: Vec::new(),
            line_ending: LineEnding::Lf,
            dirty: false,
            prompt: None,
        };
        let mut app = app_with_entries(vec![]);
        handle_editor_key(
            &mut app,
            &mut editor,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(
            editor.textarea.lines()[0].chars().count(),
            MAX_EDITOR_COLUMNS
        );
    }

    #[test]
    fn editor_rejects_tab_character_after_display_capacity() {
        let mut textarea = TextArea::new(vec!["a".repeat(MAX_EDITOR_COLUMNS - 1)]);
        textarea.move_cursor(CursorMove::End);
        let mut editor = RemoteEditor {
            path: "/sdcard/notes.txt".into(),
            textarea,
            original_bytes: Vec::new(),
            line_ending: LineEnding::Lf,
            dirty: false,
            prompt: None,
        };
        let mut app = app_with_entries(vec![]);
        handle_editor_key(
            &mut app,
            &mut editor,
            KeyEvent::new(KeyCode::Char('\t'), KeyModifiers::NONE),
        );
        assert_eq!(
            editor.textarea.lines()[0].chars().count(),
            MAX_EDITOR_COLUMNS - 1
        );
    }

    #[test]
    fn editor_rejects_newline_after_line_capacity() {
        let mut textarea = TextArea::new(vec![String::new(); MAX_EDITOR_LINES]);
        textarea.move_cursor(CursorMove::Bottom);
        let mut editor = RemoteEditor {
            path: "/sdcard/notes.txt".into(),
            textarea,
            original_bytes: Vec::new(),
            line_ending: LineEnding::Lf,
            dirty: false,
            prompt: None,
        };
        let mut app = app_with_entries(vec![]);
        handle_editor_key(
            &mut app,
            &mut editor,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert_eq!(editor.textarea.lines().len(), MAX_EDITOR_LINES);
    }

    #[test]
    fn editor_rejects_deletion_that_would_join_oversized_lines() {
        let first = "a".repeat(MAX_EDITOR_COLUMNS / 2 + 1);
        let second = "b".repeat(MAX_EDITOR_COLUMNS / 2);
        let mut textarea = TextArea::new(vec![first.clone(), second.clone()]);
        textarea.move_cursor(CursorMove::Bottom);
        textarea.move_cursor(CursorMove::Head);
        let mut editor = RemoteEditor {
            path: "/sdcard/notes.txt".into(),
            textarea,
            original_bytes: Vec::new(),
            line_ending: LineEnding::Lf,
            dirty: false,
            prompt: None,
        };
        let mut app = app_with_entries(vec![]);
        handle_editor_key(
            &mut app,
            &mut editor,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(editor.textarea.lines(), [first, second]);
    }

    #[test]
    fn editor_checks_wide_characters_by_display_columns() {
        assert!(validate_editor_lines(&["中".repeat(32_768)]).is_err());
    }

    #[test]
    fn editor_checks_tabs_by_display_columns() {
        assert!(validate_editor_lines(&["\t".repeat(16_384)]).is_err());
    }

    #[test]
    fn editor_checks_zero_width_characters_by_character_count() {
        assert!(validate_editor_lines(&["\u{0301}".repeat(65_535)]).is_err());
    }

    #[test]
    fn editor_counts_trailing_empty_line_towards_capacity() {
        assert!(validate_editor_lines(&text_lines(&"\n".repeat(65_535))).is_err());
    }

    #[test]
    fn editor_accepts_crlf_and_tab_at_column_boundary() {
        let content = format!("{}a\t中\r\n", "a".repeat(65_528));
        assert!(validate_editor_lines(&text_lines(&content)).is_ok());
    }

    #[test]
    fn editor_capacity_boundaries_support_rendered_mouse_position() {
        // 分别覆盖最大行数和最大列宽，避免构造二者乘积大小的文档。
        for lines in [vec![String::new(); 65_535], vec!["a".repeat(65_534)]] {
            validate_editor_lines(&lines).unwrap();
            let mut textarea = TextArea::new(lines);
            textarea.move_cursor(CursorMove::Bottom);
            textarea.move_cursor(CursorMove::End);
            let expected = textarea.cursor();
            let mut terminal = Terminal::new(TestBackend::new(30, 4)).unwrap();
            terminal
                .draw(|frame| frame.render_widget(&textarea, frame.area()))
                .unwrap();
            let position = textarea.rendered_cursor_position().unwrap();
            assert_eq!(textarea.cursor_at_position(position), Some(expected));
        }
    }

    #[test]
    fn list_mouse_click_selects_visible_row() {
        let mut app = app_with_entries(vec![
            mk("a.txt", EntryKind::File, 1, 0),
            mk("b.txt", EntryKind::File, 2, 0),
            mk("c.txt", EntryKind::File, 3, 0),
        ]);
        app.state.select(Some(0));
        let area = Rect::new(0, 0, 30, 5);
        handle_browse_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 4,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
            &mut app,
            UiLayout {
                list: Some(area),
                ..UiLayout::default()
            },
        );
        assert_eq!(app.state.selected(), Some(1));
    }

    #[test]
    fn editor_mouse_click_uses_unicode_aware_hit_testing() {
        let content = "你好 world";
        let mut textarea = TextArea::new(text_lines(content));
        textarea.set_block(Block::bordered().title(" 编辑 "));
        let mut editor = RemoteEditor {
            path: "/sdcard/notes.txt".into(),
            textarea,
            original_bytes: content.as_bytes().to_vec(),
            line_ending: LineEnding::Lf,
            dirty: false,
            prompt: None,
        };
        let area = Rect::new(0, 0, 30, 4);
        let mut terminal = Terminal::new(TestBackend::new(30, 4)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(&editor.textarea, area);
            })
            .unwrap();

        // 边框内 x=3 位于两个宽字符之后；库返回按字符计数的第 1 列，而非字节偏移。
        handle_editor_mouse(
            &mut editor,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row: 1,
                modifiers: KeyModifiers::NONE,
            },
            UiLayout {
                editor: Some(area),
                ..UiLayout::default()
            },
        );
        assert_eq!(editor.textarea.cursor(), (0, 1));
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
    fn decodes_plain_text_preview() {
        let preview = build_preview(
            "notes.txt".to_owned(),
            "/sdcard/notes.txt".to_owned(),
            "第一行\n第二行".as_bytes().to_vec(),
        )
        .unwrap();
        let Preview::Text(text) = preview else {
            panic!("UTF-8 text should create a text preview");
        };
        assert_eq!(text.name, "notes.txt");
        assert_eq!(text.content, "第一行\n第二行");
        assert_eq!(text.scroll, 0);
    }

    #[test]
    fn decodes_png_preview() {
        // 1×1 PNG，验证图片按内容识别而不是依赖文件名后缀。
        const ONE_PIXEL_PNG: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 4, 0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15,
            0, 1, 5, 1, 1, 39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
        ];
        let preview = build_preview(
            "photo.bin".to_owned(),
            "/sdcard/photo.bin".to_owned(),
            ONE_PIXEL_PNG.to_vec(),
        )
        .unwrap();
        let Preview::Image(image) = preview else {
            panic!("PNG bytes should create an image preview");
        };
        assert_eq!((image.width, image.height), (1, 1));
    }

    #[test]
    fn rejects_image_with_too_many_pixels_before_decoding() {
        // 黑色 PNG 具有很高压缩率，模拟“压缩数据很小但解码画布很大”的输入。
        let source = RgbImage::from_pixel(2048, 2049, image::Rgb([0, 0, 0]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(source)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageOutputFormat::Png)
            .unwrap();
        let error = match build_preview(
            "large.png".to_owned(),
            "/sdcard/large.png".to_owned(),
            bytes,
        ) {
            Ok(_) => panic!("images over the pixel limit should be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("像素数"));
    }

    #[test]
    fn renders_image_inside_right_preview_pane() {
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app_with_entries(vec![mk("photo.png", EntryKind::File, 4, 0)]);
        let preview = Preview::Image(ImagePreview {
            name: "photo.png".to_owned(),
            path: "/sdcard/photo.png".to_owned(),
            image: RgbImage::from_pixel(2, 2, image::Rgb([255, 0, 0])),
            width: 2,
            height: 2,
        });

        terminal
            .draw(|frame| {
                ui(frame, &mut app, Some(&preview), None);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().any(|cell| cell.symbol() == "▀"));
        assert!(buffer.content().iter().any(|cell| cell.symbol() == "预"));
    }

    #[test]
    fn rejects_binary_preview() {
        let error = match build_preview(
            "data.bin".to_owned(),
            "/sdcard/data.bin".to_owned(),
            vec![0, 159, 146, 150],
        ) {
            Ok(_) => panic!("binary bytes should not create a preview"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("只支持纯文本"));
    }

    #[test]
    fn rejects_preview_over_size_limit() {
        let error = match build_preview(
            "large.txt".to_owned(),
            "/sdcard/large.txt".to_owned(),
            vec![b'x'; MAX_FILE_BYTES + 1],
        ) {
            Ok(_) => panic!("oversized bytes should not create a preview"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("10 MiB"));
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
