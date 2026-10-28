//! 自定义主题扫描与路径校验。

use super::*;

/// 扫描到的自定义主题。
pub(crate) struct CustomTheme {
    /// 主题 id = 文件名去 .css 后缀(/api/theme-css/{id} 的 {id})
    pub(crate) id: String,
    /// 显示名 = 文件名(含扩展名,与 id 区分)
    pub(crate) name: String,
    /// CSS 文件路径(来自本机 read_dir,非用户输入拼装)
    pub(crate) path: PathBuf,
}

/// 自定义主题目录:exe 旁 themes/(exe 定位失败 → 相对路径 themes/)。
/// 目录不存在 = 无自定义主题(扫描静默返回空)。
pub(crate) fn themes_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("themes")))
        .unwrap_or_else(|| PathBuf::from("themes"))
}

/// 主题文件名白名单:ASCII 字母/数字/点/短横线/下划线、≤64 字节、.css 结尾且
/// 去后缀后 id 非空(排除分隔符、控制字符、全角/Unicode 混淆名;
/// 与 SerialHub themes.rs 同规则)
pub(crate) fn valid_theme_file_name(file: &str) -> bool {
    file.len() > 4
        && file.len() <= 64
        && file.ends_with(".css")
        && file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// 扫描自定义主题:目录下每个 *.css = 一套主题,id 字典序;目录不存在/不可读
/// = 空列表。每次请求现扫(拖入 .css 即新主题,零注册,无需重启)。
pub(crate) fn scan_custom_themes(dir: &std::path::Path) -> Vec<CustomTheme> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<CustomTheme> = rd
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !valid_theme_file_name(&name) {
                return None;
            }
            Some(CustomTheme {
                id: name[..name.len() - 4].to_string(),
                name,
                path: e.path(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}
