//! 无界面验证:枚举本机接入的 J-Link 调试器(EMU_GetList)。
//! 用法: cargo run --example emu_check

use mini_rtt_viewer::jlink_dll::JLinkDll;

fn main() -> anyhow::Result<()> {
    let jlink = JLinkDll::load()?;
    let emus = jlink.enumerate_emulators();
    if emus.is_empty() {
        println!("未检测到 J-Link");
        return Ok(());
    }
    println!("检测到 {} 台 J-Link:", emus.len());
    for (sn, product) in emus {
        println!("  - {product}: {sn}");
    }
    Ok(())
}
