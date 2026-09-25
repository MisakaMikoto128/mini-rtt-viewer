// release 版隐藏控制台黑框;debug 保留方便看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 入口:CLI 解析 → 分派 窗口 / 纯服务 两种形态(SerialHub main.rs 同构)。
//!
//! - 窗口(默认):`gui::run_gui` —— 主线程 tao 事件循环 + wry WebView + 托盘,
//!   tokio 服务在后台线程(整合方式与坑见 gui.rs / web.rs 模块头注释);
//! - `--no-window`:纯服务前台阻塞(自动化测试与脚本场景,无 GUI 依赖路径);
//! - 两种形态共用同一套数据层(worker/LogPump/demo/device_db)与内嵌管理台。

use mini_rtt_viewer::gui;
use mini_rtt_viewer::web::{self, WebOptions};
use std::net::IpAddr;

const USAGE: &str = "\
mini-rtt-viewer — J-Link RTT 查看器(Rust 数据层 + 内嵌 Web 管理台)

用法: mini-rtt-viewer [选项]

选项:
  --demo-log    使用内置演示数据源,无需 J-Link 设备即可体验/测试
  --port <n>    HTTP 监听端口(默认 8686;端口即单实例互斥)
  --listen <ip> HTTP 监听地址(默认 127.0.0.1 仅本机;0.0.0.0 开放局域网/互联网,
                管理台无鉴权,仅建议可信网络使用)
  --no-window   纯服务模式:不开窗口与托盘,只跑本机服务(自动化/脚本场景)
  --no-tray     不创建系统托盘图标(窗口模式)
  -h, --help    显示本帮助并退出

窗口模式:桌面窗口内嵌管理台 WebView,关窗即退出;也可经托盘菜单在浏览器打开。
浏览器直接访问 http://127.0.0.1:<端口> 同样可用。";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    let mut demo = false;
    let mut port: u16 = 8686;
    let mut listen: IpAddr = IpAddr::from([127, 0, 0, 1]);
    let mut no_window = false;
    let mut no_tray = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--demo-log" => demo = true,
            "--no-window" => no_window = true,
            "--no-tray" => no_tray = true,
            "--port" => {
                let Some(v) = args.get(i + 1).and_then(|s| s.parse::<u16>().ok()) else {
                    eprintln!("mini-rtt-viewer: --port 需要一个 1-65535 的数字参数");
                    std::process::exit(2);
                };
                if v == 0 {
                    // 0 会被 OS 分配随机端口:端口互斥(第二实例唤起防重)与
                    // 管理台固定地址的承诺都会失效,与 --help 口径一并拦下
                    eprintln!("mini-rtt-viewer: --port 需要 1-65535(0 会绑定随机端口)");
                    std::process::exit(2);
                }
                port = v;
                i += 1;
            }
            "--listen" => {
                let Some(v) = args.get(i + 1).and_then(|s| s.parse::<IpAddr>().ok()) else {
                    eprintln!("mini-rtt-viewer: --listen 需要一个 IP 地址参数(如 0.0.0.0)");
                    std::process::exit(2);
                };
                listen = v;
                i += 1;
            }
            other => {
                eprintln!("mini-rtt-viewer: 未知参数 '{other}'(--help 查看用法)");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let opts = WebOptions {
        demo,
        port,
        listen,
        // 窗口模式由 WebView 承载界面、托盘菜单按需开浏览器;纯服务面向自动化,
        // 都不自动弹浏览器(RTT_WEB_NO_BROWSER 环境变量语义保留给 open_browser=true 的调用方)
        open_browser: false,
    };

    if no_window {
        // 纯服务:前台阻塞直到 Ctrl-C / 进程被杀;bind 失败(端口被占)非零退出
        if let Err(e) = web::run(opts) {
            eprintln!("mini-rtt-viewer: {e}");
            std::process::exit(1);
        }
        return;
    }

    // 窗口模式:run_gui 只在"窗口创建之前"的失败路径返回(如端口被占用);
    // 事件循环启动后进程由事件循环接管。
    if let Err(e) = gui::run_gui(opts, no_tray) {
        eprintln!("mini-rtt-viewer: {e}");
        std::process::exit(1);
    }
    unreachable!("tao 事件循环不返回");
}
