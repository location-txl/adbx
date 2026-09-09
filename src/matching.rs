//! 包名模糊匹配：供多个子命令（stop / clear 等）共用的纯函数。

/// 包名匹配结果。
pub enum PackageMatch {
    /// 关键词恰好等于某个完整包名（精确优先，直接采用）
    Exact(String),
    /// 关键词交集过滤后唯一命中
    Unique(String),
    /// 多个包命中，需要调用方提示用户细化关键词
    Multiple(Vec<String>),
    /// 没有任何包命中
    None,
}

/// 按关键词匹配包名，多关键词为 AND 关系。
///
/// 匹配规则（按优先级）：
/// 1. 仅当单个关键词且大小写不敏感地等于某个完整包名时，返回 [`PackageMatch::Exact`]，
///    避免误伤包含该词的其他包（如输入完整包名 `com.v1.chat` 时不希望停掉 `com.v1.chat.xx`）
/// 2. 否则做子串交集过滤：包名（小写化后）必须同时包含所有关键词（小写化后），
///    命中数决定返回 Unique / Multiple / None
///
/// * `packages` - 设备上的全量包名
/// * `keywords` - 用户输入的关键词，至少一个
///
/// 纯函数，不触碰设备，可直接单测。
///
/// # 示例
///
/// ```
/// use adbx::matching::{PackageMatch, match_packages};
/// let pkgs = vec!["com.v1.chat".to_string(), "com.v1.music".to_string()];
/// match match_packages(&pkgs, &["v1".into(), "chat".into()]) {
///     PackageMatch::Unique(p) => assert_eq!(p, "com.v1.chat"),
///     _ => panic!("应为唯一命中"),
/// }
/// ```
pub fn match_packages(packages: &[String], keywords: &[String]) -> PackageMatch {
    // 精确匹配优先：单个关键词恰好等于完整包名时直接采用
    if let [keyword] = keywords
        && let Some(pkg) = packages.iter().find(|p| p.eq_ignore_ascii_case(keyword))
    {
        return PackageMatch::Exact(pkg.clone());
    }

    let lower_keywords: Vec<String> = keywords.iter().map(|k| k.to_ascii_lowercase()).collect();
    let hits: Vec<&String> = packages
        .iter()
        .filter(|p| {
            let lower = p.to_ascii_lowercase();
            lower_keywords.iter().all(|k| lower.contains(k))
        })
        .collect();

    match hits.as_slice() {
        [] => PackageMatch::None,
        [only] => PackageMatch::Unique((*only).clone()),
        many => PackageMatch::Multiple(many.iter().map(|p| (*p).clone()).collect()),
    }
}

/// 把关键词列表拼成 `"v1" "chat"` 形式，用于错误提示。
pub fn keywords_summary(keywords: &[String]) -> String {
    keywords
        .iter()
        .map(|k| format!("\"{k}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 匹配结果 → 执行动作的统一错误文案：多命中列候选、零命中报没有匹配。
///
/// 各子命令命中唯一/精确包后自行执行 adb 动作，本函数只兜住不能执行的场景。
pub fn match_failure(keywords: &[String], matched: PackageMatch) -> anyhow::Error {
    match matched {
        // 多命中时列出候选，让用户换更精确的关键词
        PackageMatch::Multiple(candidates) => anyhow::anyhow!(
            "多个包匹配 {}：\n{}\n  请使用更精确的关键词重试",
            keywords_summary(keywords),
            candidates
                .iter()
                .map(|c| format!("    {c}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        PackageMatch::None => anyhow::anyhow!(
            "没有包同时包含 {}",
            keywords_summary(keywords)
        ),
        // 唯一/精确命中不是失败场景，调用方不该把这种结果传进来
        PackageMatch::Exact(_) | PackageMatch::Unique(_) => {
            unreachable!("命中唯一/精确包时不应走失败分支")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packages() -> Vec<String> {
        vec![
            "com.v1.chat".to_string(),
            "com.v1.music".to_string(),
            "com.example.Maps".to_string(),
        ]
    }

    #[test]
    fn exact_match_should_take_priority_over_substring_hits() {
        // "com.v1.chat" 同时也是 "com.v1.chat" 的子串场景，精确命中必须赢
        let pkgs = vec![
            "com.v1.chat".to_string(),
            "com.v1.chat.plugin".to_string(),
        ];
        assert!(matches!(
            match_packages(&pkgs, &["com.v1.chat".to_string()]),
            PackageMatch::Exact(p) if p == "com.v1.chat"
        ));
    }

    #[test]
    fn single_keyword_should_return_unique_substring_hit() {
        assert!(matches!(
            match_packages(&packages(), &["music".to_string()]),
            PackageMatch::Unique(p) if p == "com.v1.music"
        ));
    }

    #[test]
    fn single_keyword_should_return_multiple_when_ambiguous() {
        assert!(matches!(
            match_packages(&packages(), &["v1".to_string()]),
            PackageMatch::Multiple(hits) if hits.len() == 2
        ));
    }

    #[test]
    fn multiple_keywords_should_require_all_present() {
        // "v1 chat" 只命中 com.v1.chat；com.v1.music 缺 chat，com.example.Maps 缺 v1
        assert!(matches!(
            match_packages(&packages(), &["v1".to_string(), "chat".to_string()]),
            PackageMatch::Unique(p) if p == "com.v1.chat"
        ));
    }

    #[test]
    fn match_should_be_case_insensitive() {
        assert!(matches!(
            match_packages(&packages(), &["MAPS".to_string()]),
            PackageMatch::Unique(p) if p == "com.example.Maps"
        ));
    }

    #[test]
    fn no_hit_should_return_none() {
        assert!(matches!(
            match_packages(&packages(), &["v1".to_string(), "zzz".to_string()]),
            PackageMatch::None
        ));
    }
}
