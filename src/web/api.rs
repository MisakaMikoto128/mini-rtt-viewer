//! HTTP 请求类型、API handlers 与本地时间格式化。

use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SendReq {
    pub(crate) text: String,
    /// HEX 发送模式:输入按十六进制字节解析(规则见下方 `parse_hex_bytes`)
    #[serde(default)]
    pub(crate) hex: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PauseReq {
    pub(crate) on: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectReq {
    pub(crate) chip: String,
    #[serde(default)]
    pub(crate) iface_index: usize,
    #[serde(default)]
    pub(crate) speed_index: usize,
    #[serde(default)]
    pub(crate) channel: u32,
    /// 「J-Link」下拉选中的序列号(前端 1273 行起接线;缺省/不在当前列表内
    /// 一律回落列表首台,与旧行为兼容)
    #[serde(default)]
    pub(crate) jlink_sn: Option<u32>,
}

/// 复位模式:in-place = 现有 Reset 命令(worker 复位目标并重挂 RTT 续收);
/// reconnect = 先走断开逻辑,1 秒后用上次连接参数重新 spawn worker
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResetMode {
    InPlace,
    Reconnect,
}

/// 解析复位模式:缺省/未知值一律回落 in-place(容忍旧前端空请求体与拼写
/// 失误,复位不至于整体失效)
pub(crate) fn parse_reset_mode(mode: Option<&str>) -> ResetMode {
    match mode.map(str::trim) {
        Some(m) if m.eq_ignore_ascii_case(RESET_MODE_RECONNECT) => ResetMode::Reconnect,
        Some(m) if m.eq_ignore_ascii_case(RESET_MODE_IN_PLACE) => ResetMode::InPlace,
        _ => ResetMode::InPlace,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PowerReq {
    pub(crate) on: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsReq {
    #[serde(default)]
    pub(crate) rx_ending: Option<i32>,
    #[serde(default)]
    pub(crate) frame_timeout: Option<u32>,
    #[serde(default)]
    pub(crate) encoding_index: Option<i32>,
    #[serde(default)]
    pub(crate) hex_rx: Option<bool>,
    #[serde(default)]
    pub(crate) auto_frame: Option<bool>,
    #[serde(default)]
    pub(crate) search_regex: Option<bool>,
    /// 发送行尾 0=CRLF 1=LF 2=CR 3=无(UI 习惯记忆)
    #[serde(default)]
    pub(crate) send_ending: Option<i32>,
    /// HEX 发送模式
    #[serde(default)]
    pub(crate) hex_send: Option<bool>,
    // ---- 连接设置组(F1:web→服务端保存链路;此前这五项只写 localStorage,
    // reload 被 /api/prefs 服务端值回填覆盖,改动全部丢失)----
    /// 目标设备名(存原始输入;连接时才做设备库匹配)
    #[serde(default)]
    pub(crate) chip: Option<String>,
    /// 接口 0=SWD 1=JTAG
    #[serde(default)]
    pub(crate) iface_index: Option<i32>,
    /// 速度下拉索引
    #[serde(default)]
    pub(crate) speed_index: Option<i32>,
    /// RTT 通道 0-15
    #[serde(default)]
    pub(crate) channel: Option<u32>,
    /// 数据采集后端 id(阶段二;None = 不切换。校验与连接态门禁在
    /// api_settings:未知/不可用 id → 400,连接态 → 409)
    #[serde(default)]
    pub(crate) backend: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TimerReq {
    pub(crate) on: bool,
    #[serde(default)]
    pub(crate) interval_sec: Option<f64>,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) hex: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MarkReq {
    #[serde(default)]
    pub(crate) text: String,
}

/// 用系统默认浏览器打开 URL(Windows;cmd start 的首个引号参数是窗口标题,
/// 补空串占位)。失败静默:用户可手动访问地址栏。
pub(crate) fn open_in_browser(url: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .spawn();
}

/// 本机出网网卡 IPv4:UDP connect 探测默认路由源地址(UDP 假连接不实际发包,
/// 只让 OS 选路)。无网络/特殊多网卡环境可能拿不到,如实返回 None。
pub(crate) fn lan_ipv4() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind(std::net::SocketAddr::from(([0, 0, 0, 0], 0))).ok()?;
    s.connect("8.8.8.8:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (ip.is_ipv4() && !ip.is_loopback()).then_some(ip)
}

/// /api/addr(FR-28 远程访问框数据):本机/局域网地址是否开放 + 局域网 IP。
/// open = 监听地址非回环(0.0.0.0 或具体网卡)时其他设备才可达;
/// 回环监听时 lanIp 仍返回(端口开放后即用),前端只决定展示与否。
#[derive(serde::Serialize)]
pub(crate) struct AddrInfo {
    port: u16,
    open: bool,
    #[serde(rename = "lanIp")]
    lan_ip: Option<String>,
}
pub(crate) async fn api_addr(State(shared): State<Arc<Shared>>) -> Json<AddrInfo> {
    // lan_ipv4 的 UDP connect 是同步阻塞调用(审计 P2):网络栈异常/杀软拦截
    // 时可达数百 ms,放 spawn_blocking 避免冻结 tokio worker 线程上的并发请求
    let lan_ip = tokio::task::spawn_blocking(lan_ipv4)
        .await
        .ok()
        .flatten()
        .map(|i| i.to_string());
    Json(AddrInfo {
        port: shared.port,
        open: !shared.listen.is_loopback(),
        lan_ip,
    })
}

/// 在系统默认浏览器打开管理台(壳内前端按钮用;SerialHub Sprint7 实测壳内
/// window.open 被原生层吞掉,浏览器访问用不到此端点)。可选 JSON body
/// `{"url": …}` 供关于框打开 GitHub 外链,仅放行本仓库前缀、其余一律回落
/// 管理台本页——保持「不接受任意 URL」的打开面约束。
pub(crate) async fn api_open_browser(
    State(shared): State<Arc<Shared>>,
    body: String,
) -> StatusCode {
    const REPO_PREFIX: &str = "https://github.com/MisakaMikoto128/";
    let url = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(str::to_owned))
        .filter(|u| u.starts_with(REPO_PREFIX))
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", shared.port));
    open_in_browser(&url);
    StatusCode::OK
}

pub(crate) fn embedded_asset(content: &'static str, mime: &'static str) -> Response {
    ([(header::CONTENT_TYPE, mime)], content).into_response()
}

pub(crate) async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub(crate) async fn favicon() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/png")],
        FAVICON,
    )
        .into_response()
}

pub(crate) async fn api_status(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let connected = shared.connected.load(Ordering::Relaxed);
    let (rows_total, paused) = {
        let pump = shared.pump.lock().unwrap();
        (pump.rows_len(), pump.paused)
    };
    // device 8 字段(与 WS device 事件同形):由后端连接成功时发 DeviceInfo
    // 消息供数,断开统一清空(State(false) 分支),与统计清零同口径
    let device = shared
        .device_info
        .lock()
        .unwrap()
        .as_ref()
        .map(device_info_json);
    let session_sec = shared
        .session_start
        .lock()
        .unwrap()
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);
    Json(serde_json::json!({
        "connected": connected,
        "rxBytes": shared.rx_bytes.load(Ordering::Relaxed),
        "txBytes": shared.tx_bytes.load(Ordering::Relaxed),
        "rowsTotal": rows_total,
        "paused": paused,
        "uptimeSec": shared.started.elapsed().as_secs(),
        "sessionSec": session_sec,
        "sessionStatus": shared.status_text.lock().unwrap().clone(),
        "device": device,
        "version": env!("CARGO_PKG_VERSION"),
        "backend": shared.backend().id(),
    }))
}

/// 当前偏好快照(面板初值恢复;主题/字号在前端 localStorage,不含在内)
pub(crate) async fn api_prefs(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "chip": shared.chip_name.lock().unwrap().clone(),
        "ifaceIndex": *shared.iface_index.lock().unwrap(),
        "speedIndex": *shared.speed_index.lock().unwrap(),
        "channel": *shared.channel.lock().unwrap(),
        "rxEnding": *shared.rx_ending.lock().unwrap(),
        "frameTimeout": shared.frame_timeout_ms.load(Ordering::Relaxed),
        "encodingIndex": shared.encoding_index.load(Ordering::Relaxed) as i32,
        "hexRx": shared.hex_rx.load(Ordering::Relaxed),
        "autoFrame": shared.auto_frame.load(Ordering::Relaxed),
        "searchRegex": shared.search_regex.load(Ordering::Relaxed),
        "timerOn": shared.timer_on.load(Ordering::Relaxed),
        "timerIntervalSec": *shared.timer_interval.lock().unwrap(),
        "sendEnding": *shared.send_ending.lock().unwrap(),
        "hexSend": shared.hex_send.load(Ordering::Relaxed),
        "jlinkSerial": *shared.jlink_serial.lock().unwrap(),
        "backend": shared.backend().id(),
    }))
}

/// /api/backends(阶段二后端可选面):当前选定 + 本进程可用后端列表。
/// items 恒非空(普通形态 jlink+demo;--demo-log 强制形态仅 demo)。
/// 文案来自后端实现的 &'static 字面量,前端仍按注入面纪律用 DOM API 渲染
pub(crate) async fn api_backends(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let items: Vec<serde_json::Value> = shared
        .backends_available
        .iter()
        .map(|b| {
            serde_json::json!({
                "id": b.id(),
                "name": b.display_name(),
                "description": b.description(),
                "isVirtual": b.is_virtual(),
            })
        })
        .collect();
    Json(serde_json::json!({
        "selected": shared.backend().id(),
        "items": items,
    }))
}

/// 内置主题(前端 index.html 内嵌同款主题表,前端按 id 覆盖,不落盘)+
/// 自定义主题(exe 旁 themes/*.css,每次现扫:拖入即生效,零注册)
pub(crate) async fn api_themes() -> Json<serde_json::Value> {
    let mut items = serde_json::json!([
        {"id": "dark", "name": "深色", "custom": false},
        {"id": "light", "name": "浅色", "custom": false},
        {"id": "oled", "name": "OLED 纯黑", "custom": false},
        {"id": "sepia", "name": "护眼暖色", "custom": false},
    ]);
    let arr = items.as_array_mut().unwrap();
    for t in scan_custom_themes(&themes_dir()) {
        // 与内置同名的 .css 不进列表(前端内置表覆盖,列出来只会出现两个相同 value)
        if BUILTIN_THEME_IDS.contains(&t.id.as_str()) {
            continue;
        }
        arr.push(serde_json::json!({"id": t.id, "name": t.name, "custom": true}));
    }
    Json(items)
}

/// 自定义主题 CSS 文本(text/css);内置主题与未知 id 一律 404(前端内置表
/// 覆盖)。路径安全:只在「本机扫描列表」里按 id 精确匹配,拿到的路径来自
/// read_dir 而非用户输入,天然无目录穿越面。
pub(crate) async fn api_theme_css(Path(id): Path<String>) -> Response {
    let hit = scan_custom_themes(&themes_dir())
        .into_iter()
        .find(|t| t.id == id);
    match hit {
        Some(t) => match std::fs::read(&t.path) {
            Ok(bytes) => {
                ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], bytes).into_response()
            }
            // 扫描后文件被删等竞态:按不存在处理
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// 发送历史(最新在前,上限 50;前端 ↑↓ 浏览的数据源)
pub(crate) async fn api_history(State(shared): State<Arc<Shared>>) -> Json<Vec<String>> {
    Json(shared.send_history.lock().unwrap().clone())
}

pub(crate) async fn api_devices(State(shared): State<Arc<Shared>>) -> Json<Vec<String>> {
    // 设备候选统一从 shared.device_names 供数:jlink 来自后台枚举回传,
    // demo 在装配期已由 initial_devices 种子(演示设备库)
    Json(shared.device_names.lock().unwrap().clone())
}

pub(crate) async fn api_jlinks(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let list = shared.jlinks.lock().unwrap().clone();
    Json(serde_json::json!(list
        .iter()
        .map(|(sn, name)| serde_json::json!({"sn": sn, "name": name}))
        .collect::<Vec<_>>()))
}

/// 连接:连接前门禁(后端 can_connect,Err→409)→ 芯片名补全(库内子串
/// 匹配首个全称,与桌面版同规则)→ 连接参数写入偏好快照源 → 后端 connect
/// (jlink spawn worker;demo 命令进单例线程模拟连接,不加载 DLL)
pub(crate) async fn api_connect(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<ConnectReq>,
) -> Response {
    let backend = shared.backend();
    // reset reconnect 的 1 秒等待窗口内 session 已被 take 清空,存活检查
    // 会放行——重连门闩在此堵住双 worker 并发(审计 P1;demo 后端不会
    // 置位此门闩,通用检查对其恒放行,与现状一致)
    if shared.reconnecting.load(Ordering::Relaxed) {
        return (StatusCode::CONFLICT, "正在自动重连,请稍候").into_response();
    }
    // 后端门禁(存活检查「已有连接」/ BUSY 门闩「设备库枚举中,请稍候几秒
    // 再连接」;demo 恒 Ok)——文案由后端提供,与现状逐字一致
    if let Err(msg) = backend.can_connect(&*shared) {
        return (StatusCode::CONFLICT, msg).into_response();
    }
    let chip_raw = req.chip.trim().to_string();
    if chip_raw.is_empty() {
        return (StatusCode::BAD_REQUEST, "请先填写目标芯片型号").into_response();
    }
    // 校验名单与前端看到的候选同源:统一取 shared.device_names(demo 的
    // 演示设备库已在装配期种子进同一槽)
    let full: Vec<String> = shared.device_names.lock().unwrap().clone();
    let exact = full.iter().any(|n| n.eq_ignore_ascii_case(&chip_raw));
    let chip = if exact {
        chip_raw
    } else {
        let needle = chip_raw.to_uppercase();
        match full.iter().find(|s| s.to_uppercase().contains(&needle)) {
            Some(c) => c.clone(),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("设备库无匹配型号:{chip_raw}"),
                )
                    .into_response()
            }
        }
    };
    drop(full);

    // 连接参数夹取一次,偏好快照源 / 上次连接参数 / worker 配置三者同源
    let iface_index = req.iface_index.min(1);
    let speed_index = req.speed_index.min(SPEEDS_KHZ.len() - 1);
    let channel = req.channel.min(15);
    // J-Link 选定:请求里的 sn 必须在当前列表内才生效(下拉可能是过期列表,
    // 设备拔出后回落自动选首台,避免 connect 阶段「找不到序列号」直接失败)
    let jlink_sn = req
        .jlink_sn
        .filter(|sn| shared.jlinks.lock().unwrap().iter().any(|(s, _)| s == sn));

    // 连接参数进偏好快照源(tick 内 500ms 落盘;重启后面板初值恢复)
    *shared.chip_name.lock().unwrap() = chip.clone();
    *shared.iface_index.lock().unwrap() = iface_index as i32;
    *shared.speed_index.lock().unwrap() = speed_index as i32;
    *shared.channel.lock().unwrap() = channel;
    *shared.jlink_serial.lock().unwrap() = jlink_sn;
    // 上次连接参数:reset reconnect 模式的重连来源(demo 也记录,语义一致)
    let params = ConnectParams {
        chip: chip.clone(),
        iface_index,
        speed_index,
        channel,
        jlink_sn,
    };
    *shared.last_connect.lock().unwrap() = Some(params.clone());

    if let Err(msg) = backend.connect(&*shared, &params) {
        return (StatusCode::CONFLICT, msg).into_response();
    }
    StatusCode::OK.into_response()
}

/// 断开:走当前后端(jlink = 置停止标志 + 空发送唤醒,Exited 消息回到
/// 未连接态;demo = 内部 Disconnect 命令,手动断开后不自动重连,由 demo
/// 状态机保证)
pub(crate) async fn api_disconnect(State(shared): State<Arc<Shared>>) -> StatusCode {
    shared.backend().disconnect(&*shared);
    StatusCode::OK
}

pub(crate) async fn api_power(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<PowerReq>,
) -> Response {
    // demo 与真机同语义:未连接拒绝(前端此时也禁用勾选框;409 文案由
    // 后端提供,与现状逐字一致)
    match shared.backend().power(&*shared, req.on) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(msg) => (StatusCode::CONFLICT, msg).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResetReq {
    /// 复位模式:"in-place"(缺省)| "reconnect";缺省/未知值一律 in-place
    #[serde(default)]
    pub(crate) mode: Option<String>,
}

/// 复位两模式:in-place = 现有 Reset 命令(worker 复位目标并重挂 RTT 续收);
/// reconnect = 先走断开逻辑,1 秒后用上次连接参数重新 spawn worker(重启服务
/// 且无上次参数 → 409)。mode 解析、last_connect 检查、reconnecting 门闩置位
/// 留在 web 层;具体执行走当前后端——reset_reconnect 不被支持时(demo,
/// caps().reset_reconnect == false)一律回落 reset_in_place,与现状「demo
/// 忽略 mode」逐字等价。
pub(crate) async fn api_reset(State(shared): State<Arc<Shared>>, body: Bytes) -> Response {
    // 兼容旧前端:POST 无请求体 = 缺省 in-place;非空则必须是合法 JSON
    let req: ResetReq = if body.is_empty() {
        ResetReq { mode: None }
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, format!("请求体 JSON 解析失败:{e}"))
                    .into_response()
            }
        }
    };
    let backend = shared.backend();
    // 不支持重连式复位的后端(demo):忽略 mode,一律 in-place 模拟
    // (未连接 409,文案由后端提供与现状逐字一致)
    if !backend.caps().reset_reconnect {
        return match backend.reset_in_place(&*shared) {
            Ok(()) => StatusCode::OK.into_response(),
            Err(msg) => (StatusCode::CONFLICT, msg).into_response(),
        };
    }
    match parse_reset_mode(req.mode.as_deref()) {
        ResetMode::InPlace => match backend.reset_in_place(&*shared) {
            Ok(()) => StatusCode::OK.into_response(),
            Err(msg) => (StatusCode::CONFLICT, msg).into_response(),
        },
        ResetMode::Reconnect => {
            // 已在重连等待期:拒绝重复触发(避免多个重连线程叠跑)
            if shared.reconnecting.load(Ordering::Relaxed) {
                return (StatusCode::CONFLICT, "正在自动重连,请稍候").into_response();
            }
            let Some(params) = shared.last_connect.lock().unwrap().clone() else {
                return (StatusCode::CONFLICT, "无上次连接参数,无法重连").into_response();
            };
            // 重连等待期门闩(审计 P1):调用 reset_reconnect 之前置位(设计
            // 契约 §3.2,比历史 take 后置位更保守,两行间无 await,可观测
            // 等价)——此刻起 api_connect 以 409 拒绝,直到重连线程收尾清除
            shared.reconnecting.store(true, Ordering::Relaxed);
            match backend.reset_reconnect(shared.clone(), &params) {
                Ok(()) => StatusCode::OK.into_response(),
                Err(msg) => {
                    // 后端未受理(理论不可达):门闩回滚,避免永久堵死连接
                    shared.reconnecting.store(false, Ordering::Relaxed);
                    (StatusCode::CONFLICT, msg).into_response()
                }
            }
        }
    }
}

/// 运行时参数:逐字段可选,worker 共享原子热切换(与桌面版同语义)。
/// 负数一律 400(审计 P2:此前 i32 负值被 clamp(0,N) 静默夹成 0,调用方
/// 得不到任何非法输入反馈;无符号字段负数由 serde 反序列化直接 400)
pub(crate) async fn api_settings(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<SettingsReq>,
) -> Response {
    // 先校验完整请求，再应用任何字段：400 响应不能伴随部分设置已生效。
    for (value, name) in [
        (req.rx_ending, "rxEnding"),
        (req.encoding_index, "encodingIndex"),
        (req.send_ending, "sendEnding"),
        (req.iface_index, "ifaceIndex"),
        (req.speed_index, "speedIndex"),
    ] {
        if value.is_some_and(|v| v < 0) {
            return (StatusCode::BAD_REQUEST, format!("{name} 不能为负数")).into_response();
        }
    }
    if let Some(v) = req.rx_ending {
        *shared.rx_ending.lock().unwrap() = v.clamp(0, 4);
    }
    if let Some(v) = req.frame_timeout {
        shared
            .frame_timeout_ms
            .store(v.clamp(1, 200), Ordering::Relaxed);
    }
    if let Some(v) = req.encoding_index {
        shared
            .encoding_index
            .store(v.clamp(0, 4) as u32, Ordering::Relaxed);
    }
    if let Some(v) = req.hex_rx {
        shared.hex_rx.store(v, Ordering::Relaxed);
    }
    if let Some(v) = req.auto_frame {
        shared.auto_frame.store(v, Ordering::Relaxed);
    }
    if let Some(v) = req.search_regex {
        shared.search_regex.store(v, Ordering::Relaxed);
    }
    if let Some(v) = req.send_ending {
        *shared.send_ending.lock().unwrap() = v.clamp(0, 3);
    }
    if let Some(v) = req.hex_send {
        shared.hex_send.store(v, Ordering::Relaxed);
    }
    // 连接设置组(F1):进偏好快照源,与其它设置同一条「tick 500ms 快照比对
    // 落盘 → 重启 /api/prefs 回填」链路;夹取口径与 /api/connect 一致
    if let Some(v) = req.chip {
        *shared.chip_name.lock().unwrap() = v.trim().to_string();
    }
    if let Some(v) = req.iface_index {
        *shared.iface_index.lock().unwrap() = v.clamp(0, 1);
    }
    if let Some(v) = req.speed_index {
        *shared.speed_index.lock().unwrap() = v.clamp(0, SPEEDS_KHZ.len() as i32 - 1);
    }
    if let Some(v) = req.channel {
        *shared.channel.lock().unwrap() = v.clamp(0, 15);
    }
    // 后端切换(阶段二):未知/不可用 id → 400;连接态(判据 shared.connected,
    // 不是 session 槽——demo 的会话槽恒活)→ 409「断开后才能切换后端」;
    // 成功 = 换 Shared.backend 引用(写偏好走 snapshot_prefs 快照链路)+
    // 旧后端 on_deselected + 新后端 on_selected(切走 demo 发内部 Disconnect
    // 清连接态;切回 jlink 清候选 + 枚举回填)。同 id 幂等 no-op:不触发回调,
    // 防止 demo 的 on_deselected 手动断开永久挂起 20s/3s 自动周期
    if let Some(id) = req.backend.as_deref() {
        let Some(target) = shared
            .backends_available
            .iter()
            .copied()
            .find(|b| b.id() == id)
        else {
            return (StatusCode::BAD_REQUEST, format!("未知或不可用的后端:{id}")).into_response();
        };
        if shared.connected.load(Ordering::Relaxed) {
            return (StatusCode::CONFLICT, "断开后才能切换后端").into_response();
        }
        let old = shared.backend();
        if old.id() != target.id() {
            old.on_deselected(&*shared);
            *shared.backend.lock().unwrap() = target;
            target.on_selected(&*shared);
        }
    }
    StatusCode::OK.into_response()
}

pub(crate) async fn api_send(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<SendReq>,
) -> Response {
    if req.text.is_empty() {
        return (StatusCode::BAD_REQUEST, "空输入").into_response();
    }
    // 空输入已在上面拦掉:此处 Err 只可能是 HEX 解析失败
    if let Err(e) = shared.send_payload(&req.text, req.hex) {
        return (StatusCode::BAD_REQUEST, format!("HEX 格式错误:{e}")).into_response();
    }
    StatusCode::OK.into_response()
}

pub(crate) async fn api_timer(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<TimerReq>,
) -> Response {
    if let Some(v) = req.interval_sec {
        if !TIMER_INTERVAL_RANGE.contains(&v) {
            return (StatusCode::BAD_REQUEST, "定时间隔需在 0.001-999 秒").into_response();
        }
        *shared.timer_interval.lock().unwrap() = v;
    }
    if let Some(text) = req.text {
        *shared.timer_text.lock().unwrap() = text;
    }
    if let Some(hex) = req.hex {
        shared.timer_hex.store(hex, Ordering::Relaxed);
    }
    shared.timer_on.store(req.on, Ordering::Relaxed);
    StatusCode::OK.into_response()
}

// 本地时间(Win32 GetLocalTime,零依赖):标记行时间戳与导出文件名用。
// std 不提供本地时区时间,单独为此引 chrono/time 不值当。
#[repr(C)]
pub(crate) struct WinSystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    millis: u16,
}
#[link(name = "kernel32")]
extern "system" {
    fn GetLocalTime(out: *mut WinSystemTime);
}

pub(crate) fn local_time() -> WinSystemTime {
    let mut st = WinSystemTime {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        millis: 0,
    };
    unsafe { GetLocalTime(&mut st) };
    st
}

/// "HH:MM:SS"(标记行内嵌)
pub(crate) fn hms_stamp() -> String {
    let t = local_time();
    format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second)
}

/// 会话标记行(手动 /api/mark 与自动连接/断开标记共用款式):
/// 「── [HH:MM:SS] {label} ──」
pub(crate) fn marker_line(label: &str, stamp: &str) -> String {
    format!("── [{stamp}] {label} ──")
}

/// State 迁移的自动标记行:已连接 / 已断开
pub(crate) fn state_marker_line(connected: bool) -> String {
    marker_line(if connected { "已连接" } else { "已断开" }, &hms_stamp())
}

/// worker 状态文案 → /api/status 展示文案:去横幅圆点前缀
/// ("● 已连接 (demo)" → "已连接 (demo)")。循环剥离:双 bullet/前导空格
/// 等变体也剥干净(审计 P3)
pub(crate) fn clean_status_text(s: &str) -> String {
    let mut out = s.trim();
    while let Some(stripped) = out.strip_prefix('●') {
        out = stripped.trim();
    }
    out.to_string()
}

/// "YYYYMMDD_HHMMSS"(导出文件名)
pub(crate) fn now_stamp() -> String {
    let t = local_time();
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

pub(crate) async fn api_mark(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<MarkReq>,
) -> StatusCode {
    const MARK_LABEL_CAP: usize = 200;
    let label = if req.text.is_empty() {
        "标记".to_string()
    } else {
        // 512 单行兜底只作用于设备流(push_colored_line 不截),HTTP 直发的
        // 标记在入口限长,防超长行经 WS/导出放大(审计 P3)
        req.text.chars().take(MARK_LABEL_CAP).collect()
    };
    let mut pump = shared.pump.lock().unwrap();
    pump.push_colored_line(&marker_line(&label, &hms_stamp()), MARK_COLOR);
    StatusCode::OK
}

pub(crate) async fn api_pause(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<PauseReq>,
) -> StatusCode {
    shared.pump.lock().unwrap().paused = req.on;
    StatusCode::OK
}

pub(crate) async fn api_clear(State(shared): State<Arc<Shared>>) -> StatusCode {
    shared.pump.lock().unwrap().clear();
    // 水位之后的 rows 才有效;清空前的滞留行由前端按 seq 丢弃
    let seq = shared.seq.fetch_add(1, Ordering::Relaxed) + 1;
    shared.clear_seq.store(seq, Ordering::Relaxed);
    broadcast_event(&shared, WsEvent::Cleared { seq });
    StatusCode::OK
}

/// 导出当前显示的全部日志(.log 纯文本附件:行文本 + \r\n 行尾,时间戳取
/// 本地时间)
pub(crate) async fn api_export(State(shared): State<Arc<Shared>>) -> Response {
    // 锁内只做快照克隆,拼接放到锁外(审计 P3:缩短泵锁持有时间)
    let rows = shared.pump.lock().unwrap().snapshot_rows();
    let mut body = String::new();
    for row in &rows {
        for run in row {
            body.push_str(&run.text);
        }
        body.push_str("\r\n");
    }
    let disposition = format!("attachment; filename=\"rtt_{}.log\"", now_stamp());
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response()
}
