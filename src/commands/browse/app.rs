//! App 状态机与目录加载。
//!
//! `load_dir` 是本命令唯一的 `adb ls` 出口；除 `App::new` 与
//! `App::switch_dir`/`App::reload` 会发起 adb 调用外，其余方法都是纯状态变更。

use std::collections::HashSet;

use anyhow::{Result, anyhow};
use ratatui::widgets::ListState;

use super::model::{Entry, EntryKind, SortKey, parse_ls, sort_entries, visible_indices};
use super::paths::{join_path, parent_path};
use crate::adb;

/// 状态区一行的语义，决定渲染前缀与颜色（显式取代 bool + "⚠" 前缀字符串的隐式约定）。
pub(super) enum StatusLine {
    /// 拉取成功等：✓ 前缀，绿色
    Ok(String),
    /// 拉取失败/提示：✗ 前缀，红色
    Err(String),
    /// 落点冲突等警告：整行黄色
    Warn(String),
}

/// TUI 会话状态。除 `switch_dir`/`reload`/`new` 会发起 adb 调用外，其余方法都是纯状态变更。
pub(super) struct App {
    /// 目标设备 serial，透传给 adb
    pub(super) serial: Option<String>,
    /// 当前目录（设备端绝对路径，已 normalize，无尾 `/`，根为 `/`）
    pub(super) cwd: String,
    /// 当前目录条目（目录在前）
    pub(super) entries: Vec<Entry>,
    /// 列表选中状态：光标位置与滚动偏移由 ListState 托管，跨帧持久持有
    pub(super) state: ListState,
    /// 已标记的设备端绝对路径，跨目录保留，Enter 拉取后清空
    pub(super) marked: HashSet<String>,
    /// 进入子目录前的 (cwd, 光标)，返回上级时恢复
    pub(super) history: Vec<(String, usize)>,
    /// 最近一次目录加载失败信息，展示在状态区；下次成功加载即清除
    pub(super) error: Option<String>,
    /// 状态区行（结果/警告），经 [`App::push_status`] 保持最多 8 条
    pub(super) status: Vec<StatusLine>,
    /// 正在拉取的条目（含 (i/N) 进度与名字），渲染"拉取中"提示
    pub(super) pulling: Option<String>,
    /// 整个会话累计拉取成功 / 失败的条数，退出时打印摘要用
    pub(super) pulled_total: usize,
    pub(super) failed_total: usize,
    /// 排序键与方向：`S` 循环切键（重置为该键自然方向）、`O` 翻转方向；跨目录保留
    pub(super) sort: SortKey,
    pub(super) desc: bool,
    /// 筛选词：`/` 进入筛选模式后边输边滤；切换目录自动清除
    pub(super) filter: Option<String>,
    /// 是否处于筛选输入模式（Enter 保留筛选退出，Esc 清除筛选退出）
    pub(super) searching: bool,
    /// 是否处于新建文件夹输入模式（M 进入；Enter 确认创建在 tui 层，Esc 取消）
    pub(super) creating: bool,
    /// 新建文件夹的待输入名字（进入输入模式时清空）
    pub(super) create_name: String,
}

impl App {
    /// 创建 App 并加载起始目录；失败直接 Err（调用方在进 TUI 前退出）。
    pub(super) fn new(serial: Option<&str>, cwd: &str) -> Result<Self> {
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
            creating: false,
            create_name: String::new(),
        };
        // 首次加载复用 switch_dir（加载/排序/光标初始化只此一份），失败转 Err
        if !app.switch_dir(cwd, 0) {
            let detail = app.error.take().unwrap_or_default();
            return Err(anyhow!("加载起始目录 {cwd} 失败：{detail}"));
        }
        Ok(app)
    }

    /// 追加一条状态行并保持总量不超过 8 条（丢最旧），防止状态区挤占列表区。
    pub(super) fn push_status(&mut self, line: StatusLine) {
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
                self.state
                    .select((!self.entries.is_empty()).then(|| cursor.min(self.entries.len() - 1)));
                true
            }
            Err(err) => {
                self.error = Some(err);
                false
            }
        }
    }

    /// 重新加载当前目录并把光标定位到名为 `select_name` 的条目（新建文件夹
    /// 成功后刷新并定位用）。转发 [`Self::switch_dir`]，因此同样会清除筛选、
    /// 不动 history；加载失败停留原地并写 error，返回 false。
    pub(super) fn reload(&mut self, select_name: &str) -> bool {
        let cwd = self.cwd.clone();
        if !self.switch_dir(&cwd, 0) {
            return false;
        }
        self.select_by_name(select_name);
        true
    }

    /// →：进入光标所在目录或目录型符号链接（Android 顶层 `/sdcard`、`/etc`
    /// 等多为 symlink，不放行则从根目录无处可去）。目录型 symlink 靠
    /// shell_list_dir 的尾部 `/` 正确列出内容；指向文件的 symlink 进入时
    /// ls 自然报错、经 switch_dir 失败路径停留原地。成功才记录历史。
    pub(super) fn enter_selected(&mut self) {
        let Some(i) = self.selected_entry_index() else {
            return;
        };
        let Some(entry) = self.entries.get(i) else {
            return;
        };
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
    pub(super) fn go_parent(&mut self) {
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
    pub(super) fn move_cursor(&mut self, delta: i32) {
        let len = self.visible().len();
        if len == 0 {
            return;
        }
        let current = self.state.selected().unwrap_or(0) as i32;
        self.state
            .select(Some((current + delta).clamp(0, len as i32 - 1) as usize));
    }

    /// Space：标记/取消标记当前条目（设备端绝对路径进出 marked 集合）。
    pub(super) fn toggle_mark(&mut self) {
        let Some(i) = self.selected_entry_index() else {
            return;
        };
        let Some(entry) = self.entries.get(i) else {
            return;
        };
        let path = join_path(&self.cwd, &entry.name);
        if !self.marked.remove(&path) {
            self.marked.insert(path);
        }
    }

    /// 当前筛选视图：可见条目的 entries 下标（保序）。
    pub(super) fn visible(&self) -> Vec<usize> {
        visible_indices(&self.entries, self.filter.as_deref())
    }

    /// 光标所指可见位对应的 entries 真实下标（筛选/排序都会改变可见位，
    /// 标记与进入目录必须映射回真实条目）。
    pub(super) fn selected_entry_index(&self) -> Option<usize> {
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

    /// 把光标定位到可见视图中名为 name 的条目（刷新后定位新建文件夹用；
    /// 与 resort 同理以名字定位、落在可见位坐标）；找不到则光标不动。
    fn select_by_name(&mut self, name: &str) {
        if let Some(pos) = self
            .visible()
            .iter()
            .position(|&i| self.entries[i].name == name)
        {
            self.state.select(Some(pos));
        }
    }

    /// `/`：进入筛选输入模式（已有筛选词则继续编辑）。
    pub(super) fn search_begin(&mut self) {
        self.searching = true;
    }

    /// 筛选模式输入一个字符：追加筛选词并即时过滤。
    pub(super) fn search_input(&mut self, c: char) {
        self.filter.get_or_insert_with(String::new).push(c);
        self.sync_cursor();
    }

    /// 筛选模式退格：删除筛选词末字符（删空 = 显示全部，仍留在筛选模式）。
    pub(super) fn search_del_char(&mut self) {
        if let Some(word) = self.filter.as_mut() {
            word.pop();
            self.sync_cursor();
        }
    }

    /// Enter：退出筛选模式；空筛选词视同未筛选（清除）。
    pub(super) fn search_commit(&mut self) {
        self.searching = false;
        if self.filter.as_deref().is_some_and(|w| w.trim().is_empty()) {
            self.filter = None;
        }
    }

    /// Esc：退出筛选模式并清除筛选（恢复完整列表）。
    pub(super) fn search_cancel(&mut self) {
        self.searching = false;
        if self.filter.take().is_some() {
            self.sync_cursor();
        }
    }

    /// `M`：进入新建文件夹输入模式（清空上次未完成的名字）。
    pub(super) fn create_begin(&mut self) {
        self.creating = true;
        self.create_name.clear();
    }

    /// 新建模式输入一个字符（空格是合法的文件夹名字符）。
    pub(super) fn create_input(&mut self, c: char) {
        self.create_name.push(c);
    }

    /// 新建模式退格。
    pub(super) fn create_del_char(&mut self) {
        self.create_name.pop();
    }

    /// Esc：退出新建模式并丢弃未完成的名字。
    pub(super) fn create_cancel(&mut self) {
        self.creating = false;
        self.create_name.clear();
    }

    /// `S`：切换排序键并重置为该键自然方向，重排当前列表。
    pub(super) fn cycle_sort(&mut self) {
        self.sort = self.sort.cycle();
        self.desc = self.sort.default_desc();
        self.resort();
    }

    /// `O`：在当前排序键上翻转方向（升 ↔ 降），重排当前列表。
    pub(super) fn toggle_desc(&mut self) {
        self.desc = !self.desc;
        self.resort();
    }

    /// 按当前键与方向重排，并让光标跟随原条目到新位置（排序是原地换位，
    /// 下标不跨排序存活，故以目录内唯一的文件名定位原条目）。
    fn resort(&mut self) {
        let current = self
            .selected_entry_index()
            .map(|i| self.entries[i].name.clone());
        sort_entries(&mut self.entries, self.sort, self.desc);
        let vis = self.visible();
        self.state.select(match current {
            Some(name) => vis
                .iter()
                .position(|&i| self.entries[i].name == name)
                .or(Some(0)),
            None => (!vis.is_empty()).then_some(0),
        });
    }
}

/// 列出并解析设备端目录（adb I/O），错误折叠为可直接展示的字符串。
fn load_dir(serial: Option<&str>, path: &str) -> std::result::Result<Vec<Entry>, String> {
    let out = adb::shell_list_dir(serial, path).map_err(|e| e.to_string())?;
    parse_ls(&out).map_err(|e| format!("无法解析 {path} 的 ls 输出：{e}"))
}

/// 直接构造 App（同模块可见私有字段），不经 adb 加载目录，纯状态可测。
#[cfg(test)]
pub(super) fn app_with_entries(entries: Vec<Entry>) -> App {
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
        creating: false,
        create_name: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::mk;
    use super::*;

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
    fn create_input_builds_name_and_cancel_resets() {
        let mut app = app_with_entries(vec![]);
        app.create_begin();
        assert!(app.creating);
        // 空格是合法名字符，逐字输入"新建 目录"
        for c in "新建 目录".chars() {
            app.create_input(c);
        }
        assert_eq!(app.create_name, "新建 目录");
        // 退格删掉末字，再 Esc 取消丢弃全部
        app.create_del_char();
        assert_eq!(app.create_name, "新建 目");
        app.create_cancel();
        assert!(!app.creating);
        assert_eq!(app.create_name, "");
        // 再次进入时残留不带回输入模式
        app.create_begin();
        assert_eq!(app.create_name, "");
    }

    #[test]
    fn select_by_name_targets_visible_position() {
        let mut app = app_with_entries(vec![
            mk("DCIM", EntryKind::Dir, 0, 0),
            mk("Music", EntryKind::Dir, 0, 0),
            mk("notes.txt", EntryKind::File, 0, 0),
        ]);
        app.select_by_name("notes.txt");
        assert_eq!(app.state.selected(), Some(2));
        // 筛选视图下定位落在可见位坐标而非真实下标
        app.filter = Some("txt".to_owned());
        app.select_by_name("notes.txt");
        assert_eq!(app.state.selected(), Some(0));
        assert_eq!(app.selected_entry_index(), Some(2));
        // 名字不存在时光标不动
        app.select_by_name("zzz");
        assert_eq!(app.state.selected(), Some(0));
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
}
