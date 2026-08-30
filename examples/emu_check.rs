//! 无界面验证:枚举本机接入的 J-Link,并逐一实测"多台时选定指定序列号"的方案。
//! 每个策略打开后立刻关闭,打印选定 SN 与实际打开 SN 是否一致。
//! 用法: cargo run --example emu_check
//! 注意:若 DLL 弹 probe 选择窗需人工点掉(默认第一台),输出会标明该策略失效。

use mini_rtt_viewer::jlink_dll::JLinkDll;

fn main() -> anyhow::Result<()> {
    let jlink = JLinkDll::load()?;
    let emus = jlink.enumerate_emulators();
    if emus.len() < 2 {
        println!("需要 >=2 台 J-Link 才能验证选定,当前 {} 台", emus.len());
        return Ok(());
    }
    for (sn, product) in &emus {
        println!("  - {product}: {sn}");
    }
    let (want, _) = emus.last().unwrap();
    println!("目标:选定最后一台 {want}\n");

    // 策略 1:SetHostIF 命令串(官方文档:Connect to J-Link via USB (SN xxx))
    println!("== 策略1: ExecCommand(\"SetHostIF USB = {want}\") ==");
    jlink.exec_command(&format!("SetHostIF USB = {want}"));
    jlink.open();
    report(&jlink, *want);
    jlink.close();

    // 策略 2:SelectByUSBSN + OpenEx(pylink 同款入口)
    println!("== 策略2: SelectByUSBSN + OpenEx ==");
    let rc = jlink.select_by_usb_sn(*want);
    println!("SelectByUSBSN rc={rc}");
    jlink.open_ex();
    report(&jlink, *want);
    jlink.close();

    // 策略 3:SelectByUSBSN + SetHostIF 双管齐下 + OpenEx
    println!("== 策略3: SelectByUSBSN + SetHostIF + OpenEx ==");
    let rc = jlink.select_by_usb_sn(*want);
    println!("SelectByUSBSN rc={rc}");
    jlink.exec_command(&format!("SetHostIF USB = {want}"));
    jlink.open_ex();
    report(&jlink, *want);
    jlink.close();
    Ok(())
}

fn report(jlink: &JLinkDll, want: u32) {
    let opened = jlink.serial_number();
    if opened == want {
        println!("   Open 后 SN={opened} == 选定 ✓");
    } else {
        println!("   Open 后 SN={opened} != 选定 {want} ✗(策略失效)");
    }
}
