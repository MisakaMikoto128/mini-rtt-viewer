//! WebSocket 事件契约、快照与慢消费者重同步。

use super::*;

/// WS 事件统一序列化契约;变体名与字段名由线格式测试逐项锁定。
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum WsEvent<'a> {
    /// 增量行 {"type":"rows","seq":N,"dropped":N,"rows":[{runs:[{text,fg}]}]}
    Rows {
        seq: u64,
        dropped: usize,
        rows: &'a [RunsRow<'a>],
    },
    /// 重同步全量快照 {"type":"snapshot","clearSeq":N,"rows":[…]}。
    /// clearSeq 让重连后的新服务能重置旧水位，同时保留同服务清空前的过滤语义。
    Snapshot {
        #[serde(rename = "clearSeq")]
        clear_seq: u64,
        rows: &'a [RunsRow<'a>],
    },
    /// 清空水位 {"type":"cleared","seq":N}(水位之后的 rows 才有效)
    Cleared { seq: u64 },
    /// 连接状态 {"type":"state","connected":B,"status":"…"}
    State { connected: bool, status: String },
    /// 设备信息 {"type":"device","info":{8 字段}}——info 与 /api/status 的 device
    /// 同形,继续由 serde_json::json! 构造(与 device_info_json 单点同源)
    Device { info: serde_json::Value },
    /// 电源命令执行结果 {"type":"power","on":B,"ok":B,"message":"…"}
    Power { on: bool, ok: bool, message: String },
    /// 进度文案 {"type":"progress","text":"…"}
    Progress { text: String },
    /// 芯片型号名单 {"type":"names","names":[…]}
    Names { names: Vec<String> },
    /// J-Link 列表 {"type":"jlinks","list":[{sn,name}]}——camelCase 会把变体名
    /// 转成 "jLinks",与历史契约不符 → 显式 rename
    #[serde(rename = "jlinks")]
    JLinks { list: Vec<JLinkEntry> },
    /// 统计 {"type":"stats","rx":N,"tx":N,"rows":N,"sessionSec":N,"cap":N}——
    /// sessionSec=当前连接会话秒(断开为 0);cap=服务端行数上限(MAX_LOG_ROWS,
    /// 底栏「n / 500 行」口径单点:前端不硬编码,以本字段为准)。
    /// 注意:容器 rename_all="camelCase" 只改写**变体名**,不变体字段——首个
    /// 多词字段必须显式 rename(线格式测试已锁),别再假设字段名自动转驼峰
    Stats {
        rx: u64,
        tx: u64,
        rows: usize,
        #[serde(rename = "sessionSec")]
        session_sec: u64,
        cap: usize,
    },
}

pub(crate) fn device_info_json(info: &DeviceInfo) -> serde_json::Value {
    serde_json::json!({
        "firmware": info.firmware, "hardware": info.hardware,
        "serial": info.serial, "core": info.core_name,
        "cpu": info.core_cpu, "target": info.target,
        "iface": info.iface, "speedKhz": info.speed_khz,
    })
}

/// 序列化与广播只保留一个入口，发送端无需重复处理线格式。
pub(crate) fn broadcast_event(shared: &Shared, event: WsEvent<'_>) {
    let payload = serde_json::to_string(&event).unwrap();
    let _ = shared.events_tx.send(payload);
}

pub(crate) async fn ws_handler(
    ws: WebSocketUpgrade,
    State(shared): State<Arc<Shared>>,
) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, shared))
}

/// WS 会话:初次全量快照 + 订阅事件流;Lagged 自动重同步。
/// 重同步限流(审计 P2):慢消费者持续 Lagged 时,若每次都无条件发 500 行
/// 快照,会形成「Lagged → 全量快照 → 又 Lagged」的重同步风暴(持泵锁全量
/// 克隆+序列化,反噬数据泵节奏)。短窗内重同步过频即断开,由客户端重连取
/// 新快照——重连粒度的循环替代每条消息粒度的风暴。
pub(crate) async fn ws_loop(mut socket: WebSocket, shared: Arc<Shared>) {
    let mut rx = shared.events_tx.subscribe();
    {
        let snapshot = shared.pump.lock().unwrap().snapshot_rows();
        let msg = serde_json::to_string(&WsEvent::Snapshot {
            clear_seq: shared.clear_seq.load(Ordering::Relaxed),
            rows: &rows_view(&snapshot),
        })
        .unwrap();
        if socket.send(Message::Text(msg.into())).await.is_err() {
            return;
        }
    }
    // Lagged 重同步节流:1 秒内超过 3 次视为追不平,断开让客户端重连
    let mut resync_window = Instant::now();
    let mut resync_burst = 0u32;
    loop {
        tokio::select! {
            res = rx.recv() => {
                match res {
                    Ok(payload) => {
                        if socket.send(Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if resync_window.elapsed() >= Duration::from_secs(1) {
                            resync_window = Instant::now();
                            resync_burst = 0;
                        }
                        resync_burst += 1;
                        if resync_burst > 3 {
                            break;
                        }
                        // 订阅滞后:丢弃的行不可恢复,重发全量快照让前端重同步
                        let snapshot = shared.pump.lock().unwrap().snapshot_rows();
                        let msg = serde_json::to_string(&WsEvent::Snapshot {
                            clear_seq: shared.clear_seq.load(Ordering::Relaxed),
                            rows: &rows_view(&snapshot),
                        })
                        .unwrap();
                        if socket.send(Message::Text(msg.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                if socket.send(Message::Ping("".into())).await.is_err() {
                    break;
                }
            }
            else => break,
        }
    }
}
