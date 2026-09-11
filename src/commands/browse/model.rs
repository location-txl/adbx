//! 目录条目数据模型与 `ls -l` 输出解析、排序、筛选视图（纯函数，可单测）。
//!
//! 与设备无关：只吃 `adb shell ls` 的 stdout 字符串，adb I/O 在
//! [`super::app`] 的 `load_dir` 中完成。

use std::cmp::Ordering;

/// 目录条目类型，由 `ls -l` 输出的 perms 首字符判断。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum EntryKind {
    Dir,
    File,
    Symlink,
    Other,
}

/// 目录中的一个条目，[`parse_ls`] 的产物。
#[derive(Debug, PartialEq)]
pub(super) struct Entry {
    /// 文件名，可含空格（从原行第 8 字段起截取）
    pub(super) name: String,
    pub(super) kind: EntryKind,
    /// 字节数；目录为其自身 inode 大小（通常 4096），仅作展示
    pub(super) size: u64,
    /// 修改时间的原始展示串（常规 `2024-05-01 12:34`；旧格式变体含年份）
    pub(super) date: String,
    /// 修改时间数值键（[`parse_mtime_key`]），日期排序用；解析不了为 0
    pub(super) mtime: u64,
}

impl Entry {
    fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
}

/// 排序键：`S` 循环切换，`O` 在当前键上翻转方向。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum SortKey {
    Name,
    Size,
    Date,
}

impl SortKey {
    /// 名称 → 大小 → 日期 → 名称。
    pub(super) fn cycle(self) -> Self {
        match self {
            SortKey::Name => SortKey::Size,
            SortKey::Size => SortKey::Date,
            SortKey::Date => SortKey::Name,
        }
    }

    /// 标题栏中文标签。
    pub(super) fn label(self) -> &'static str {
        match self {
            SortKey::Name => "名称",
            SortKey::Size => "大小",
            SortKey::Date => "日期",
        }
    }

    /// 自然方向：名称升序，大小/日期降序（`S` 切键时重置到该方向）。
    pub(super) fn default_desc(self) -> bool {
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
pub(super) fn parse_ls(output: &str) -> std::result::Result<Vec<Entry>, String> {
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
    Some(Entry {
        name: name.to_owned(),
        kind,
        size,
        date,
        mtime,
    })
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
    let Some(ymd) = numeric_parts(date, '-') else {
        return 0;
    };
    let [y, mo, d] = ymd[..] else { return 0 };
    // time 位是纯数字：只可能是 4 位年份的旧格式变体，按该年年初近似
    if let Ok(year) = time.parse::<u64>() {
        return if (1000..10000).contains(&year) {
            key(year, 1, 1, 0, 0)
        } else {
            0
        };
    }
    let Some(hms) = numeric_parts(time, ':') else {
        return 0;
    };
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

/// 排序（原地）：目录始终置顶（不受方向影响），键内按 `key` 升序、`desc` 时
/// 反转为降序；同值退回名称序，保证顺序确定。
pub(super) fn sort_entries(entries: &mut [Entry], key: SortKey, desc: bool) {
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
pub(super) fn visible_indices(entries: &[Entry], filter: Option<&str>) -> Vec<usize> {
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

/// 造测试条目：date 置空串（不参与断言），mtime 由调用方给。
#[cfg(test)]
pub(super) fn mk(name: &str, kind: EntryKind, size: u64, mtime: u64) -> Entry {
    Entry {
        name: name.to_owned(),
        kind,
        size,
        date: String::new(),
        mtime,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        fn names(es: &[Entry]) -> Vec<&str> {
            es.iter().map(|e| e.name.as_str()).collect()
        }
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
        fn names(es: &[Entry]) -> Vec<&str> {
            es.iter().map(|e| e.name.as_str()).collect()
        }
        // 降序（自然方向）：新在前；mtime 0 视为最旧落末尾
        sort_entries(&mut entries, SortKey::Date, true);
        assert_eq!(
            names(&entries),
            ["new.txt", "mid.txt", "old.txt", "unknown.txt"]
        );
        // 升序：旧在前
        sort_entries(&mut entries, SortKey::Date, false);
        assert_eq!(
            names(&entries),
            ["unknown.txt", "old.txt", "mid.txt", "new.txt"]
        );
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
}
