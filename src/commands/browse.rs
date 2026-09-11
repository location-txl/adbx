//! `adbx browse` 子命令：TUI 浏览设备文件系统，Space 标记、Enter 批量拉取到本地，
//! `/` 现场筛选、`S`/`O`（大小写均可）切换排序键与方向。
//!
//! 文件内分两层：与设备无关的解析/路径/格式化纯函数在前（可单测），
//! App 状态机、渲染与事件循环在后；adb 调用统一经 [`crate::adb`]，
//! 本模块不直接 spawn 进程。

use std::cmp::Ordering;
use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::adb;

// ---------- 纯函数层：解析、路径、格式化 ----------

/// 目录条目类型，由 `ls -l` 输出的 perms 首字符判断。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum EntryKind {
    Dir,
    File,
    Symlink,
    Other,
}

/// 目录中的一个条目，[`parse_ls`] 的产物。
#[derive(Debug, PartialEq)]
struct Entry {
    /// 文件名，可含空格（从原行第 8 字段起截取）
    name: String,
    kind: EntryKind,
    /// 字节数；目录为其自身 inode 大小（通常 4096），仅作展示
    size: u64,
    /// 修改时间的原始展示串（常规 `2024-05-01 12:34`；旧格式变体含年份）
    date: String,
    /// 修改时间数值键（[`parse_mtime_key`]），日期排序用；解析不了为 0
    mtime: u64,
}

impl Entry {
    fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
}

/// 排序键：`S` 循环切换，`O` 在当前键上翻转方向。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum SortKey {
    Name,
    Size,
    Date,
}

impl SortKey {
    /// 名称 → 大小 → 日期 → 名称。
    fn cycle(self) -> Self {
        match self {
            SortKey::Name => SortKey::Size,
            SortKey::Size => SortKey::Date,
            SortKey::Date => SortKey::Name,
        }
    }

    /// 标题栏中文标签。
    fn label(self) -> &'static str {
        match self {
            SortKey::Name => "名称",
            SortKey::Size => "大小",
            SortKey::Date => "日期",
        }
    }

    /// 自然方向：名称升序，大小/日期降序（`S` 切键时重置到该方向）。
    fn default_desc(self) -> bool {
        !matches!(self, SortKey::Name)
    }
}

/// 解析 `ls -l`（toybox，Android 6+）输出为条目列表（纯函数，可单测）。
///
/// * `output` - `adb shell ls -l <path>` 的 stdout 原文（`\r\n` 换行，`lines()` 天然兼容）
///
/// 逐行调用 [`parse_ls_line`]，天然跳过 `total N` 头行和混在输出里的
/// `ls: ...` 报错行。返回 Err 的唯一情况：输出有内容但一行都没解析成功，
/// 用于防止格式整体不匹配被误显示成空目录。
fn parse_ls(output: &str) -> std::result::Result<Vec<Entry>, String> {
    let entries: Vec<Entry> = output.lines().filter_map(parse_ls_line).collect();
    if entries.is_empty()
        && output.lines().any(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("total ")
        })
    {
        return Err(output.trim().to_owned());
    }
    Ok(entries)
}

/// 解析单行 `ls -l` 输出（纯函数，可单测）。
///
/// toybox 行格式：`perms nlink owner group size date time name...`，
/// 7 个字段之后的内容全部属于文件名（可含空格）。perms 首字符判类型，
/// size 取第 5 字段（字符/块设备此处是 `major, minor`，占两列，
/// 名字后移到第 9 字段，size 记 0），
/// 文件名从原行第 8 字段的字节偏移处截取以保留内部连续空格；
/// 符号链接去掉 ` -> target` 尾部（取最后一个分隔符，链接名本身含
/// " -> " 时不会被截断）。不满足格式（total 头行、`ls:` 报错行、字段数
/// 不足等）返回 None。
fn parse_ls_line(line: &str) -> Option<Entry> {
    let kind = match line.as_bytes().first()? {
        b'd' => EntryKind::Dir,
        b'l' => EntryKind::Symlink,
        b'-' => EntryKind::File,
        b'c' | b'b' | b'p' | b's' => EntryKind::Other,
        _ => return None,
    };
    // size 解析失败即滤掉非目录行（total、ls: 报错行等）。
    // 例外：字符/块设备的 size 位置是 "major, minor"（如 /dev/null 的 5, 1），
    // 含空格占两列，名字相应后移到第 9 字段（0 起 8），size 按 0 计，
    // date/time 也相应后移一字段（常规行在第 6/7 字段，0 起 5/6）
    let size_field = line.split_whitespace().nth(4)?;
    let (size, date_field, name_field) = if kind == EntryKind::Other && size_field.contains(',') {
        (0, 6, 8)
    } else {
        match size_field.parse() {
            Ok(n) => (n, 5, 7),
            Err(_) => return None,
        }
    };
    let mut date_time = line.split_whitespace();
    let date = date_time.nth(date_field)?;
    let time = date_time.next()?;
    let mtime = parse_mtime_key(date, time);
    let date = format!("{date} {time}");
    let mut name = &line[field_offset(line, name_field)?..];
    if kind == EntryKind::Symlink
        && let Some(pos) = name.rfind(" -> ")
    {
        // 取最后一个分隔符：链接名本身含 " -> "（如 'a -> b'）时不被截断
        name = &name[..pos];
    }
    if name.is_empty() {
        return None;
    }
    // 拒绝 `.`/`..` 与控制字符。真实 ls 恒不输出前两者，唯一来源是文件名内嵌
    // 换行把一行拆成两行后伪造出的幻影条目（`..` 会让 adb pull 逃出输出目录）；
    // 控制字符（如 ESC 转义序列）则可注入 TUI 显示层。
    if matches!(name, "." | "..") || name.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    Some(Entry { name: name.to_owned(), kind, size, date, mtime })
}

/// 把 `ls -l` 的日期/时间两字段折算为可比较的数值键（纯函数，可单测）。
///
/// 常规 toybox 格式 `2024-05-01` + `12:34` → `202405011234`：各段固定乘法间隔
/// 落位、不补零也不串位，数值序即时间序；旧文件变体 time 位是 4 位年份
/// （`2023`）按该年 1 月 1 日近似；带秒的 `12:34:56` 取前两段；任一段解析
/// 不了返回 0，日期排序中视为最旧（同值再退名称序）。
fn parse_mtime_key(date: &str, time: &str) -> u64 {
    // "2024-05-01" → [2024, 5, 1]；任一段非数字返回 None
    fn numeric_parts(s: &str, sep: char) -> Option<Vec<u64>> {
        s.split(sep).map(|p| p.parse().ok()).collect()
    }
    let key = |y: u64, mo: u64, d: u64, h: u64, mi: u64| {
        y * 100_000_000 + mo * 1_000_000 + d * 10_000 + h * 100 + mi
    };
    let Some(ymd) = numeric_parts(date, '-') else { return 0 };
    let [y, mo, d] = ymd[..] else { return 0 };
    // time 位是纯数字：只可能是 4 位年份的旧格式变体，按该年年初近似
    if let Ok(year) = time.parse::<u64>() {
        return if (1000..10000).contains(&year) { key(year, 1, 1, 0, 0) } else { 0 };
    }
    let Some(hms) = numeric_parts(time, ':') else { return 0 };
    if hms.len() < 2 {
        return 0;
    }
    key(y, mo, d, hms[0], hms[1])
}

/// 返回第 `n` 个（0 起）空白分隔字段的起始字节偏移（纯函数）。
/// 字段不足 n+1 个时返回 None。
fn field_offset(line: &str, n: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut field = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // bytes[i] 是一个字段的起始（上一字节是空白或行首）
        if field == n {
            return Some(i);
        }
        field += 1;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
    }
    None
}

/// 拼接设备端绝对路径：`join_path("/sdcard", "DCIM")` → `/sdcard/DCIM`。
fn join_path(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// 父目录路径：`/` 无父级返回 None；`/sdcard` → `/`；`/a/b` → `/a`。
fn parent_path(path: &str) -> Option<String> {
    if path == "/" {
        return None;
    }
    let trimmed = path.strip_suffix('/').unwrap_or(path);
    Some(match trimmed.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(i) => trimmed[..i].to_owned(),
    })
}

/// 路径最后一段名字：`/sdcard/DCIM` → `DCIM`；`/` → `/`。
fn base_name(path: &str) -> &str {
    if path == "/" {
        "/"
    } else {
        path.rsplit('/').next().unwrap_or(path)
    }
}

/// 去掉尾部 `/`：`/sdcard/` → `/sdcard`；全斜杠或空串归一为 `/`，
/// 保证结果恒非空、以 `/` 开头（App.cwd 的不变量）。
fn normalize_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() { "/".to_owned() } else { trimmed.to_owned() }
}

/// 排序（原地）：目录始终置顶（不受方向影响），键内按 `key` 升序、`desc` 时
/// 反转为降序；同值退回名称序，保证顺序确定。
fn sort_entries(entries: &mut [Entry], key: SortKey, desc: bool) {
    entries.sort_unstable_by(|a, b| match (a.is_dir(), b.is_dir()) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => {
            let by_name = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
            let ord = match key {
                SortKey::Name => by_name(),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Date => a.mtime.cmp(&b.mtime),
            }
            .then_with(by_name);
            if desc { ord.reverse() } else { ord }
        }
    });
}

/// 筛选视图（纯函数，可单测）：名称大小写不敏感包含过滤词的条目下标（保序）；
/// 过滤词为 None 或去空白后为空 = 全部可见。
fn visible_indices(entries: &[Entry], filter: Option<&str>) -> Vec<usize> {
    let Some(word) = filter.map(str::trim).filter(|w| !w.is_empty()) else {
        return (0..entries.len()).collect();
    };
    let word = word.to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.name.to_lowercase().contains(&word))
        .map(|(i, _)| i)
        .collect()
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

/// 剔除已被其他标记路径覆盖的子路径（如已标记 `/a` 就不再单独拉 `/a/b.txt`），
/// 返回按字典序排序的 Vec，保证拉取顺序确定。
fn filter_covered(marked: &HashSet<String>) -> Vec<String> {
    let mut paths: Vec<String> = marked.iter().cloned().collect();
    paths.sort();
    paths.retain(|p| {
        // 逐级上溯父目录，任一级被标记即本路径已被覆盖
        let mut ancestor = p.as_str();
        while let Some(idx) = ancestor.rfind('/') {
            ancestor = &ancestor[..idx];
            if marked.contains(ancestor) {
                return false;
            }
        }
        true
    });
    paths
}

/// 同名冲突比较用的名字折叠：macOS/Windows 本地文件系统默认大小写不敏感
/// （APFS/NTFS 默认形态），`DCIM` 与 `dcim` 会落到同一本地目录，须折叠后
/// 比较；Linux 文件系统大小写敏感，保持精确比较。
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn fold_name(name: &str) -> String {
    name.to_lowercase()
}

/// Linux 上直接返回原名，避免无谓的分配。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn fold_name(name: &str) -> String {
    name.to_owned()
}

/// 找出同批拉取清单中 base_name 重复的名字（纯函数，可单测）。
///
/// 所有条目统一拉到 `out_dir/<base_name>`，不同父目录下的同名条目
/// （如 `/a/DCIM` 与 `/b/DCIM`，或大小写不敏感文件系统上的 `/b/dcim`）
/// 会互相覆盖/合并，调用方据此在拉取前提示。
/// 返回按字典序排序的去重名字列表；无冲突返回空。
fn dup_base_names(targets: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut dups: HashSet<String> = HashSet::new();
    for p in targets {
        let name = base_name(p);
        if !seen.insert(fold_name(name)) {
            dups.insert(name.to_owned());
        }
    }
    let mut dups: Vec<String> = dups.into_iter().collect();
    dups.sort();
    dups
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

// ---------- App 状态机 ----------

/// 状态区一行的语义，决定渲染前缀与颜色（显式取代 bool + "⚠" 前缀字符串的隐式约定）。
enum StatusLine {
    /// 拉取成功等：✓ 前缀，绿色
    Ok(String),
    /// 拉取失败/提示：✗ 前缀，红色
    Err(String),
    /// 落点冲突等警告：整行黄色
    Warn(String),
}

/// TUI 会话状态。除 `switch_dir`/`new` 会发起 adb 调用外，其余方法都是纯状态变更。
struct App {
    /// 目标设备 serial，透传给 adb
    serial: Option<String>,
    /// 当前目录（设备端绝对路径，已 normalize，无尾 `/`，根为 `/`）
    cwd: String,
    /// 当前目录条目（目录在前）
    entries: Vec<Entry>,
    /// 列表选中状态：光标位置与滚动偏移由 ListState 托管，跨帧持久持有
    state: ListState,
    /// 已标记的设备端绝对路径，跨目录保留，Enter 拉取后清空
    marked: HashSet<String>,
    /// 进入子目录前的 (cwd, 光标)，返回上级时恢复
    history: Vec<(String, usize)>,
    /// 最近一次目录加载失败信息，展示在状态区；下次成功加载即清除
    error: Option<String>,
    /// 状态区行（结果/警告），经 [`App::push_status`] 保持最多 8 条
    status: Vec<StatusLine>,
    /// 正在拉取的条目（含 (i/N) 进度与名字），渲染"拉取中"提示
    pulling: Option<String>,
    /// 整个会话累计拉取成功 / 失败的条数，退出时打印摘要用
    pulled_total: usize,
    failed_total: usize,
    /// 排序键与方向：`S` 循环切键（重置为该键自然方向）、`O` 翻转方向；跨目录保留
    sort: SortKey,
    desc: bool,
    /// 筛选词：`/` 进入筛选模式后边输边滤；切换目录自动清除
    filter: Option<String>,
    /// 是否处于筛选输入模式（Enter 保留筛选退出，Esc 清除筛选退出）
    searching: bool,
}

impl App {
    /// 创建 App 并加载起始目录；失败直接 Err（调用方在进 TUI 前退出）。
    fn new(serial: Option<&str>, cwd: &str) -> Result<Self> {
        let mut app = Self {
            serial: serial.map(str::to_owned),
            cwd: cwd.to_owned(),
            entries: Vec::new(),
            state: ListState::default(),
            marked: HashSet::new(),
            history: Vec::new(),
            error: None,
            status: Vec::new(),
            pulling: None,
            pulled_total: 0,
            failed_total: 0,
            sort: SortKey::Name,
            desc: false,
            filter: None,
            searching: false,
        };
        // 首次加载复用 switch_dir（加载/排序/光标初始化只此一份），失败转 Err
        if !app.switch_dir(cwd, 0) {
            let detail = app.error.take().unwrap_or_default();
            return Err(anyhow!("加载起始目录 {cwd} 失败：{detail}"));
        }
        Ok(app)
    }

    /// 追加一条状态行并保持总量不超过 8 条（丢最旧），防止状态区挤占列表区。
    fn push_status(&mut self, line: StatusLine) {
        self.status.push(line);
        while self.status.len() > 8 {
            self.status.remove(0);
        }
    }

    /// 切换到目标目录：先加载，成功才提交状态（cwd/列表/光标），返回 true；
    /// 失败只写 error、停留原地，返回 false。
    fn switch_dir(&mut self, target: &str, cursor: usize) -> bool {
        match load_dir(self.serial.as_deref(), target) {
            Ok(mut entries) => {
                sort_entries(&mut entries, self.sort, self.desc);
                self.cwd = target.to_owned();
                self.entries = entries;
                self.error = None;
                // 筛选是"对当前目录列表"的语义，换目录即失效
                self.filter = None;
                self.searching = false;
                // 光标尽量落在期望行；目录变空则取消选中
                self.state.select((!self.entries.is_empty()).then(|| cursor.min(self.entries.len() - 1)));
                true
            }
            Err(err) => {
                self.error = Some(err);
                false
            }
        }
    }

    /// →：进入光标所在目录或目录型符号链接（Android 顶层 `/sdcard`、`/etc`
    /// 等多为 symlink，不放行则从根目录无处可去）。目录型 symlink 靠
    /// shell_list_dir 的尾部 `/` 正确列出内容；指向文件的 symlink 进入时
    /// ls 自然报错、经 switch_dir 失败路径停留原地。成功才记录历史。
    fn enter_selected(&mut self) {
        let Some(i) = self.selected_entry_index() else { return };
        let Some(entry) = self.entries.get(i) else { return };
        if !matches!(entry.kind, EntryKind::Dir | EntryKind::Symlink) {
            self.error = Some(format!("「{}」不是目录，→ 仅用于进入文件夹", entry.name));
            return;
        }
        let next = join_path(&self.cwd, &entry.name);
        let prev = (self.cwd.clone(), i);
        if self.switch_dir(&next, 0) {
            self.history.push(prev);
        }
    }

    /// ← / Backspace：返回上一级。有历史则恢复进入前的光标位置；
    /// 无历史（如启动时直接给深层路径）退到父目录置顶；已在根目录则不动作。
    fn go_parent(&mut self) {
        let (target, cursor) = match self.history.last() {
            Some((cwd, cursor)) => (cwd.clone(), *cursor),
            None => match parent_path(&self.cwd) {
                Some(parent) => (parent, 0),
                None => return,
            },
        };
        if self.switch_dir(&target, cursor) {
            self.history.pop();
        }
    }

    /// 光标上下移动 delta 行，clamp 在 [0, 可见条数-1]（筛选视图内）；空列表不动作。
    fn move_cursor(&mut self, delta: i32) {
        let len = self.visible().len();
        if len == 0 {
            return;
        }
        let current = self.state.selected().unwrap_or(0) as i32;
        self.state.select(Some((current + delta).clamp(0, len as i32 - 1) as usize));
    }

    /// Space：标记/取消标记当前条目（设备端绝对路径进出 marked 集合）。
    fn toggle_mark(&mut self) {
        let Some(i) = self.selected_entry_index() else { return };
        let Some(entry) = self.entries.get(i) else { return };
        let path = join_path(&self.cwd, &entry.name);
        if !self.marked.remove(&path) {
            self.marked.insert(path);
        }
    }

    /// 当前筛选视图：可见条目的 entries 下标（保序）。
    fn visible(&self) -> Vec<usize> {
        visible_indices(&self.entries, self.filter.as_deref())
    }

    /// 光标所指可见位对应的 entries 真实下标（筛选/排序都会改变可见位，
    /// 标记与进入目录必须映射回真实条目）。
    fn selected_entry_index(&self) -> Option<usize> {
        let pos = self.state.selected()?;
        self.visible().get(pos).copied()
    }

    /// 可见集变化后同步光标：有可见项则 clamp（原先未选中则选中首个），否则取消选中。
    fn sync_cursor(&mut self) {
        let len = self.visible().len();
        self.state.select(match len {
            0 => None,
            _ => Some(self.state.selected().map_or(0, |s| s.min(len - 1))),
        });
    }

    /// `/`：进入筛选输入模式（已有筛选词则继续编辑）。
    fn search_begin(&mut self) {
        self.searching = true;
    }

    /// 筛选模式输入一个字符：追加筛选词并即时过滤。
    fn search_input(&mut self, c: char) {
        self.filter.get_or_insert_with(String::new).push(c);
        self.sync_cursor();
    }

    /// 筛选模式退格：删除筛选词末字符（删空 = 显示全部，仍留在筛选模式）。
    fn search_del_char(&mut self) {
        if let Some(word) = self.filter.as_mut() {
            word.pop();
            self.sync_cursor();
        }
    }

    /// Enter：退出筛选模式；空筛选词视同未筛选（清除）。
    fn search_commit(&mut self) {
        self.searching = false;
        if self.filter.as_deref().is_some_and(|w| w.trim().is_empty()) {
            self.filter = None;
        }
    }

    /// Esc：退出筛选模式并清除筛选（恢复完整列表）。
    fn search_cancel(&mut self) {
        self.searching = false;
        if self.filter.take().is_some() {
            self.sync_cursor();
        }
    }

    /// `S`：切换排序键并重置为该键自然方向，重排当前列表。
    fn cycle_sort(&mut self) {
        self.sort = self.sort.cycle();
        self.desc = self.sort.default_desc();
        self.resort();
    }

    /// `O`：在当前排序键上翻转方向（升 ↔ 降），重排当前列表。
    fn toggle_desc(&mut self) {
        self.desc = !self.desc;
        self.resort();
    }

    /// 按当前键与方向重排，并让光标跟随原条目到新位置（排序是原地换位，
    /// 下标不跨排序存活，故以目录内唯一的文件名定位原条目）。
    fn resort(&mut self) {
        let current = self.selected_entry_index().map(|i| self.entries[i].name.clone());
        sort_entries(&mut self.entries, self.sort, self.desc);
        let vis = self.visible();
        self.state.select(match current {
            Some(name) => {
                vis.iter().position(|&i| self.entries[i].name == name).or(Some(0))
            }
            None => (!vis.is_empty()).then_some(0),
        });
    }
}

/// 列出并解析设备端目录（adb I/O），错误折叠为可直接展示的字符串。
fn load_dir(serial: Option<&str>, path: &str) -> std::result::Result<Vec<Entry>, String> {
    let out = adb::shell_list_dir(serial, path).map_err(|e| e.to_string())?;
    parse_ls(&out).map_err(|e| format!("无法解析 {path} 的 ls 输出：{e}"))
}

// ---------- TUI 层：渲染与事件循环 ----------

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
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press && is_quit_key(k.code, k.modifiers) => {
                return true;
            }
            Ok(_) => {}
            // 事件源损坏时继续轮询只会空转，直接视为未取消
            Err(_) => return false,
        }
    }
    false
}

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

    let mut app = App::new(serial, &normalize_path(path))?;
    // ratatui::run 自动完成 raw mode、备用屏、退出恢复与 panic hook
    ratatui::run(|terminal| event_loop(terminal, &mut app, &out_dir))?;

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

/// TUI 主循环：绘制一帧 → 阻塞等按键 → 分发。
/// 只有终端 I/O 本身失败（终端不可用）才向上返回 Err。
fn event_loop(terminal: &mut DefaultTerminal, app: &mut App, out_dir: &Path) -> Result<()> {
    loop {
        terminal.draw(|frame| ui(frame, app))?;
        let Event::Key(key) = event::read()? else { continue };
        // 只响应按下事件：kitty 键盘协议下 Release/Repeat 也会上报
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // 筛选输入模式：可打印字符进筛选词（q 不再退出），Space 仍标记光标条目，
        // Esc 清除筛选返回，仅 Ctrl-C 整体退出；→/← 不响应（先 Enter/Esc 回浏览模式）
        if app.searching {
            match key.code {
                KeyCode::Esc => app.search_cancel(),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
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
        match key.code {
            KeyCode::Up => app.move_cursor(-1),
            KeyCode::Down => app.move_cursor(1),
            KeyCode::Right => app.enter_selected(),
            KeyCode::Left | KeyCode::Backspace => app.go_parent(),
            KeyCode::Char(' ') => app.toggle_mark(),
            KeyCode::Char('/') => app.search_begin(),
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
        app.status = vec![StatusLine::Err("未标记任何条目，先按 Space 标记".to_owned())];
        return Ok(());
    }
    let targets = filter_covered(&app.marked);
    app.status.clear();
    app.error = None;
    // 落点冲突提示（只提示不阻断），同样计入 8 条上限，防止挤占列表区
    for name in dup_base_names(&targets) {
        app.push_status(StatusLine::Warn(format!("⚠ 多个同名条目 {name}，将合并/覆盖到同一路径")));
    }
    let mut warned_local = HashSet::new();
    for name in targets.iter().map(|p| base_name(p)) {
        if out_dir.join(name).exists() && warned_local.insert(name.to_owned()) {
            app.push_status(StatusLine::Warn(format!("⚠ 本地已存在 {name}，将被覆盖/合并")));
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
                app.push_status(StatusLine::Ok(format!("{remote} → {}", local_path.display())));
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

/// 渲染一帧：列表区（自适应）/ 状态区（仅有内容时出现）/ 帮助栏（固定 1 行）。
fn ui(frame: &mut Frame, app: &mut App) {
    let status = status_content(app);
    let status_h: u16 = if status.is_empty() { 0 } else { (status.len() + 2) as u16 };
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
    render_help(frame, chunks[2], app.searching);
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
            StatusLine::Warn(msg) => {
                lines.push(Line::from(Span::styled(msg.clone(), Style::new().fg(Color::Yellow))))
            }
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
    if app.searching || app.filter.is_some() {
        let caret = if app.searching { "▏" } else { "" };
        let word = app.filter.as_deref().unwrap_or("");
        title.push_str(&format!(" · 筛选:{word}{caret} ({}/{})", vis.len(), app.entries.len()));
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
            Paragraph::new(hint).style(Style::new().fg(Color::DarkGray)).block(block),
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
    let title = if is_error { " 错误 " } else { " 拉取结果 " };
    frame.render_widget(Paragraph::new(lines).block(Block::bordered().title(title)), area);
}

/// 渲染底部帮助栏：浏览模式与筛选输入模式各一版。
fn render_help(frame: &mut Frame, area: Rect, searching: bool) {
    let text = if searching {
        "输入筛选词（大小写不敏感） · ↑↓ 移动 · Enter 保留筛选 · Esc 清除 · Ctrl-C 退出"
    } else {
        "↑↓ 移动 · → 进入 · ←/⌫ 返回 · / 筛选 · S/s 排序 · O/o 方向 · Space 标记 · Enter 拉取 · q/Esc 退出（拉取中=取消）"
    };
    frame.render_widget(Paragraph::new(text).style(Style::new().fg(Color::DarkGray)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造测试条目：date 置空串（不参与断言），mtime 由调用方给。
    fn mk(name: &str, kind: EntryKind, size: u64, mtime: u64) -> Entry {
        Entry { name: name.to_owned(), kind, size, date: String::new(), mtime }
    }

    /// 直接构造 App（同模块可见私有字段），不经 adb 加载目录，纯状态可测。
    fn app_with_entries(entries: Vec<Entry>) -> App {
        App {
            serial: None,
            cwd: "/sdcard".into(),
            entries,
            state: ListState::default(),
            marked: HashSet::new(),
            history: Vec::new(),
            error: None,
            status: Vec::new(),
            pulling: None,
            pulled_total: 0,
            failed_total: 0,
            sort: SortKey::Name,
            desc: false,
            filter: None,
            searching: false,
        }
    }

    /// Android 真机 /sdcard 的典型 toybox ls -l 输出（\r\n 换行，含中文、空格、符号链接）。
    const TYPICAL_LS: &str = "\
total 104\r
drwxrwx--x 4 root sdcard_rw 4096 2024-05-01 12:34 Alarms\r
drwxrwx--x 2 root sdcard_rw 4096 2024-05-01 12:34 Android SDK Docs\r
lrwxrwxrwx 1 root root 21 2024-05-01 12:40 loc_kernel -> /sdcard/DCIM\r
-rw-rw---- 1 root sdcard_rw 1234567 2024-06-01 08:15 我的 备份.txt\r
";

    #[test]
    fn parses_typical_ls_output() {
        let entries = parse_ls(TYPICAL_LS).unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].name, "Alarms");
        assert_eq!(entries[0].kind, EntryKind::Dir);
        // 名字含空格：第 8 字段后的所有内容都属于文件名
        assert_eq!(entries[1].name, "Android SDK Docs");
        // 符号链接：去掉 " -> 目标" 尾部
        assert_eq!(entries[2].name, "loc_kernel");
        assert_eq!(entries[2].kind, EntryKind::Symlink);
        // size 为第 5 字段；中文与空格混合的文件名照常解析
        assert_eq!(entries[3].name, "我的 备份.txt");
        assert_eq!(entries[3].kind, EntryKind::File);
        assert_eq!(entries[3].size, 1234567);
        // 日期字段：原始串保留供展示，数值键供日期排序
        assert_eq!(entries[0].date, "2024-05-01 12:34");
        assert_eq!(entries[0].mtime, 2024_0501_1234);
    }

    #[test]
    fn preserves_consecutive_spaces_in_name() {
        let line = "-rw-rw---- 1 root root 8 2024-06-01 08:15 a  b.txt";
        assert_eq!(parse_ls_line(line).unwrap().name, "a  b.txt");
    }

    #[test]
    fn symlink_name_containing_arrow_not_truncated() {
        // 链接名本身含 " -> "：须取最后一个分隔符，不能把名字截断成 "a"
        let line = "lrwxrwxrwx 1 root root 4 2024-05-01 12:40 a -> b -> /sdcard/x";
        assert_eq!(parse_ls_line(line).unwrap().name, "a -> b");
    }

    #[test]
    fn rejects_dot_entries_and_control_chars_in_name() {
        // 文件名内嵌换行会把一行拆成两行，第二段若形如合法 ls 行会伪造出条目；
        // 名为 ".." 的幻影条目被标记拉取时会让 adb pull 逃出输出目录，必须拒绝
        assert!(parse_ls_line("drwxrwx--x 2 root root 4096 2024-01-01 00:00 ..").is_none());
        assert!(parse_ls_line("-rw-rw---- 1 root root 3 2024-01-01 00:00 .").is_none());
        // 含 ESC 等控制字符的名字（终端转义序列注入载体）同样拒绝
        assert!(parse_ls_line("-rw-rw---- 1 root root 3 2024-01-01 00:00 a\x1b[2Jb").is_none());
        // 普通名字（含空格、中文）不受影响
        assert!(parse_ls_line("-rw-rw---- 1 root root 3 2024-01-01 00:00 我的 备份.txt").is_some());
    }

    #[test]
    fn empty_or_total_only_output_is_ok_empty() {
        assert!(parse_ls("").unwrap().is_empty());
        assert!(parse_ls("total 0\r\n").unwrap().is_empty());
    }

    #[test]
    fn skips_error_lines_mixed_in_output() {
        // 部分设备 adb shell 不回传退出码，ls 报错行会混在 stdout 里
        let out = "ls: /data/xxx: Permission denied\r\n-rw-rw---- 1 root root 3 2024-06-01 08:15 ok.txt\r\n";
        let entries = parse_ls(out).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "ok.txt");
    }

    #[test]
    fn fully_unparseable_output_is_error() {
        // 输出有内容但一行都没解析成功：报错而非误报空目录
        assert!(parse_ls("some\nunknown\nformat\n").is_err());
    }

    #[test]
    fn parses_entry_with_year_instead_of_time() {
        // 个别 ls 变体对旧文件用年份顶替 time 字段，字段数不变，应照常解析
        let line = "drwxrwx--x 2 root sdcard_rw 4096 2024-05-01 2023 old_dir";
        assert_eq!(parse_ls_line(line).unwrap().name, "old_dir");
    }

    #[test]
    fn parses_device_file_with_major_minor() {
        // 字符/块设备在 size 字段输出 "major, minor"（如 /dev/null 的 5, 1），按 0 计
        let line = "crw-rw-rw- 1 root root 5, 1 1970-01-01 08:00 null";
        let entry = parse_ls_line(line).unwrap();
        assert_eq!(entry.name, "null");
        assert_eq!(entry.kind, EntryKind::Other);
        assert_eq!(entry.size, 0);
        // 普通文件行出现同样字段仍被滤掉（size 必须是纯数字）
        assert!(parse_ls_line("-rw-rw---- 1 root root 5, 1 1970-01-01 08:00 x").is_none());
    }

    #[test]
    fn field_offset_counts_from_line_start() {
        let line = "a  bb ccc";
        assert_eq!(field_offset(line, 0), Some(0));
        assert_eq!(field_offset(line, 1), Some(3));
        assert_eq!(field_offset(line, 2), Some(6));
        assert_eq!(field_offset(line, 3), None);
    }

    #[test]
    fn parses_date_and_mtime_from_ls_lines() {
        let e = parse_ls_line("-rw-rw---- 1 root root 3 2024-06-01 08:15 a.txt").unwrap();
        assert_eq!(e.date, "2024-06-01 08:15");
        assert_eq!(e.mtime, 2024_0601_0815);
        // 年份变体：time 位是 4 位年份，按该年 1 月 1 日近似
        let e = parse_ls_line("drwxrwx--x 2 root sdcard_rw 4096 2024-05-01 2023 old_dir").unwrap();
        assert_eq!(e.mtime, 2023_0101_0000);
        // 带秒变体取前两段
        let e = parse_ls_line("-rw-rw---- 1 root root 3 2024-06-01 08:15:30 a.txt").unwrap();
        assert_eq!(e.mtime, 2024_0601_0815);
        // 设备行（size 为 "major, minor"）日期/时间后移一字段
        let e = parse_ls_line("crw-rw-rw- 1 root root 5, 1 1970-01-01 08:00 null").unwrap();
        assert_eq!(e.mtime, 1970_0101_0800);
        // 非常规日期格式 → 0（日期排序中视为最旧）
        let e = parse_ls_line("-rw-rw---- 1 root root 3 unknown-date 12:00 weird.txt").unwrap();
        assert_eq!(e.mtime, 0);
    }

    #[test]
    fn parse_mtime_key_handles_variants() {
        assert_eq!(parse_mtime_key("2024-06-01", "08:15"), 2024_0601_0815);
        // 段不补零也正确落位（乘法间隔保证不串位）
        assert_eq!(parse_mtime_key("2024-6-1", "8:05"), 2024_0601_0805);
        assert_eq!(parse_mtime_key("2024-05-01", "2023"), 2023_0101_0000);
        assert_eq!(parse_mtime_key("2024-06-01", "08:15:30"), 2024_0601_0815);
        assert_eq!(parse_mtime_key("bad", "08:15"), 0);
        assert_eq!(parse_mtime_key("2024-06-01", "nope"), 0);
        // 纯数字但不是 4 位年份的 time 位无法解释 → 0
        assert_eq!(parse_mtime_key("2024-06-01", "12"), 0);
    }

    #[test]
    fn sort_key_cycles_labels_and_default_direction() {
        assert_eq!(SortKey::Name.cycle(), SortKey::Size);
        assert_eq!(SortKey::Size.cycle(), SortKey::Date);
        assert_eq!(SortKey::Date.cycle(), SortKey::Name);
        assert!(!SortKey::Name.default_desc());
        assert!(SortKey::Size.default_desc());
        assert!(SortKey::Date.default_desc());
        assert_eq!(SortKey::Size.label(), "大小");
        assert_eq!(SortKey::Date.label(), "日期");
    }

    #[test]
    fn sorts_dirs_first_case_insensitive() {
        let mut entries = vec![
            mk("b.txt", EntryKind::File, 0, 0),
            mk("DCIM", EntryKind::Dir, 0, 0),
            mk("abc", EntryKind::Dir, 0, 0),
            mk("A.txt", EntryKind::File, 0, 0),
        ];
        sort_entries(&mut entries, SortKey::Name, false);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["abc", "DCIM", "A.txt", "b.txt"]);
        // 方向反转：名称降序，目录组/文件组各自内部反序，目录置顶不变
        sort_entries(&mut entries, SortKey::Name, true);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["DCIM", "abc", "b.txt", "A.txt"]);
    }

    #[test]
    fn sorts_by_size_with_direction_dirs_first() {
        let mut entries = vec![
            mk("big", EntryKind::File, 300, 0),
            mk("small", EntryKind::File, 1, 0),
            mk("zdir", EntryKind::Dir, 99, 0),
        ];
        fn names(es: &[Entry]) -> Vec<&str> { es.iter().map(|e| e.name.as_str()).collect() }
        sort_entries(&mut entries, SortKey::Size, false);
        assert_eq!(names(&entries), ["zdir", "small", "big"]);
        sort_entries(&mut entries, SortKey::Size, true);
        assert_eq!(names(&entries), ["zdir", "big", "small"]);
    }

    #[test]
    fn sorts_by_date_newest_first_unknown_oldest() {
        let mut entries = vec![
            mk("old.txt", EntryKind::File, 1, 2023_0101_0000),
            mk("new.txt", EntryKind::File, 1, 2024_0101_0000),
            mk("unknown.txt", EntryKind::File, 1, 0),
            mk("mid.txt", EntryKind::File, 1, 2023_0601_0000),
        ];
        fn names(es: &[Entry]) -> Vec<&str> { es.iter().map(|e| e.name.as_str()).collect() }
        // 降序（自然方向）：新在前；mtime 0 视为最旧落末尾
        sort_entries(&mut entries, SortKey::Date, true);
        assert_eq!(names(&entries), ["new.txt", "mid.txt", "old.txt", "unknown.txt"]);
        // 升序：旧在前
        sort_entries(&mut entries, SortKey::Date, false);
        assert_eq!(names(&entries), ["unknown.txt", "old.txt", "mid.txt", "new.txt"]);
    }

    #[test]
    fn visible_indices_filters_case_insensitively() {
        let entries = vec![
            mk("DCIM", EntryKind::Dir, 0, 0),
            mk("Music", EntryKind::Dir, 0, 0),
            mk("notes.txt", EntryKind::File, 0, 0),
        ];
        // None/空白 = 不过滤
        assert_eq!(visible_indices(&entries, None), vec![0, 1, 2]);
        assert_eq!(visible_indices(&entries, Some("")), vec![0, 1, 2]);
        assert_eq!(visible_indices(&entries, Some("  ")), vec![0, 1, 2]);
        // 大小写不敏感子串
        assert_eq!(visible_indices(&entries, Some("dci")), vec![0]);
        assert_eq!(visible_indices(&entries, Some("TXT")), vec![2]);
        assert!(visible_indices(&entries, Some("没有")).is_empty());
    }

    #[test]
    fn search_flow_filters_maps_and_cancels() {
        let mut app = app_with_entries(vec![
            mk("DCIM", EntryKind::Dir, 0, 0),
            mk("Music", EntryKind::Dir, 0, 0),
            mk("notes.txt", EntryKind::File, 0, 0),
        ]);
        app.search_begin();
        assert!(app.searching);
        app.search_input('t');
        app.search_input('x');
        // 只剩 notes.txt：可见位 0 映射回真实下标 2
        assert_eq!(app.visible(), vec![2]);
        app.state.select(Some(0));
        assert_eq!(app.selected_entry_index(), Some(2));
        // 筛选态下标记（事件循环把 Space 路由到 toggle_mark）作用于真实条目路径
        app.toggle_mark();
        assert!(app.marked.contains("/sdcard/notes.txt"));
        app.toggle_mark();
        assert!(app.marked.is_empty());
        // 无匹配 → 取消选中；回退一个字符恢复匹配后光标回落首行
        app.search_input('q');
        assert!(app.visible().is_empty());
        assert_eq!(app.state.selected(), None);
        app.search_del_char();
        assert_eq!(app.state.selected(), Some(0));
        // 退格到空筛选词 = 显示全部，仍处于筛选模式
        app.search_del_char();
        app.search_del_char();
        assert_eq!(app.filter.as_deref(), Some(""));
        assert_eq!(app.visible().len(), 3);
        assert!(app.searching);
        // Enter：保留筛选退出；空词视同未筛选被清除
        app.search_commit();
        assert!(!app.searching);
        assert_eq!(app.filter, None);
        app.search_begin();
        app.search_commit();
        assert_eq!(app.filter, None);
        // Esc：清除筛选退出筛选模式
        app.search_begin();
        app.search_input('m');
        app.search_cancel();
        assert!(!app.searching);
        assert_eq!(app.filter, None);
        assert_eq!(app.visible().len(), 3);
    }

    #[test]
    fn sort_cycles_resort_and_cursor_follows() {
        let mut app = app_with_entries(vec![
            mk("b", EntryKind::File, 300, 5),
            mk("a", EntryKind::File, 1, 9),
            mk("Z", EntryKind::Dir, 99, 1),
        ]);
        // 名称升序的目录置顶基线（与 switch_dir 加载后的顺序一致）
        sort_entries(&mut app.entries, SortKey::Name, false);
        app.state.select(Some(1)); // 光标在 "a"
        assert_eq!(app.entries[1].name, "a");
        // S：切到大小并重置为自然方向（降序），目录仍置顶；光标跟随 "a" 到新位置
        app.cycle_sort();
        assert_eq!(app.sort, SortKey::Size);
        assert!(app.desc);
        let names: Vec<&str> = app.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Z", "b", "a"]);
        assert_eq!(app.state.selected(), Some(2));
        // O：翻转为升序，光标仍跟随 "a"
        app.toggle_desc();
        let names: Vec<&str> = app.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Z", "a", "b"]);
        assert_eq!(app.state.selected(), Some(1));
        // 循环回名称键时方向重置为升序
        app.cycle_sort(); // Date（降序）
        app.cycle_sort(); // Name（升序）
        assert_eq!(app.sort, SortKey::Name);
        assert!(!app.desc);
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
    fn joins_and_traverses_paths() {
        assert_eq!(join_path("/", "sdcard"), "/sdcard");
        assert_eq!(join_path("/sdcard", "DCIM"), "/sdcard/DCIM");
        assert_eq!(parent_path("/"), None);
        assert_eq!(parent_path("/sdcard").as_deref(), Some("/"));
        assert_eq!(parent_path("/a/b").as_deref(), Some("/a"));
        assert_eq!(base_name("/sdcard/DCIM"), "DCIM");
        assert_eq!(base_name("/"), "/");
    }

    #[test]
    fn normalize_strips_trailing_slash() {
        assert_eq!(normalize_path("/sdcard/"), "/sdcard");
        assert_eq!(normalize_path("/"), "/");
        // 全斜杠/空串归一为根，保证结果恒非空且以 / 开头
        assert_eq!(normalize_path("//"), "/");
        assert_eq!(normalize_path(""), "/");
    }

    #[test]
    fn filter_covered_drops_children_of_marked_parents() {
        // 已标记 /a 时，其子路径 /a/b.txt 不再单独拉取
        let marked: HashSet<String> = ["/a", "/a/b.txt", "/c"].map(String::from).into_iter().collect();
        assert_eq!(filter_covered(&marked), vec!["/a".to_owned(), "/c".to_owned()]);
        // 无嵌套关系时全部保留，且顺序确定
        let flat: HashSet<String> = ["/y", "/x"].map(String::from).into_iter().collect();
        assert_eq!(filter_covered(&flat), vec!["/x".to_owned(), "/y".to_owned()]);
    }

    #[test]
    fn dup_base_names_finds_cross_dir_collisions() {
        // 不同父目录下的同名条目会落到同一本地路径 out/<名字>
        let targets: Vec<String> =
            ["/a/DCIM", "/b/DCIM", "/b/DCIM2", "/c/Music"].map(String::from).into_iter().collect();
        assert_eq!(dup_base_names(&targets), vec!["DCIM".to_owned()]);
        // 无重复时为空
        let flat: Vec<String> = ["/x/a", "/y/b"].map(String::from).into_iter().collect();
        assert!(dup_base_names(&flat).is_empty());
    }

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
        let file = Entry { date: "2024-06-01 08:15".into(), ..file };
        assert!(!text(&format_row(&file, false, false, false)).contains("2024-"));
        assert!(text(&format_row(&file, false, false, true)).contains(" · 2024-06-01 08:15"));
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
