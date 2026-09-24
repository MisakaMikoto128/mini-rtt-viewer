//! 用户偏好持久化:`%APPDATA%/MiniRttViewer/prefs.json`。
//!
//! - 字段全部 `serde(default)`:旧版本配置缺新字段、JSON 损坏、文件丢失一律回落
//!   默认值,绝不阻塞启动
//! - 原子写:先写 `.tmp` 再替换,崩溃不留半截文件(Windows 的 rename 不覆盖,
//!   先移除旧文件再改名)
//! - 保存时机由 web.rs 数据泵线程 tick_loop 节流(500ms 快照比对,不变不落盘);
//!   进程退出(关窗强退 / Ctrl-C)不做退出补写,最后 ≤500ms 的改动可能不落盘
//!   (gui.rs 模块头「跳过的清理与副作用」有说明)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 左侧面板全部用户可选项(下次启动恢复为上次状态)
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct StoredPrefs {
    /// 目标设备名(连接输入框)
    pub chip_name: String,
    /// 上次使用的 J-Link 序列号(索引随枚举顺序漂移,按序列号恢复才稳)
    pub jlink_serial: Option<u32>,
    /// 接口 0=SWD 1=JTAG
    pub iface_index: i32,
    /// 速度下拉索引
    pub speed_index: i32,
    /// RTT 通道 0-15
    pub channel: i32,
    /// 接收行尾 0=自动 1=CRLF 2=LF 3=CR 4=无
    pub rx_ending: i32,
    /// 发送行尾 0=CRLF 1=LF 2=CR 3=无
    pub send_ending: i32,
    /// 自动断帧开关
    pub auto_frame: bool,
    /// 断帧间隔文本(保持用户输入原样)
    pub frame_timeout: String,
    /// 自动滚动勾选
    pub auto_scroll: bool,
    /// HEX 发送模式
    pub hex_send: bool,
    /// HEX 接收模式(原始字节十六进制显示)
    pub hex_rx: bool,
    /// 字符集下拉索引(0=UTF-8 1=GBK 2=UTF-16 LE 3=Latin-1 4=ASCII)
    pub encoding_index: i32,
    /// 日志字号(px;桌面版遗留字段,现由前端 localStorage 管理、不经服务端,
    /// 仅随偏好底版原样写回;9-30 之外视为坏值不恢复为旧桌面版口径)
    pub log_font_px: i32,
    /// 设备信息折叠展开态
    pub info_expanded: bool,
    /// 深色模式(0.1.x 遗留字段,被 theme 取代;仅作旧偏好迁移来源,仍随快照写)
    pub dark_theme: bool,
    /// 主题索引:0=深色 1=浅色 2=OLED 纯黑 3=护眼暖色(越界回落深色)
    pub theme: i32,
    /// 发送历史(最新在前,去重,上限 50 条)
    pub send_history: Vec<String>,
    /// 窗口位置/尺寸(物理像素;全 0 = 不恢复)
    pub window_x: i32,
    pub window_y: i32,
    pub window_w: i32,
    pub window_h: i32,
    /// 保持屏幕常亮(SetThreadExecutionState)
    pub keep_awake: bool,
    /// 定时发送开关
    pub timer_send: bool,
    /// 定时发送间隔文本(秒,保持用户输入原样)
    pub timer_interval: String,
    /// 搜索正则开关(false = 字面量匹配)
    pub search_regex: bool,
}

impl Default for StoredPrefs {
    fn default() -> Self {
        Self {
            chip_name: String::new(),
            jlink_serial: None,
            iface_index: 0,
            speed_index: 5,
            channel: 0,
            rx_ending: 0,
            send_ending: 0,
            auto_frame: true,
            frame_timeout: "20".into(),
            auto_scroll: true,
            hex_send: false,
            hex_rx: false,
            encoding_index: 0,
            log_font_px: 13,
            info_expanded: false,
            dark_theme: true,
            theme: 0,
            send_history: Vec::new(),
            window_x: 0,
            window_y: 0,
            window_w: 0,
            window_h: 0,
            keep_awake: false,
            timer_send: false,
            timer_interval: "1".into(),
            search_regex: false,
        }
    }
}

/// 配置文件路径:`%APPDATA%/MiniRttViewer/prefs.json`(%APPDATA% 经
/// `dirs::config_dir()` 解析 = SHGetKnownFolderPath RoamingAppData,与历史
/// `env::var("APPDATA")` 同路径,单测 `dirs_config_dir_matches_legacy_appdata`
/// 锁定等价性;known folder API 在环境变量缺失的非常规环境也能解析)。
/// 环境变量 `RTT_PREFS_FILE` 显式指定完整文件路径时优先(测试/便携场景的
/// prefs 隔离纪律:指向临时文件,绝不读写真实 %APPDATA%)。
fn prefs_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RTT_PREFS_FILE") {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    dirs::config_dir().map(|d| d.join("MiniRttViewer").join("prefs.json"))
}

/// 读取偏好:文件缺失/损坏/字段缺失一律回落默认
pub fn load() -> StoredPrefs {
    match prefs_path() {
        Some(p) => load_from(&p),
        None => StoredPrefs::default(),
    }
}

/// 保存偏好(原子替换);无路径或写入失败静默返回 false(偏好保存失败不值得打扰用户)
pub fn save(prefs: &StoredPrefs) -> bool {
    match prefs_path() {
        Some(p) => save_to(&p, prefs).is_ok(),
        None => false,
    }
}

/// 从指定路径读(测试与 load 共用)
pub(crate) fn load_from(path: &std::path::Path) -> StoredPrefs {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 写指定路径(测试与 save 共用):建目录 → 写 .tmp → 移除旧文件 → 原子改名
pub(crate) fn save_to(path: &std::path::Path, prefs: &StoredPrefs) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(prefs).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, body)?;
    // Windows 的 rename 不覆盖已存在文件:先移除旧的再改名(窗口极小)
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mini-rtt-prefs-test-{tag}.json"))
    }

    #[test]
    fn round_trip_preserves_all_fields() {
        let p = tmp_path("roundtrip");
        let prefs = StoredPrefs {
            chip_name: "STM32F103RB".into(),
            jlink_serial: Some(888),
            iface_index: 1,
            speed_index: 3,
            channel: 7,
            rx_ending: 2,
            send_ending: 1,
            auto_frame: false,
            frame_timeout: "55".into(),
            auto_scroll: false,
            hex_send: true,
            hex_rx: true,
            encoding_index: 1,
            log_font_px: 16,
            info_expanded: true,
            dark_theme: false,
            theme: 2,
            send_history: vec!["read 3".into(), "led on".into()],
            window_x: -1920,
            window_y: 40,
            window_w: 1280,
            window_h: 800,
            keep_awake: true,
            timer_send: true,
            timer_interval: "0.5".into(),
            search_regex: true,
        };
        save_to(&p, &prefs).unwrap();
        let back = load_from(&p);
        assert_eq!(back, prefs);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn corrupt_or_missing_file_falls_back_to_default() {
        let p = tmp_path("corrupt");
        std::fs::write(&p, "{ not json !!!").unwrap();
        assert_eq!(load_from(&p), StoredPrefs::default());
        let missing = tmp_path("missing");
        let _ = std::fs::remove_file(&missing);
        assert_eq!(load_from(&missing), StoredPrefs::default());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn partial_json_fills_missing_fields_from_default() {
        let p = tmp_path("partial");
        std::fs::write(&p, r#"{"chip_name":"STM32F030C8"}"#).unwrap();
        let prefs = load_from(&p);
        assert_eq!(prefs.chip_name, "STM32F030C8");
        assert_eq!(prefs.speed_index, StoredPrefs::default().speed_index);
        assert!(prefs.auto_frame);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn save_overwrites_existing_file_atomically() {
        let p = tmp_path("overwrite");
        save_to(
            &p,
            &StoredPrefs {
                chip_name: "a".into(),
                ..Default::default()
            },
        )
        .unwrap();
        save_to(
            &p,
            &StoredPrefs {
                chip_name: "b".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(load_from(&p).chip_name, "b");
        // .tmp 不残留
        assert!(!p.with_extension("json.tmp").exists());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn dirs_config_dir_matches_legacy_appdata() {
        // APPDATA → dirs::config_dir() 替换的等价性证明:Windows 标准环境下
        // SHGetKnownFolderPath(FOLDERID_RoamingAppData) 与 %APPDATA% 环境变量
        // 解析到同一路径(逐分量比较,含大小写);RTT_PREFS_FILE 覆盖分支
        // 在此未设置时走默认解析,分支结构与替换前一致。
        let Ok(env_appdata) = std::env::var("APPDATA") else {
            return; // 环境变量被裁剪的非常规环境无从对比,跳过(不影响功能)
        };
        assert_eq!(
            dirs::config_dir(),
            Some(PathBuf::from(env_appdata)),
            "dirs::config_dir() 应与 %APPDATA% 环境变量同路径"
        );
    }
}
