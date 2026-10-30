// release 版隐藏控制台黑框;debug 保留方便看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 入口:CLI 解析 → 分派 窗口 / 纯服务 两种形态(SerialHub main.rs 同构)。
//!
//! - 窗口(默认):`gui::run_gui` —— 主线程 tao 事件循环 + wry WebView + 托盘,
//!   tokio 服务在后台线程(整合方式与坑见 gui.rs / web.rs 模块头注释);
//! - `--no-window`:纯服务前台阻塞(自动化测试与脚本场景,无 GUI 依赖路径);
//! - 两种形态共用同一套数据层(worker/LogPump/device_db)与内嵌管理台。

use mini_rtt_viewer::gui;
use mini_rtt_viewer::web::{self, WebOptions};
use std::net::IpAddr;

const USAGE: &str = "\
mini-rtt-viewer — J-Link RTT 查看器(Rust 数据层 + 内嵌 Web 管理台)

用法: mini-rtt-viewer [选项]

选项:
  --backend <id> 启动时选定数据采集后端(当前支持 jlink;缺省读偏好文件,
                非法值忽略回落)
  --port <n>    HTTP 监听端口(默认 8686;端口即单实例互斥)
  --listen <ip> HTTP 监听地址(默认 127.0.0.1 仅本机;0.0.0.0 开放局域网/互联网,
                管理台无鉴权,仅建议可信网络使用)
  --no-window   纯服务模式:不开窗口与托盘,只跑本机服务(自动化/脚本场景)
  --no-tray     不创建系统托盘图标(窗口模式)
  -h, --help    显示本帮助并退出

窗口模式:桌面窗口内嵌管理台 WebView,关窗即退出;也可经托盘菜单在浏览器打开。
浏览器直接访问 http://127.0.0.1:<端口> 同样可用。";

#[derive(Debug)]
struct ParsedArgs {
    backend: Option<String>,
    port: u16,
    listen: IpAddr,
    no_window: bool,
    no_tray: bool,
    wait_pid: Option<u32>,
}

fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    let mut parsed = ParsedArgs {
        backend: None,
        port: 8686,
        listen: IpAddr::from([127, 0, 0, 1]),
        no_window: false,
        no_tray: false,
        wait_pid: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--backend" => {
                let Some(v) = args
                    .get(i + 1)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                else {
                    return Err("--backend 需要一个后端 id 参数(如 jlink)".into());
                };
                // 合法性(是否在可用列表)由 serve 校验:非法值忽略回落偏好/缺省
                parsed.backend = Some(v);
                i += 1;
            }
            "--no-window" => parsed.no_window = true,
            "--no-tray" => parsed.no_tray = true,
            "--port" => {
                let Some(v) = args.get(i + 1).and_then(|s| s.parse::<u16>().ok()) else {
                    return Err("--port 需要一个 1-65535 的数字参数".into());
                };
                if v == 0 {
                    // 0 会被 OS 分配随机端口:端口互斥(第二实例唤起防重)与
                    // 管理台固定地址的承诺都会失效,与 --help 口径一并拦下
                    return Err("--port 需要 1-65535(0 会绑定随机端口)".into());
                }
                parsed.port = v;
                i += 1;
            }
            "--listen" => {
                let Some(v) = args.get(i + 1).and_then(|s| s.parse::<IpAddr>().ok()) else {
                    return Err("--listen 需要一个 IP 地址参数(如 0.0.0.0)".into());
                };
                parsed.listen = v;
                i += 1;
            }
            "--update-wait-pid" => {
                let Some(v) = args
                    .get(i + 1)
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|pid| *pid != 0)
                else {
                    return Err("--update-wait-pid 需要 1-4294967295 的数字参数".into());
                };
                parsed.wait_pid = Some(v);
                i += 1;
            }
            other => return Err(format!("未知参数 '{other}'(--help 查看用法)")),
        }
        i += 1;
    }
    Ok(parsed)
}

#[cfg(windows)]
fn wait_for_previous_process(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    const WAIT_TIMEOUT_MS: u32 = 10_000;
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        // 旧进程可能在替换进程取得句柄前就已退出,此时无需继续等待。
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(());
        }
        return Err(format!("无法等待旧进程 {pid} 退出: {error}"));
    }
    let result = unsafe { WaitForSingleObject(handle, WAIT_TIMEOUT_MS) };
    let wait_error = std::io::Error::last_os_error();
    unsafe {
        CloseHandle(handle);
    }
    match result {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(format!("等待旧进程 {pid} 退出超过 10 秒")),
        WAIT_FAILED => Err(format!("等待旧进程 {pid} 退出失败: {wait_error}")),
        other => Err(format!("等待旧进程 {pid} 退出返回异常状态 {other}")),
    }
}

#[cfg(not(windows))]
fn wait_for_previous_process(_pid: u32) -> Result<(), String> {
    Err("当前平台不支持更新重启进程等待".into())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    let parsed = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("mini-rtt-viewer: {error}");
            std::process::exit(2);
        }
    };
    if let Some(pid) = parsed.wait_pid {
        if let Err(error) = wait_for_previous_process(pid) {
            eprintln!("mini-rtt-viewer: {error}");
            std::process::exit(1);
        }
    }
    let opts = WebOptions {
        backend: parsed.backend,
        port: parsed.port,
        listen: parsed.listen,
        // 窗口模式由 WebView 承载界面、托盘菜单按需开浏览器;纯服务面向自动化,
        // 都不自动弹浏览器(RTT_WEB_NO_BROWSER 环境变量语义保留给 open_browser=true 的调用方)
        open_browser: false,
        update_manager: None,
    };

    if parsed.no_window {
        // 纯服务:前台阻塞直到 Ctrl-C / 进程被杀;bind 失败(端口被占)非零退出
        if let Err(e) = web::run(opts) {
            eprintln!("mini-rtt-viewer: {e}");
            std::process::exit(1);
        }
        return;
    }

    // 窗口模式:run_gui 只在"窗口创建之前"的失败路径返回(如端口被占用);
    // 事件循环启动后进程由事件循环接管。
    if let Err(e) = gui::run_gui(opts, parsed.no_tray) {
        eprintln!("mini-rtt-viewer: {e}");
        std::process::exit(1);
    }
    unreachable!("tao 事件循环不返回");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_wait_pid_is_numeric_and_bounded() {
        assert_eq!(
            parse_args(&["--update-wait-pid".into(), "1234".into()])
                .unwrap()
                .wait_pid,
            Some(1234)
        );
        assert_eq!(
            parse_args(&["--update-wait-pid".into(), u32::MAX.to_string()])
                .unwrap()
                .wait_pid,
            Some(u32::MAX)
        );
        for invalid in ["0", "abc", "-1", "4294967296", "C:\\Windows\\cmd.exe"] {
            assert!(
                parse_args(&["--update-wait-pid".into(), invalid.into()]).is_err(),
                "invalid PID was accepted: {invalid}"
            );
        }
        assert!(parse_args(&["--update-wait-pid".into()]).is_err());
    }
}
