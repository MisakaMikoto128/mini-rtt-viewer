//! 薄启动层(ADR-11:UI 全面转 Web 形态):解析命令行参数后交给
//! `web::run` 启动本机服务并自动打开浏览器管理台。业务规则一律在 lib
//! (web / rtt / log_model / config / …),本文件不出现任何业务分支。

// release 版隐藏控制台黑框;debug 保留方便看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use mini_rtt_viewer::web::{self, WebOptions};

/// 用法说明(--help / -h 打印后 exit 0,不占端口)
fn print_usage() {
    println!(
        "mini-rtt-viewer — Mini RTT Viewer(浏览器管理台形态,Rust 数据层 + 内嵌 Web UI)

用法: mini-rtt-viewer [选项]

选项:
  --demo-log    使用内置演示数据源,无需 J-Link 设备即可体验/测试
  --port <n>    HTTP 监听端口(默认 8686,仅绑定 127.0.0.1)
  --no-open     不自动打开浏览器(仅启动服务)
  -h, --help    显示本帮助并退出

环境变量:
  RTT_WEB_NO_BROWSER=1    跳过自动打开浏览器(无头/测试场景)

启动后浏览器访问 http://127.0.0.1:<端口>;端口被占 = 已有实例运行"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // --help / -h:打印用法后退出
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }
    let demo = args.iter().any(|a| a == "--demo-log");
    let no_open = args.iter().any(|a| a == "--no-open");
    let port: u16 = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse().ok())
        .unwrap_or(8686);
    // 未知参数:警告到 stderr 但继续启动(向后兼容,不让旧调用方式直接失败)
    let mut skip_next = false;
    for a in args.iter().skip(1) {
        if skip_next {
            skip_next = false; // --port 的值参数也算已知
            continue;
        }
        match a.as_str() {
            "--demo-log" | "--no-open" | "--help" | "-h" => {}
            "--port" => skip_next = true,
            other => eprintln!("[mini-rtt-viewer] 警告:未知参数 '{other}'(已忽略;--help 查看用法)"),
        }
    }

    if let Err(e) = web::run(WebOptions {
        demo,
        port,
        open_browser: !no_open,
    }) {
        eprintln!("[mini-rtt-viewer] {e:#}");
        std::process::exit(1);
    }
}
