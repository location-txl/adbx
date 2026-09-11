//! 设备端路径运算与拉取清单整理（纯函数，可单测）。
//!
//! 路径均为以 `/` 开头的设备端绝对路径（App.cwd 的不变量）：
//! [`join_path`]/[`parent_path`]/[`base_name`] 做目录导航运算，
//! [`filter_covered`]/[`dup_base_names`] 在拉取前整理标记清单。

use std::collections::HashSet;

/// 拼接设备端绝对路径：`join_path("/sdcard", "DCIM")` → `/sdcard/DCIM`。
pub(super) fn join_path(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// 父目录路径：`/` 无父级返回 None；`/sdcard` → `/`；`/a/b` → `/a`。
pub(super) fn parent_path(path: &str) -> Option<String> {
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
pub(super) fn base_name(path: &str) -> &str {
    if path == "/" {
        "/"
    } else {
        path.rsplit('/').next().unwrap_or(path)
    }
}

/// 去掉尾部 `/`：`/sdcard/` → `/sdcard`；全斜杠或空串归一为 `/`，
/// 保证结果恒非空、以 `/` 开头（App.cwd 的不变量）。
pub(super) fn normalize_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// 校验新建文件夹名（纯函数，可单测）：拒绝空/仅空白、`.`、`..`、含 `/`、
/// 含控制字符的名字；Err 带可直接展示的中文原因。不自动 trim（所见即所建）。
pub(super) fn validate_dir_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("文件夹名不能为空".to_owned());
    }
    if matches!(name, "." | "..") {
        return Err("文件夹名不能是 . 或 ..".to_owned());
    }
    if name.contains('/') {
        return Err("文件夹名不能包含 /（只在当前目录下新建一层）".to_owned());
    }
    if name.bytes().any(|b| b.is_ascii_control()) {
        return Err("文件夹名不能包含控制字符".to_owned());
    }
    Ok(())
}

/// 剔除已被其他标记路径覆盖的子路径（如已标记 `/a` 就不再单独拉 `/a/b.txt`），
/// 返回按字典序排序的 Vec，保证拉取顺序确定。
pub(super) fn filter_covered(marked: &HashSet<String>) -> Vec<String> {
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
pub(super) fn fold_name(name: &str) -> String {
    name.to_lowercase()
}

/// Linux 上直接返回原名，避免无谓的分配。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(super) fn fold_name(name: &str) -> String {
    name.to_owned()
}

/// 找出同批拉取清单中 base_name 重复的名字（纯函数，可单测）。
///
/// 所有条目统一拉到 `out_dir/<base_name>`，不同父目录下的同名条目
/// （如 `/a/DCIM` 与 `/b/DCIM`，或大小写不敏感文件系统上的 `/b/dcim`）
/// 会互相覆盖/合并，调用方据此在拉取前提示。
/// 返回按字典序排序的去重名字列表；无冲突返回空。
pub(super) fn dup_base_names(targets: &[String]) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn validate_dir_name_accepts_and_rejects() {
        // 合法：普通名、含空格/中文、含单引号（shell_quote 会转义）
        assert!(validate_dir_name("DCIM").is_ok());
        assert!(validate_dir_name("新建 目录").is_ok());
        assert!(validate_dir_name("it's").is_ok());
        // 非法：空、仅空白、.、..、含 /、含控制字符
        assert!(validate_dir_name("").is_err());
        assert!(validate_dir_name("   ").is_err());
        assert!(validate_dir_name(".").is_err());
        assert!(validate_dir_name("..").is_err());
        assert!(validate_dir_name("a/b").is_err());
        assert!(validate_dir_name("a\u{1b}b").is_err());
    }

    #[test]
    fn filter_covered_drops_children_of_marked_parents() {
        // 已标记 /a 时，其子路径 /a/b.txt 不再单独拉取
        let marked: HashSet<String> = ["/a", "/a/b.txt", "/c"]
            .map(String::from)
            .into_iter()
            .collect();
        assert_eq!(
            filter_covered(&marked),
            vec!["/a".to_owned(), "/c".to_owned()]
        );
        // 无嵌套关系时全部保留，且顺序确定
        let flat: HashSet<String> = ["/y", "/x"].map(String::from).into_iter().collect();
        assert_eq!(
            filter_covered(&flat),
            vec!["/x".to_owned(), "/y".to_owned()]
        );
    }

    #[test]
    fn dup_base_names_finds_cross_dir_collisions() {
        // 不同父目录下的同名条目会落到同一本地路径 out/<名字>
        let targets: Vec<String> = ["/a/DCIM", "/b/DCIM", "/b/DCIM2", "/c/Music"]
            .map(String::from)
            .into_iter()
            .collect();
        assert_eq!(dup_base_names(&targets), vec!["DCIM".to_owned()]);
        // 无重复时为空
        let flat: Vec<String> = ["/x/a", "/y/b"].map(String::from).into_iter().collect();
        assert!(dup_base_names(&flat).is_empty());
    }
}
