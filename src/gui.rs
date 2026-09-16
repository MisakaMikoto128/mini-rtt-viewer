//! GUI 壳(SerialHub gui.rs 蓝本适配):tao 事件循环(主线程)+ wry WebView
//! (加载内嵌管理台 http://127.0.0.1:{port}/,不做第二套界面)+ tray-icon 托盘。
//!
//! ⚠ 整合要点(与 SerialHub 同源,先读这里):
//! - Win32 规定窗口/托盘/菜单必须在主线程;tao 的 `EventLoop::run` 占死主线程且
//!   永不返回;因此 tokio 服务整体搬后台线程(`web::spawn_gui_service`),
//!   主线程只跑事件循环;
//! - 服务 → 主线程:①就绪握手走 std mpsc ready 通道(窗口创建前就知道 bind
//!   成败,失败弹 MessageBox);②连接状态变化经 `EventLoopProxy<UserEvent>`
//!   打回主循环驱动托盘图标(事件循环未启动前发送的事件会排队);
//! - 事件循环闭包里绝不能 await / block_on —— 会冻结 Win32 消息泵,窗口假死;
//! - 简化决策(架构师指定):**关窗 = 退出进程**(SerialHub 的"关窗到托盘 +
//!   首次气泡"未移植);退出不经服务优雅停机(本项目数据层无 COM 释放),
//!   托盘退出/关窗都直接 `ControlFlow::Exit`,服务线程随进程终止;
//! - 第二实例唤醒未移植:端口即互斥(bind 失败弹窗报错)已满足单实例语义;
//! - 托盘图标只有一份资产(app-32.png):未连接态用亮度灰化变体区分
//!   (SerialHub 三态图标依赖三份交付资产,本项目不引入)。

use tao::event::{Event, WindowEvent};
use tao::event_loop::ControlFlow;
use tao::window::{Icon as TaoIcon, WindowBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, TrayIconBuilder, TrayIconEvent};

use crate::web::{self, WebOptions};

// 构建期嵌入的图标资产(build.rs 生成,存在才 Some;灰化托盘变体在构建期算好,
// 运行时零解码/零分配)
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded_icons.rs"));
}
use embedded::{RgbaIcon, TRAY_ICON_GRAY_RGBA, TRAY_ICON_RGBA, WINDOW_ICON_RGBA};

/// 服务 → 事件循环 的用户事件(经 EventLoopProxy 从后台线程打回主线程)。
#[derive(Debug, Clone)]
pub enum UserEvent {
    /// 连接状态变化(托盘图标刷新)。
    ConnectedChanged(bool),
    ShowWindow,
    OpenBrowser,
    /// 托盘菜单「退出」:简化停机,直接结束事件循环(见模块头)。
    Quit,
}

// 菜单项 id(muda MenuId)
const ID_SHOW: &str = "show";
const ID_BROWSER: &str = "browser";
const ID_QUIT: &str = "quit";

/// 就绪握手超时:服务线程(tokio runtime + bind)应在数秒内完成,
/// 首启无设备库缓存时枚举与 bind 并行,不挡 bind。
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub fn run_gui(opts: WebOptions, no_tray: bool) -> Result<(), String> {
    // 事件循环必须建在主线程(Win32);build 需要 &mut
    let mut loop_builder = tao::event_loop::EventLoopBuilder::<UserEvent>::with_user_event();
    let event_loop = loop_builder.build();
    let proxy = event_loop.create_proxy();

    // —— 服务线程:tokio runtime 整体在后台线程跑(见模块头注释)——
    // 连接状态变化 → UserEvent(事件循环启动前发送的事件会排队)
    let proxy_for_state = proxy.clone();
    let on_event: web::OnEvent = std::sync::Arc::new(move |ev| match ev {
        web::ServiceEvent::ConnectedChanged(connected) => {
            let _ = proxy_for_state.send_event(UserEvent::ConnectedChanged(connected));
        }
        web::ServiceEvent::Ready(_) => {} // ready 握手走独立通道,不经这里
    });
    let ready_rx = web::spawn_gui_service(opts, on_event)?;

    // —— 等服务就绪(端口被占用在此处报错;MessageBox 让双击用户可见)——
    let addr = match ready_rx.recv_timeout(READY_TIMEOUT) {
        Ok(Ok(addr)) => addr,
        Ok(Err(e)) => {
            fatal_msgbox(&e);
            return Err(e);
        }
        Err(_) => {
            let e = "服务线程 15s 内未就绪".to_string();
            fatal_msgbox(&e);
            return Err(e);
        }
    };

    // —— 主窗口:WebView 内嵌现有管理台 ——
    let window = WindowBuilder::new()
        .with_title("Mini RTT Viewer")
        .with_inner_size(tao::dpi::LogicalSize::new(1120.0, 780.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(640.0, 480.0))
        .with_window_icon(Some(tao_icon_any(WINDOW_ICON_RGBA, DOT_IDLE)))
        .build(&event_loop)
        .map_err(|e| format!("创建窗口失败: {e}"))?;

    let webview = match wry::WebViewBuilder::new()
        .with_url(format!("http://{addr}/"))
        .build(&window)
    {
        Ok(w) => w,
        Err(e) => {
            let msg = format!("创建 WebView 失败 (缺 WebView2 运行时?): {e}");
            fatal_msgbox(&msg);
            return Err(msg);
        }
    };

    // —— 托盘(--no-tray 可关)——
    let tray = if no_tray {
        None
    } else {
        Some(build_tray(&proxy)?)
    };

    // —— 事件循环(主线程;绝不 await/block_on,见模块头)——
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(ue) => match ue {
                UserEvent::ConnectedChanged(now) => {
                    if let Some(tray) = &tray {
                        let (src, dot) = if now {
                            (TRAY_ICON_RGBA, DOT_CONNECTED)
                        } else {
                            (TRAY_ICON_GRAY_RGBA, DOT_IDLE)
                        };
                        let _ = tray.set_icon(Some(tray_icon_any(src, dot)));
                    }
                }
                UserEvent::ShowWindow => {
                    window.set_visible(true);
                    window.set_focus();
                }
                UserEvent::OpenBrowser => {
                    web::open_in_browser(&format!("http://{addr}/"));
                }
                UserEvent::Quit => {
                    // 简化停机:数据层无 COM 释放,服务线程随进程终止(模块头)
                    if let Some(tray) = &tray {
                        let _ = tray.set_visible(false);
                    }
                    *control_flow = ControlFlow::Exit;
                }
            },
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // 关窗 = 退出进程(简化决策,模块头);托盘随进程销毁,先显式隐掉
                if let Some(tray) = &tray {
                    let _ = tray.set_visible(false);
                }
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(_) | WindowEvent::Moved(_),
                ..
            } => {
                // wry 0.57 build(&window) 已随窗口自适应,此处仅保底触发一次重排
                let _ = webview.bounds();
            }
            _ => {}
        }
    });
}

/// 构建托盘:图标(app-32 资产,缺失回退程序画圆点)+ 菜单
/// (显示主窗口 / 在浏览器打开 / 退出)+ 左键单击 = 显示主窗口。
fn build_tray(
    proxy: &tao::event_loop::EventLoopProxy<UserEvent>,
) -> Result<tray_icon::TrayIcon, String> {
    let menu = Menu::new();
    let mi_show = MenuItem::with_id(ID_SHOW, "显示主窗口", true, None);
    let mi_browser = MenuItem::with_id(ID_BROWSER, "在浏览器打开管理台", true, None);
    let mi_quit = MenuItem::with_id(ID_QUIT, "退出", true, None);
    menu.append(&mi_show).map_err(mstr)?;
    menu.append(&mi_browser).map_err(mstr)?;
    menu.append(&PredefinedMenuItem::separator())
        .map_err(mstr)?;
    menu.append(&mi_quit).map_err(mstr)?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false) // 左键单击 = 显示主窗口(右键 = 菜单)
        .with_tooltip("Mini RTT Viewer · 状态见管理台")
        .with_icon(tray_icon_any(TRAY_ICON_GRAY_RGBA, DOT_IDLE))
        .build()
        .map_err(mstr)?;

    // 菜单事件 → 事件循环(muda 全局 handler,回调在主线程触发)
    let p_menu = proxy.clone();
    MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
        let ue = match ev.id.0.as_str() {
            ID_SHOW => UserEvent::ShowWindow,
            ID_BROWSER => UserEvent::OpenBrowser,
            ID_QUIT => UserEvent::Quit,
            _ => return,
        };
        let _ = p_menu.send_event(ue);
    }));
    // 托盘图标事件 → 是否恢复主窗口:仅**左键**的单击/双击(Windows 下
    // WM_RBUTTONDOWN/UP 都会发 Click{button: Right},右键弹菜单的瞬间不得拉起
    // 主窗口抢焦点——SerialHub ADR-21③ 的病根回归)
    let p_click = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
        if tray_event_restores_window(&ev) {
            let _ = p_click.send_event(UserEvent::ShowWindow);
        }
    }));
    Ok(tray)
}

// ---------------- 小工具 ----------------

fn mstr(e: impl std::fmt::Display) -> String {
    format!("托盘/菜单初始化失败: {e}")
}

type Rgba = (u8, u8, u8);
const DOT_CONNECTED: Rgba = (30, 142, 78); // 绿
const DOT_IDLE: Rgba = (124, 135, 145); // 灰

/// 程序内生成的 32×32 圆点图标:实心彩色核心 + 深色描边环 + 抗锯齿外缘,
/// 资产缺失时的回退(与 SerialHub FIX-13 同款)。
fn dot_rgba((r, g, b): Rgba) -> Vec<u8> {
    const OUTLINE: (u8, u8, u8) = (28, 38, 48);
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32u32 {
        for x in 0..32u32 {
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let d = (dx * dx + dy * dy).sqrt();
            let a = ((15.5 - d) * 40.0).clamp(0.0, 255.0) as u8; // 抗锯齿外缘
            let (cr, cg, cb) = if d < 12.0 { (r, g, b) } else { OUTLINE };
            rgba.extend_from_slice(&[cr, cg, cb, a]);
        }
    }
    rgba
}

/// 优先用构建期解码的图标(原始 RGBA);尺寸/长度不符或资产缺失 → 程序画的圆点。
/// 尺寸防呆口径与 SerialHub 一致(嵌入数据坏了不至于喂坏 Icon::from_rgba)。
fn tray_icon_any(src: Option<RgbaIcon>, fallback: Rgba) -> tray_icon::Icon {
    if let Some((bytes, w, h)) = src {
        if bytes.len() == (w as usize) * (h as usize) * 4 {
            if let Ok(icon) = tray_icon::Icon::from_rgba(bytes.to_vec(), w, h) {
                return icon;
            }
        }
    }
    tray_icon::Icon::from_rgba(dot_rgba(fallback), 32, 32).expect("固定 32×32 RGBA 不会失败")
}

fn tao_icon_any(src: Option<RgbaIcon>, fallback: Rgba) -> TaoIcon {
    if let Some((bytes, w, h)) = src {
        if bytes.len() == (w as usize) * (h as usize) * 4 {
            if let Ok(icon) = TaoIcon::from_rgba(bytes.to_vec(), w, h) {
                return icon;
            }
        }
    }
    TaoIcon::from_rgba(dot_rgba(fallback), 32, 32).expect("固定 32×32 RGBA 不会失败")
}

/// FIX-14(ADR-8)同款:GUI 早期失败(端口被占用/初始化失败)用系统 MessageBox
/// 明示 —— 双击启动无控制台,stderr 不可见;错误对话框允许抢占注意力。
#[cfg(windows)]
fn fatal_msgbox(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND,
    };
    fn wide(s: &str) -> Vec<u16> {
        let mut w: Vec<u16> = s.encode_utf16().collect();
        w.push(0);
        w
    }
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(text).as_ptr(),
            wide("Mini RTT Viewer 启动失败").as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(windows))]
fn fatal_msgbox(_text: &str) {}

/// 托盘事件是否应恢复主窗口 —— 仅**左键**的单击/双击(DoubleClick 同属左键恢复
/// 语义)。右键/中键不触碰窗口:右键菜单是 tray-icon 内建弹出
/// (with_menu_on_left_click(false))。抽成纯函数以便单测(SerialHub ADR-21③)。
fn tray_event_restores_window(ev: &TrayIconEvent) -> bool {
    matches!(
        ev,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            ..
        } | TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        }
    )
}

// ---------------- 单测(托盘分键判定,SerialHub ADR-21③ 回归) ----------------

#[cfg(test)]
mod tests {
    use super::*;
    use tray_icon::dpi::PhysicalPosition;
    use tray_icon::{MouseButtonState, Rect, TrayIconId};

    fn click(button: MouseButton, state: MouseButtonState) -> TrayIconEvent {
        TrayIconEvent::Click {
            id: TrayIconId::new("1"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
            button,
            button_state: state,
        }
    }

    fn dblclick(button: MouseButton) -> TrayIconEvent {
        TrayIconEvent::DoubleClick {
            id: TrayIconId::new("1"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
            button,
        }
    }

    #[test]
    fn tray_left_click_restores_window() {
        // 左键按下/抬起都算恢复语义(Windows 两个消息都发 Click)
        assert!(tray_event_restores_window(&click(
            MouseButton::Left,
            MouseButtonState::Down
        )));
        assert!(tray_event_restores_window(&click(
            MouseButton::Left,
            MouseButtonState::Up
        )));
        assert!(tray_event_restores_window(&dblclick(MouseButton::Left)));
    }

    #[test]
    fn tray_right_and_other_keys_never_touch_window() {
        // 右键(按下/抬起)只属于内建菜单,不得拉起主窗口(右键弹菜单抢焦点回归)
        assert!(!tray_event_restores_window(&click(
            MouseButton::Right,
            MouseButtonState::Down
        )));
        assert!(!tray_event_restores_window(&click(
            MouseButton::Right,
            MouseButtonState::Up
        )));
        assert!(!tray_event_restores_window(&dblclick(MouseButton::Right)));
        assert!(!tray_event_restores_window(&click(
            MouseButton::Middle,
            MouseButtonState::Up
        )));
        assert!(!tray_event_restores_window(&TrayIconEvent::Enter {
            id: TrayIconId::new("1"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
        }));
        assert!(!tray_event_restores_window(&TrayIconEvent::Leave {
            id: TrayIconId::new("1"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
        }));
    }
}
