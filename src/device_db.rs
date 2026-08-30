//! J-Link 设备库(目标设备下拉候选)。
//!
//! 来源策略与原项目一致:磁盘缓存优先(零 DLL 调用,秒回);
//! 缓存未命中才在后台线程加载 DLL 逐索引枚举 `JLINKARM_DEVICE_GetInfo`,
//! 写缓存后把名单发回 UI。枚举期间置位 BUSY,连接路径检查它以规避
//! 原 Python 项目踩过的「枚举与 connect 并发损坏 DLL TLS」竞态。

use crate::jlink_dll::JLinkDll;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// 设备库枚举进行中(仅首次启动无缓存时有窗口);连接前检查
static DEVICE_DB_BUSY: AtomicBool = AtomicBool::new(false);

/// 后台枚举结果:目标设备库候选 / 本机接入的 J-Link 列表
pub enum DbResult {
    DeviceNames(Vec<String>),
    Emulators(Vec<(u32, String)>),
}

pub fn busy() -> bool {
    DEVICE_DB_BUSY.load(Ordering::Relaxed)
}

/// 缓存文件:%APPDATA%\MiniRttViewer\device_names.txt(每行一个设备名)
fn cache_path() -> Option<PathBuf> {
    let base = std::env::var("APPDATA").ok()?;
    Some(PathBuf::from(base).join("MiniRttViewer").join("device_names.txt"))
}

fn load_cache() -> Option<Vec<String>> {
    let path = cache_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let names: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    (!names.is_empty()).then_some(names)
}

fn write_cache(names: &[String]) {
    if let Some(path) = cache_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let body = names.join("\n");
        let _ = std::fs::write(path, body);
    }
}

/// 归一:大写比较排序 + 去重(原项目同款语义)
fn normalize(mut names: Vec<String>) -> Vec<String> {
    names.sort_by_key(|a| a.to_uppercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

/// 后台线程:①目标设备库候选——磁盘缓存命中零 DLL 调用,未命中才枚举;
/// ②本机接入的 J-Link 列表——每次启动都枚举(插拔会变)。失败静默:下拉为空,
/// 用户仍可手动输入型号 / DLL 自动选调试器。
pub fn spawn_background(tx: std::sync::mpsc::Sender<DbResult>) {
    std::thread::spawn(move || {
        if let Some(names) = load_cache() {
            let _ = tx.send(DbResult::DeviceNames(names));
        }
        DEVICE_DB_BUSY.store(true, Ordering::Relaxed);
        if let Ok(jlink) = JLinkDll::load() {
            if load_cache().is_none() {
                let mut names = jlink.enumerate_device_names();
                // 个别 DLL 版本 GetInfo 需要先 Open 才能读设备库:首轮为空则 Open 后重试一次
                if names.is_empty() {
                    jlink.open();
                    names = jlink.enumerate_device_names();
                }
                if !names.is_empty() {
                    let names = normalize(names);
                    write_cache(&names);
                    let _ = tx.send(DbResult::DeviceNames(names));
                }
            }
            let _ = tx.send(DbResult::Emulators(jlink.enumerate_emulators()));
        }
        DEVICE_DB_BUSY.store(false, Ordering::Relaxed);
    });
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalize_sorts_case_insensitive_and_dedups() {
        let out = normalize(vec![
            "STM32F030F4".into(),
            "stm32f103c8".into(),
            "STM32F030C8".into(),
            "STM32F103C8".into(),
            "GD32F150C8".into(),
        ]);
        assert_eq!(
            out,
            vec![
                "GD32F150C8".to_string(),
                "STM32F030C8".to_string(),
                "STM32F030F4".to_string(),
                // 大小写去重保留排序后先出现的原始写法
                "stm32f103c8".to_string(),
            ]
        );
        // 排序应是大写序:stm32f103c8(小写)与 STM32F103C8 去重后留一个
        assert_eq!(out.iter().filter(|n| n.to_uppercase() == "STM32F103C8").count(), 1);
    }
}
