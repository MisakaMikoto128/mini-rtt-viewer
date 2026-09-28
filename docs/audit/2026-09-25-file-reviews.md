# T2 逐文件深审(2026-09-25)

评级口径与引用规范同 `2026-09-25-architecture.md`。每个文件一节;无问题的维度也写"已检查,未见异常"。
S5 抽查规则:每份报告至少 2 条引用由主会话逐字核对,对不上删条记"坏条"。

---

## src/web.rs(1995 行,FR-28 后当前版)

# T2 深审 web.rs(上):1-1015

审计范围:`src/web.rs` 第 1-1015 行(模块头注释、WebOptions/ServiceEvent/OnEvent、run/spawn_gui_service、
Shared 结构与 send_payload/history_push、主题扫描、纯函数 parse_hex_bytes/append_send_ending/
text_to_hex_upper、请求结构体、SavedConnect/ResetMode、serve() 全函数、open_in_browser、
lan_ipv4/api_addr/api_open_browser、WsEvent/rows_view、tick_loop 全函数)。
下半段(1016 行起的 API handlers/ws_loop)仅作交叉引用,不展开。

环境事实(已核实):edition 2021(无默认 `#[warn(rustc::checked_float_literal_expressions)]`);
release `panic = "abort"`(Cargo.toml:46);axum 0.8.9 + axum-core 0.5.6(Cargo.lock);
`SPEEDS_KHZ: [u32; 8]`(src/rtt.rs:68,非空,所有 `SPEEDS_KHZ.len() - 1` 无下溢风险)。

### 发现清单(上:1-1015)

#### [P1] WsEvent::State 的 status 字段未清洗,"● "前缀泄漏进 WS 线契约(前端按"已清洗"消费)

文件:web.rs:935-938

```rust
                    // 状态文案每条 State 都刷新(连接失败的错误详情也随 false 态带出)
                    *shared.status_text.lock().unwrap() = clean_status_text(&status);
                    let _ = shared.events_tx.send(
                        serde_json::to_string(&WsEvent::State { connected, status }).unwrap(),
```

分析:第 935 行清洗后存入 `status_text`(供 `/api/status` 的 `sessionStatus` 用),但第 937 行广播
`WsEvent::State` 时用的是**原始 `status`**,前端的 `setState` 期望"去横幅圆点前缀"的文案——
模块头第 245 行注释也写明 `status_text` 的语义是"去横幅圆点前缀"。`WsEvent::State` 的
doc(810 行)同样未声明带前缀。前端 `ui/web/index.html:607` 为 `m.status.replace('● ', '')`,
只剥离**固定前缀** `● `(bullet+空格):worker 发的 `● 错误: …`(src/rtt.rs:156)能剥掉,
但 `● 正在加载 JLink_x64.dll…` 这类无空格的串(rtt.rs:191 同款式)剥不掉,状态栏会带出
`●` 前缀显示。对比同文件 903 行 Progress 路径:存 `status_text` 清洗、广播原文,前端
`index.html:561-563` 对 progress 的 `m.text.replace('● ', '')` 是**有意**的契约(前端自己剥)。
State 路径没有对应的"前端有意剥"的注释,且 `setState` 只剥 `● ` 一种写法,两条路径的前端
处理不对称。
触发条件:任意 State 消息的 status 不以 `● ` 开头(如 `● 连接中…(第 1 次重试)`,rtt.rs:249)时,
前端状态栏显示带 `●` 的原始文案。
判定:WS 契约(模块头 44-45 行与 tests/web 黑盒)到底要不要前缀,不确定,需人工确认;
若契约是清洗后的,则 937 行应为 `clean_status_text(&status)`(或先存后发),属线格式 bug;
若契约是原文,则 607 行的 `replace('● ', '')` 与 810 行注释是错误来源。

#### [P1] api_open_browser 的 String 提取器受 axum 默认 2MB 体限,且超限请求体被整体拒收

文件:web.rs:780-789

```rust
async fn api_open_browser(State(shared): State<Arc<Shared>>, body: String) -> StatusCode {
    const REPO_PREFIX: &str = "https://github.com/MisakaMikoto128/";
    let url = serde_json::from_str::<serde_json::Value>(&body)
```

分析:axum-core 0.5.6 中 `String` 经 `Bytes::from_request` → `into_limited_body()` 走
**默认体上限 2MB**——`axum-core-0.5.6/src/ext_traits/request.rs:317-335`:
`const DEFAULT_LIMIT: usize = 2_097_152; // 2 mb`。后果:
(a) 任何 >2MB 请求体(即使本端点语义上可容忍任意垃圾)直接返回 413,与路由注释"可选 JSON body"
的"可选"语义无碍,但对调用方是隐性上限;
(b) 本端点无 `DefaultBodyLimit` 层,`serde_json::from_str` 解析 2MB 垃圾 JSON 无上限保护
(仅本机/局域网可达,且 `body` 只被 `starts_with(REPO_PREFIX)` 过滤,不执行,风险低);
(c) **实际前端调用不带 body**:`ui/web/index.html:1287-1290` 的"浏览器打开"按钮直接
`POST /api/open-browser` 无 body,`body: String` 收到空串,走 `unwrap_or_else` 回落
`http://127.0.0.1:{port}`——行为正确,但 776-779 行注释声称"可选 JSON body `{\"url\": …}`
供关于框打开 GitHub 外链"的调用方在本仓库前端**不存在**(grep 前端无 github.com 前缀
的 open-browser 调用),该 body 通道目前无真实使用方,属死接口。
触发条件:外部自动化 POST >2MB body 得 413;前端行为不受影响。

#### [P1] spawn_gui_service 就绪后 ready_tx 仍存活:服务后失败会往已 drop 的 Receiver 发,且 gui 壳超时后服务线程继续占端口

文件:web.rs:162-191 与 gui.rs:80-91

```rust
        if let Err(e) = rt.block_on(serve(opts, Some(cb))) {
            // 就绪后失败 ready 端
            // 已被 gui 丢弃,发送失败无妨
            let _ = ready_tx.send(Err(format!("{e:#}")));
        }
```

```rust
    let addr = match ready_rx.recv_timeout(READY_TIMEOUT) {   // gui.rs:83,READY_TIMEOUT=15s
        Ok(Ok(addr)) => addr,
        ...
        Err(_) => { /* 15s 未就绪 → fatal_msgbox + return Err */ }
```

分析:`ready_tx` 在闭包 `cb` 里被 clone(web.rs:177),即使 gui 壳在 15s 超时后放弃
`ready_rx`(Receiver drop),服务线程里 `serve` 若晚于 15s 才失败(如 bind 慢、axum serve 异常),
`let _ = ready_tx.send(...)` 静默丢失——这是注释预期的("发送失败无妨"),无 bug;
真正的观察点是**反向**的:gui 壳超时走 `return Err(e)` 时,后台 `rtt-service` 线程
**不被停止**(web.rs:189 注释"进程退出时随进程终止")。若调用方(桌面壳)在 15s 超时后
**没有**立即终止进程而是继续运行(例如将来 gui 改成可重试/可后台),该服务线程仍持有
tokio runtime 与(可能已成功的)端口绑定——下次启动会因 AddrInUse(web.rs:718-720)
被误判"已有实例"。当前 `run_gui` 超时后 `return Err`,main 随即退出进程,实际无害;
登记为设计脆弱点:ready 通道是**单向一次性**语义,服务线程生命周期与壳解耦,
无 cancel 句柄。
触发条件:bind 耗时 >15s(极端慢盘/杀软拦截)且壳在超时后未立即退出进程。
不确定,需人工确认 main 是否保证超时后必然进程退出(本审计未审 main.rs 全路径)。

#### [P2] Progress 消息改写 status_text,与 State 文案同槽,2s 轮询的 sessionStatus 可能与 WS state 的 status 打架

文件:web.rs:900-907

```rust
                Ok(WorkerMsg::Progress(text)) => {
                    // 进度文案同步进 status_text(F3):2s 轮询的 sessionStatus
                    // 与 WS progress 同源,轮询回填不会倒退/打架
                    *shared.status_text.lock().unwrap() = clean_status_text(&text);
```

分析:`status_text` 同时被 Progress(903)与 State(935)写入,`/api/status` 的
`sessionStatus` 读它(web.rs:1118 附近,下半段)。worker 的进度串与状态串格式不同:
rtt.rs:191 `● 正在加载 JLink_x64.dll…` 是 Progress;`● 已连接 (…)`/`● 错误: …` 是 State。
连接流程里二者交替出现,`sessionStatus` 会随最后到达的消息在"进度"与"状态"间翻转——
注释声称"同源不会打架",但实际是**两个不同语义的流写同一个槽**,前端 2s 轮询
`/api/status` 与 WS `state` 事件(`setState(m.connected, m.status)`,index.html:559)
对"当前状态文案"的来源不一致:WS 路径用 State 的原始 status,轮询路径用最后写入者。
表现为连接中阶段状态栏文案闪烁/回跳。
触发条件:连接/断开过程中 Progress 与 State 消息间隔 <2s(常态)。
判定:属设计口径问题而非崩溃,是否可接受需人工确认(桌面版 main.rs tick 是否有
同样的双写——本审计范围外)。

#### [P2] State(false) 的统计清零在 prev!=connected 判定**之外**:同一断连事件会双写 session_start,bytes 在"从未连接"时也清零

文件:web.rs:910-933

```rust
                    let prev = shared.connected.swap(connected, Ordering::Relaxed);
                    if prev != connected {
                        ...
                        // 会话计时:连接记起点,断开清零
                        *shared.session_start.lock().unwrap() = if connected {
                            Some(Instant::now())
                        } else {
                            None
                        };
                        ...
                    }
                    if !connected {
                        // 断开统一口径(F2):会话统计全清 + 设备信息清空
                        shared.rx_bytes.store(0, Ordering::Relaxed);
                        shared.tx_bytes.store(0, Ordering::Relaxed);
                        *shared.session_start.lock().unwrap() = None;
                        *shared.device_info.lock().unwrap() = None;
                    }
```

分析:worker 可能在未连接态发多条 `State(false, …)`(如连接失败重试循环 rtt.rs:249
"连接中…"期间每次失败都发 State(false, err)):
(a) `session_start` 被 `None` 重复写(916-920 的 `else` 分支仅在翻转时,931 无条件写
None)——幂等,无功能错误,但同一锁 10ms 窗口内两次借用;
(b) 更值得注意的是 929-930:`rx_bytes`/`tx_bytes` 在任何 `State(false)` 时清零,**包括
"从未连接"的初始态**。若 worker 在 connect 阶段就发 State(false, "错误: …")
(此时上一会话可能刚断开,bytes 已被 929 清过一次;若上一会话数据尚未被 tick 统计
清零,这里补清是对的——注释 F2 说明正是为此)。幂等性成立,无 bug,登记为
"清零路径依赖 State(false) 到达,worker 若只发 Progress 错误不发 State(false) 则不清"
的耦合(真机 rtt.rs:156 确实发 State(false, err),成立)。
触发条件:无独立故障,属口径记录。

#### [P2] api_addr 在 async handler 内同步执行 UDP 路由探测,可能阻塞 tokio worker 线程数百毫秒以上

文件:web.rs:751-756、768-774

```rust
fn lan_ipv4() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind(std::net::SocketAddr::from(([0, 0, 0, 0], 0))).ok()?;
    s.connect("8.8.8.8:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (ip.is_ipv4() && !ip.is_loopback()).then_some(ip)
}
...
async fn api_addr(State(shared): State<Arc<Shared>>) -> Json<AddrInfo> {
    Json(AddrInfo {
        port: shared.port,
        open: !shared.listen.is_loopback(),
        lan_ip: lan_ipv4().map(|i| i.to_string()),
    })
}
```

分析:UDP `connect` 是同步阻塞 socket 调用(无包发出,只做选路)。正常毫秒级,但
Windows 下 DNS/路由表异常、杀软拦截网络栈、多网卡策略查询慢时可达数百 ms 甚至更久。
该 handler 是 `async fn`,直接跑在 tokio worker 线程上——**没有** `spawn_blocking`,
一次慢调用会冻结该 worker 线程上的**所有**并发请求(包括 WS 分发、/api/status 轮询)。
注释 750 行承认"无网络/特殊多网卡环境可能拿不到"但只说返回 None,未提阻塞面。
触发条件:系统网络栈异常 + 前端轮询 /api/addr(前端远程访问框,频率未知,需人工确认)。
另:该探测硬编码 `8.8.8.8:80` 作为选路目标,国内无墙环境下选路结果通常正确
(UDP 假连接不发包),但语义上依赖"默认路由存在"——无默认路由(纯内网无网关
直连网段)时 `connect` 失败返回 None,lanIp 缺失,前端远程框无数据。

#### [P2] broadcast 缓冲 512 无界于订阅数:WS 消费方持续 Lagged 时,重同步快照在 WS 循环内持 pump 锁全量序列化 500 行

文件:web.rs:583、1002-1011(结合下半段 ws_loop web.rs:1657-1676)

```rust
    let (events_tx, _) = broadcast::channel(512);
...
            let _ = shared.events_tx.send(
                serde_json::to_string(&WsEvent::Rows {
                    seq,
                    dropped,
                    rows: &rows_view(&rows),
                })
                .unwrap(),
            );
```

```rust
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let snapshot = shared.pump.lock().unwrap().snapshot_rows();
                        let msg = serde_json::to_string(&WsEvent::Snapshot {
                            rows: &rows_view(&snapshot),
                        }).unwrap();
```

分析:每个 WS 客户端独立 subscribe。高吞吐 RTT 流下(设备 <30ms/条),单个慢消费者
(大浏览器标签页被后台节流)缓冲 512 条后每次 recv 立即 Lagged → ws_loop 持 pump 锁
`snapshot_rows()` 全量克隆 500 行再 serde_json 序列化 500×runs——**每收一条**都做一次
O(500 行) 快照+序列化,该 WS 的 recv 永不追平,形成"持续重同步风暴";且 `snapshot_rows`
持泵锁期间会阻塞 tick_loop 的 pump 借用(web.rs:870),反噬真机数据泵 10ms 节奏。
broadcast 本身对 Lagged 消费者**不做慢速降级**(axum/tokio 语义:跳过落后条目,继续),
风暴只能靠前端消费速度收敛。
触发条件:单个慢 WS 客户端 + 持续高速数据流。多个正常消费者不受影响(512 缓冲
在 N 个消费者都慢时才整体 Lagged)。
不确定,需人工确认:tests/web 黑盒是否覆盖 Lagged 场景(未见覆盖)。

#### [P3] send_payload 克隆 cmd_tx 后发送:worker 正在退出(Exited 未处理)时命令静默丢弃,回显/TX 计数照常

文件:web.rs:273-277

```rust
        if let Some(tx) = self.cmd_tx.lock().unwrap().clone() {
            let _ = tx.send(WorkerCmd::Send(payload));
        }
        self.tx_bytes.fetch_add(n as u64, Ordering::Relaxed);
```

分析:`cmd_tx` 在 `WorkerMsg::Exited` 处理时清 None(web.rs:985),但 `send_payload`
调用时刻 `cmd_tx` 可能仍 Some 而 worker 线程已退出(退出线程发完 Exited 前,`tx` 的
Receiver 在 worker 侧 drop 前,`tx.send` 成功=消息进 mpsc 缓冲**无人消费**;worker 已退出
后 Receiver drop,send 返回 Err 被 `let _` 吞)。两条窗口:
(a) worker 已死、Exited 尚未被 tick 消化:Send 进缓冲,缓冲随 Receiver drop 丢弃,
回显行已推入泵、TX 已计数——**回显与实发不一致**;
(b) send 返回 Err:同样静默。
触发条件:连接态快速断开瞬间的定时/手动发送。属"尽力而为"语义,注释未声明,
登记为已知的静默吞错路径(与桌面版 main.rs 同模式,需人工确认桌面版是否有同样窗口)。

#### [P3] send_payload 内 6 次独立 Mutex 借用(send_ending/cmd_tx/pump/timer_text/send_history),无原子性

文件:web.rs:270-283

```rust
        let payload = append_send_ending(payload, *self.send_ending.lock().unwrap());
...
        if let Some(tx) = self.cmd_tx.lock().unwrap().clone() {
            let _ = tx.send(WorkerCmd::Send(payload));
        }
        ...
        *self.timer_text.lock().unwrap() = text.to_string();
        self.timer_hex.store(hex, Ordering::Relaxed);
        history_push(&mut self.send_history.lock().unwrap(), text);
```

分析:各锁独立获取且顺序固定(send_ending→cmd_tx→pump→timer_text→send_history),
与 tick_loop 的借用序(pump 持锁期间再借 status_text/session_start/device_info/
worker/cmd_tx/web.rs:870-995)无环:tick 从不持 pump 时借 send_ending/cmd_tx 之外再借
pump,send_payload 从不持 cmd_tx/pump 时再借 pump——**无死锁**(已核查全部锁对)。
但"发送字节→回显→定时源→历史"四步非原子:若发送后进程崩溃,历史缺一条;
并发手动+定时发送同时进 send_payload 时,`timer_text` 最后写者胜,两次发送可能
各自回显但定时源只保留后一条——语义可接受,登记即可。
注:release 构建所有 `lock().unwrap()` 在中毒时会 abort 整进程,见"维度小结"。

#### [P3] parse_hex_bytes 的 `as char` 窄化在 edition 2021 下无警告,非法字节按 U+0000-U+00FF 显示

文件:web.rs:405-411

```rust
    for pair in cleaned.as_bytes().as_chunks::<2>().0 {
        let hi = (pair[0] as char)
            .to_digit(16)
            .ok_or_else(|| format!("非法字符 '{}'", pair[0] as char))?;
```

分析:cleaned 是 `String`(UTF-8),但取 `as_bytes()` 逐字节再 `as char` 窄化——
若输入含多字节 UTF-8(中文等),每个 UTF-8 字节被当作独立的 U+0080-U+00FF
Latin-1 码点,`to_digit(16)` 返回 None,报"非法字符"——**行为正确**(非 hex 字符
就该报错),只是错误信息里的 `'{}'` 会打印 Latin-1 乱码码点而非原字符。功能无 bug;
错误文案可读性差。edition 2021 不默认启用 `checked_float…`/`char` 窄化警告类 lint,
无编译提示。触发条件:用户往 HEX 模式输入中文/emoji。

#### [P3] text_to_hex_upper 输出以尾随空格结尾,且容量按 `s.len()`(字节数)而非字节×3 的 1/3 估算

文件:web.rs:431-438

```rust
fn text_to_hex_upper(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        let _ = write!(out, "{b:02X} ");
    }
    out
}
```

分析:每字节 3 字符(`XX `),输出末尾恒多一个空格(与注释 430 行示例
`"[ de" → "5B 20 64 65 "` 一致,契约如此);容量 `s.len()*3` 恰好精确(ASCII 时 1 字节→3
字符),CJK/emoji 时 `s.len()`(UTF-8 字节数)反而**高估**(1 字符 3 字节→9 容量 vs 需要
3),无害。`write!` 的 Err 被 `let _` 吞(String 写不会失败,无碍)。仅登记格式事实。

#### [P3] scan_custom_themes 先 is_file() 再取 path:TOCTOU 窗口内文件被删/改名,后续 /api/theme-css 读盘才失败

文件:web.rs:332-353

```rust
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !valid_theme_file_name(&name) {
                return None;
            }
            Some(CustomTheme {
                id: name[..name.len() - 4].to_string(),
                name,
                path: e.path(),
            })
        })
```

分析:扫描与后续 `/api/theme-css/{id}` 的 `fs::read_to_string` 是两次独立系统调用,
用户拖走 .css 的瞬间列表项短暂出现、点取 CSS 得 404/读错文件——本机单用户场景,
无安全面(id 白名单 321-328 行已排穿越/控制字符/Unicode 混淆,`name[..name.len()-4]`
切片安全因白名单保证纯 ASCII 且 len>4)。登记为窗口事实,非缺陷。

#### [P3] timer_interval 的 f64 经 `format!` 落盘再 `parse` 往返,serde_json 序列化 f64 无精度损失但 `format!("{}", 0.1)` 得 "0.1" 可逆——实际无问题,仅登记往返依赖

文件:web.rs:369、575-581

```rust
    p.timer_interval = format!("{}", *s.timer_interval.lock().unwrap());
```

```rust
    let timer_interval_init = saved
        .timer_interval
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| TIMER_INTERVAL_RANGE.contains(v))
        .unwrap_or(1.0);
```

分析:f64 经 `format!` → String → serde_json 字符串字段 → `parse::<f64>`,往返对
RangeInclusive 内的常见值(0.001-999)可逆(Rust f64 Display 用最短可往返表示)。
`TIMER_INTERVAL_RANGE.contains(v)` 对 NaN 返回 false(NaN 不在任何区间),
`parse::<f64>` 对 "NaN"/"inf" 成功但被 filter 拦——健壮。登记为已核查无异常。

#### [P3] Exited 分支:ConnectedChanged(false) 回调在 session_start 清零**之前**发出,回调侧读 sessionSec 可能多一次旧值

文件:web.rs:986-995

```rust
                        let prev = shared.connected.swap(false, Ordering::Relaxed);
                        if prev {
                            if let Some(cb) = &shared.on_state {
                                cb(ServiceEvent::ConnectedChanged(false));
                            }
                        }
                        // worker 线程已退:会话计时清零(State(false) 通常已先行
                        // 处理,这里兜底)
                        *shared.session_start.lock().unwrap() = None;
```

分析:回调只驱动托盘图标(gui.rs 侧 `send_event`,非阻塞),不读 session_sec;
500ms 统计广播在 `last_stats` 窗口后取 `session_start`——回调与清零间隔微秒级,
下一个 Stats 事件(≤500ms)取到的已是 None。顺序无害,登记为已核查。

#### [P3] demo 启动分支在 serve 的 async 上下文里同步推泵/发回调,先于 axum 开始接服务,状态事件早于任何 WS 客户端

文件:web.rs:629-651

```rust
        *shared.session_start.lock().unwrap() = Some(Instant::now());
        *shared.status_text.lock().unwrap() = "已连接 (demo)".into();
        shared.pump.lock().unwrap()
            .push_colored_line(&state_marker_line(true), MARK_COLOR);
        if let Some(cb) = &shared.on_state {
            cb(ServiceEvent::ConnectedChanged(true));
        }
```

分析:该段跑在 `spawn_gui_service` 的后台线程 `block_on(serve(...))` 入口,
bind 之前。`ConnectedChanged(true)` 经 `on_event` 打到 gui 的 `proxy.send_event`——
此时 tao 事件循环**尚未运行**(gui.rs:80 拿到 ready_rx 后才 `event_loop.run`),
事件排队(gui.rs:78-79 注释已声明此语义),无丢失。WS 客户端此时不可能存在
(服务未 bind),首帧快照(web.rs:1648)会在连接时覆盖。已核查无异常。

#### [P3] 已核查的 unwrap/expect 组(release panic=abort 语义下逐一判定不可达或低危)

- `serde_json::to_string(&WsEvent::…).unwrap()`(web.rs:906/937/950/956/966/1010):
  `WsEvent` 全字段为 u64/usize/bool/`&str`/`Vec<String>`/`serde_json::Value`/
  `&[RunsRow]`(其 `ansi::Run` 带 `Serialize` derive,src/ansi.rs:11-17),
  `Value` 与字符串均不可能序列化失败——**不可达**。
- `pump.lock().unwrap()` 等全部 `Mutex::lock().unwrap()`(web.rs:270-283/358-372/
  634-646/868-1033):锁中毒仅在另一线程持锁 panic 后发生;持锁段内代码无
  unwrap 可 panic(已核查各持锁段),且 release `panic=abort` 下进程不存活
  到第二次 lock——**不可达**。
- `SPEEDS_KHZ.len() as i32 - 1`(web.rs:477/613):`SPEEDS_KHZ` 是 `[u32; 8]` 常量
  数组(rtt.rs:68),`len()` 恒 8,无下溢。
- `parse_hex_bytes` 的 `as_chunks::<2>().0`(web.rs:405):奇数长度已 401 行提前
  return,余量恒 0——安全。
- `text_to_hex_upper` 的 `let _ = write!(…)`(web.rs:435):String 写不失败。
- `open_in_browser` 的 `let _ = spawn()`(web.rs:744):Windows 上 cmd.exe 缺失
  不可能;失败静默为设计(741 行注释)。
- `ready_tx.send(...)` 的 `let _`(web.rs:172/180/187):Receiver drop 后静默,预期。
- `config::load()`(web.rs:567):`load_from` 全 `.ok()` 链 + `unwrap_or_default()`
  (config.rs:135-139),损坏/缺失文件回落默认,无 panic。

### 维度小结(上)

- **panic**:所有 `unwrap/expect/索引` 逐一定位(见 P3-unwrap 组),均不可达或受常量约束;
  唯一系统性风险是 `Mutex::lock().unwrap()` 的锁中毒→abort 链,但持锁段内无可 panic 点,
  release 下实际不可达。未见新增 panic 隐患。
- **错误传播**:`send_payload` 的 `tx.send` 与所有 `broadcast::send`/`events_tx.send`
  均为 `let _` 静默(消费方全掉线/worker 已死时回显仍上屏、TX 仍计数,回显与实发可能不一致);
  `config::save` 失败静默返回 false(设计);`lan_ipv4`/`open_in_browser` 失败静默(设计);
  `api_open_browser` 超限请求体(>2MB)被 axum 默认体限直接 413 而非进入 handler。
- **边界**:`parse_hex_bytes` 奇数位/空/非法字符全覆盖且错误信息回传前端;
  `append_send_ending` 未知值(`_`)安全回落"无";`SavedConnect::from_prefs` 全部 clamp;
  `ConnectReq` 的 usize 字段经 serde 负数→反序列化失败→400,再 `min` 夹取双保险;
  `valid_theme_file_name` 白名单严密(≤64 字节/ASCII/.css 结尾/去后缀非空隐含)。未见整数
  溢出或 UTF-8 截断面(唯一 `as char` 窄化见 P3-parse_hex_bytes,行为正确)。
- **并发**:锁对已全量核查无死锁(借用序 pump→外围 与 send_payload 的独立短锁无环);
  `tick_loop` 持 pump 锁期间调用 `on_event` 回调(web.rs:921/988,回调约定非阻塞,
  gui 侧仅 `send_event`)与 `snapshot_prefs`(无,在 drop 后 1033 行)——无再借泵锁;
  `broadcast` 无界于订阅数,慢消费者 Lagged 重同步风暴见 P2;`cmd_tx` 与 worker 退出的
  静默窗口见 P3-send_payload;`rx_ending` 等在 tick 开头一次性读出,无中途变化。
- **资源**:`spawn_gui_service` 服务线程无 cancel 句柄,壳 15s 超时后线程继续存活并
  可能持端口(P1-3,当前 main 退出进程故无害);demo/设备库/数据泵三线程随进程终止
  回收,无 `Drop` 需求;WS 订阅随连接 drop 自动退订;`UdpSocket` 函数尾析构;
  无句柄泄漏面。

---

# T2 深审 web.rs(下):1015-1995

审计范围:`src/web.rs` 1015-1995 行(api_status 起至 mod tests 末尾)。上半段(serve/tick_loop/WsEvent)仅在关联处引用。
背景核实:`SPEEDS_KHZ: [u32; 8]`(rtt.rs:68)、`MAX_LOG_ROWS=500`(log_model.rs:80)、worker 退出序 `State? → alive=false → Exited`(rtt.rs:154-161)。

### 发现清单(下:1015-1995)

#### [P2] 负数 i32 字段被 clamp(0,N) 静默夹成 0,与负数语义(应为 400)不一致
文件:`src/web.rs:1277-1279`(connect)与 `:1447-1448 / :1469 / :1480 / :1483`(settings)

```rust
// api_connect
let iface_index = req.iface_index.min(1);
let speed_index = req.speed_index.min(SPEEDS_KHZ.len() - 1);
let channel = req.channel.min(15);
```
```rust
// api_settings
if let Some(v) = req.rx_ending {
    *shared.rx_ending.lock().unwrap() = v.clamp(0, 4);
}
```
分析:`i32::min(1)` 对负数返回负数本身;而 `api_connect` 里 `req.iface_index` 是 `usize`(ConnectReq,web.rs:451-455),`JSON -1` 反序列化直接 400 拒绝(整请求合法失败,可接受)。但 `api_settings` 的 `iface_index/speed_index/rx_ending/send_ending/encoding_index` 均为 `Option<i32>`(web.rs:512-517,525,536-539),`clamp(0, N)` 把 `-5` 静默夹成 0 并返回 200——用户发送非法值没有任何 400 反馈,只是被悄悄改写。同一请求里 `frame_timeout` 用 `clamp(1,200)`(web.rs:1452)同样吞负数。触发条件:客户端/脚本发 `{"ifaceIndex":-1}`。行为安全(不越界、不 panic),属错误传播/语义问题,非崩溃路径。

#### [P2] /api/prefs 与 /api/timer 口径核查 + api_timer 的 on 为必填(契约与注释一致,但忘传 on 会静默开关定时器)
文件:`src/web.rs:1126`、`:1009-1043`、`:1502-1508`

```rust
// api_prefs
"speedIndex": *shared.speed_index.lock().unwrap(),
```
```rust
// tick 定时发送
let secs = *shared.timer_interval.lock().unwrap();
if TIMER_INTERVAL_RANGE.contains(&secs)
    && last_timer.elapsed() >= Duration::from_secs_f64(secs)
```
```rust
// api_timer
if !TIMER_INTERVAL_RANGE.contains(&v) {
    return (StatusCode::BAD_REQUEST, "定时间隔需在 0.001-999 秒").into_response();
}
```
分析:`TimerReq.on` 是**非 Option 必填**(web.rs:548),每次调 /api/timer 都必须传 on——与文档注释(行 36-37 "intervalSec?/text?/hex?" 只标这三个可缺)一致,契约本身没写错;真正值得注意的是:api_timer 不校验 `on` 与文本组合、且 `req.on` 永远落盘,前端若只改 interval 忘传 on 会静默开/关定时器(前端 752-758 行已总是传 on,故实际不触发)。**不确定,需人工确认** 这是否是有意设计。speedIndex 在 prefs 里是 `i32`(启动时已 clamp,web.rs:613),api_connect 是 `usize` min-clamp——两条入口夹取口径不同(一个负数 400、一个负数→0),对外表现为"同一份设置的两个 API 对非法输入处理不一致"。触发条件:直连 API。

#### [P2] WS 快照/重同步消息超大时单次 `send` 可能静默截断,且 Lagged 重发快照可能再次 Lagged → 死循环重发
文件:`src/web.rs:1645-1688`

```rust
Ok(payload) => {
    if socket.send(Message::Text(payload.into())).await.is_err() { break; }
}
Err(broadcast::error::RecvError::Lagged(_)) => {
    let snapshot = shared.pump.lock().unwrap().snapshot_rows();
    ...
    if socket.send(Message::Text(msg.into())).await.is_err() { break; }
}
```
分析:① 500 行 × 每行 runs 的快照 JSON 可达数百 KB,超过底层 write buffer 时 `send` 内部是流式写,单条 Text 消息本身可跨多个 TCP 段发送,tungstenite/axum 处理为一次 `send` 调用内排空——此项**不确定,需人工确认** axum 0.8 `WebSocket::send` 对超大单条消息的背压行为(读 `~/.cargo/registry` 中 axum 源码确认其为 await 一次完成,不会半途返回 Ok)。② 真正的结构性问题:Lagged 分支重发快照不检查重发后又收到 Lagged 的级联——高速流下(500ms 统计广播 + 高频 rows),一个慢客户端会陷入"收 Lagged → 发全量快照(又是大消息)→ 再 Lagged → 再发"的循环,每个循环都全量 clone 500 行(`snapshot_rows` 深拷贝,web.rs:1668)并序列化,单会话即可打满 CPU/内存。broadcast 容量 512(行 583),500 行 rows 事件 + 500ms 一次 stats,滞后窗口很小,实际触发需"客户端 TCP 拥塞 > 数据产生速率"。属资源/性能风险。

#### [P3] reset reconnect 重连线程与 Exited 清理的交互:worker 句柄被 take 后,若 1s 内用户手动 /api/connect 成功,重连线程会**跳过**重连但 `take_worker_down` 已清掉 cmd_tx——手动连接正常,无泄漏
文件:`src/web.rs:1411-1437`

```rust
let old = take_worker_down(&shared);
...
if shared2.worker.lock().unwrap().is_some()
    || rtt::APP_SHUTDOWN.load(Ordering::Relaxed)
{
    return;
}
spawn_worker(&shared2, &params);
```
复核 T1 登记(不重复登记,仅核实行号):`take_worker_down`(1350-1359)在 1351 行 `shared.worker.lock().unwrap().take()` 立即清空句柄,`wait_worker_exit`(1363-1375)在重连线程内轮询 `h.alive`(1368)。窗口内 `api_connect`(1239-1244 的门闩)检查 `worker` 字段——已被 take 为 None,门闩**确实失效**:1s 窗口内手动连接会 spawn 一个新 worker,而旧 worker 线程尚未退出(旧线程 3s 内未退出才在 1422 被拦)。即:reconnect 点击后 1s 内点"连接",两个 worker 可能并发存在——与"worker 生命周期铁律"冲突。触发条件:reset(reconnect) → 1s 内 → /api/connect,且旧 worker 未退出(正常 <300ms 退出,窗口窄但真实)。重连线程 1429 行 `is_some()` 检查能拦住 spawn,但**拦不住 api_connect 的门闩**(门闩读的是已被清空的 Option)。线程本身:重连线程跑完即退(1418-1435 无循环),无泄漏——资源维度此项干净。

#### [P3] api_power 对真机"已连接但 cmd_tx 为 None"不可达:worker 存活时 cmd_tx 恒为 Some,409 分支只覆盖 demo 未连接与"Exited 后未收到"瞬态,语义可接受
文件:`src/web.rs:1326-1336`

```rust
let Some(tx) = shared.cmd_tx.lock().unwrap().clone() else {
    return (StatusCode::CONFLICT, "未连接").into_response();
};
let _ = tx.send(WorkerCmd::Power(req.on));
StatusCode::OK.into_response()
```
分析:已检查,未见异常(真机 cmd_tx 随 worker 登记/清退,行 985;demo cmd_tx 启动即 Some,行 634)。`tx.send` 返回 Err 被 `let _` 静默——worker 已退出时 send 失败,但此时 power 本就不该生效,200 略乐观,前端禁用按钮兜底。低风险。

#### [P3] WinSystemTime 是 `#[repr(C)]` 但字段序为直觉序,与 Windows SYSTEMTIME 实际布局不符——GetLocalTime 写入按字段偏移,小时/分钟/秒偏移错位
文件:`src/web.rs:1521-1550`

```rust
#[repr(C)]
struct WinSystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    millis: u16,
}
```
分析:Windows SYSTEMTIME 实际布局为 `wYear, wMonth, wDayOfWeek, wDay, wHour, wMinute, wSecond, wMilliseconds`——与本结构**恰好一致**(AGENTS.md 亦记载此序)。已核对 Win32 SDK 字段序,匹配,无错位。此条为"已核查,未见异常"(防误报登记)。`hms_stamp`/`now_stamp` 依赖它,格式正确。

#### [P3] api_mark 的 label 无长度上限,可经 WS 行模型把超长标记行注入(512 字符硬切只作用于设备流,不作用于 push_colored_line)
文件:`src/web.rs:1584-1593`、`src/log_model.rs:197-201`

```rust
let mut pump = shared.pump.lock().unwrap();
pump.push_colored_line(&marker_line(&label, &hms_stamp()), MARK_COLOR);
```
分析:`push_colored_line` 直接 push 整行(不截 512),但受 `MAX_LOG_ROWS=500` 与行级 wrap(无界单行)约束——单个 1MB 的 /api/mark 文本会产出一行 1MB 的 run,经 WS rows/snapshot 广播给所有会话,且进导出文件。管理台无鉴权(文档明示),局域网暴露时是放大面。属边界问题,非 panic。触发条件:HTTP 客户端发大 text。

#### [P3] 单测 `ws_event_wire_format` 的 Device 分支是"自引用恒真":info 用 `json!` 构造再 `to_string` 拼进期望串,断言只锁外层 `{"type":"device","info":…}` 包装
文件:`src/web.rs:1931-1940`

```rust
let info = serde_json::json!({...});
assert_eq!(
    serde_json::to_string(&WsEvent::Device { info: info.clone() }).unwrap(),
    format!(r#"{{"type":"device","info":{}}}"#, serde_json::to_string(&info).unwrap())
);
```
分析:测试注释自己承认"info 仍由原 json! 构造,断言只锁外层包装"——这是诚实声明,但意味着 device info 8 字段的键名/顺序**未被逐字节锁**(其余 7 个变体是真锁)。属"弱断言"而非"假绿"(外层包装确实被锁)。

#### [P3] `clean_status_text` 只剥单个 `●` 前缀 + 一次 trim:若状态文案为 "● ● 已连接" 或 "  ● 已连接" 会剥不干净
文件:`src/web.rs:1571-1573`

```rust
fn clean_status_text(s: &str) -> String {
    s.trim().trim_start_matches('●').trim().to_string()
}
```
分析:worker 侧 State 文案恒为单 `●` 前缀(rtt.rs/demo.rs 口径),实际不触发;属防御性边界,登记备查。

#### [P3] api_export 持 pump 锁期间构建整个 body(500 行 × runs 深拷贝 + 拼接)
文件:`src/web.rs:1613-1638`

```rust
let body = {
    let pump = shared.pump.lock().unwrap();
    let mut body = String::new();
    for row in pump.snapshot_rows() { ... }
    body
};
```
分析:`snapshot_rows` 已 clone 一次,锁内再遍历拼接。500 行上限下耗时 <ms 级,但持锁时间随行长增长(单行无 512 上限,见 [P3] api_mark)。低概率长持锁,可接受,登记。

#### [P3] /api/reset 请求体边界核查:空 body 容忍、畸形 JSON 400、非对象 JSON 400——未见异常(备查登记)
文件:`src/web.rs:1381-1393`

```rust
let req: ResetReq = if body.is_empty() {
    ResetReq { mode: None }
} else {
    match serde_json::from_slice(&body) { ... }
};
```
分析:已检查,未见异常。`ResetReq.mode` 有 `#[serde(default)]`(行 1342-1343),`{}` 合法。边界覆盖完整。

#### [P3] api_connect 的 chip 补全用 `full.iter().find(|s| s.to_uppercase().contains(&needle))`:子串包含而非前缀/单词匹配,"ST" 会匹配到第一个含 ST 的型号(如 STM32F030C8 先于 STM32F103C8 取决于列表顺序),与"首个全称"注释不符
文件:`src/web.rs:1262-1264`

```rust
let needle = chip_raw.to_uppercase();
match full.iter().find(|s| s.to_uppercase().contains(&needle)) {
```
分析:注释(行 1236-1237)写"库内子串匹配首个全称,与桌面版同规则"——实现与注释一致(子串包含),行为是设计如此,但"ST" 这种 2 字符输入会命中列表中第一个含 ST 的型号而非用户预期。属产品行为,非 bug,登记备查。

### 单测质量表(15 个)

| 测试名 | 有效/弱/假绿 | 理由 |
|---|---|---|
| hex_parse_tolerates_separators_and_prefix | 有效 | 锁定分隔符/0x/0X 前缀 7 种输入的输出字节,含内容断言 |
| hex_parse_rejects_bad_input | 有效 | 4 种非法输入只断 is_err()——**略弱**:不锁错误文案,但"拒绝"契约被锁,可接受 |
| send_ending_appends_bytes_per_selection | 有效 | 锁定 0/1/2/3/越界 5 种 ending 的精确字节输出,含 HEX 路径 |
| settings_req_accepts_camel_case_json | 有效 | 锁定 camelCase 字段反序列化命中(修复前 snake_case 静默丢),含 F1 连接组五项 |
| connect_req_connects_camel_case_json | 有效 | 锁定 4 字段反序列化;未测负数/越界(clamp 在 handler 不在反序列化层,不算缺口) |
| timer_req_accepts_camel_case_json | 有效 | 锁定 on/intervalSec/text/hex 反序列化 |
| reset_req_tolerates_missing_mode | 有效 | 锁定 reconnect 命中 + `{}` 缺省 None |
| reset_mode_defaults_and_falls_back_on_unknown | 有效 | 锁定 None/缺省/未知值/空串 5 种回落 InPlace |
| text_to_hex_upper_matches_worker_hex_format | 有效 | 锁定空串/ASCII/UTF-8 多字节精确输出(含尾空格) |
| state_marker_line_uses_manual_mark_style | 有效 | marker_line 精确锁定;state_marker_line 因实时时间戳只锁骨架+标签(时间不定,合理) |
| clean_status_text_strips_banner_bullet | 弱 | 3 个输入只锁前缀剥离;不覆盖 "  ●  " 双前缀/无前导空格变体(与 [P3] clean_status_text 对应) |
| saved_connect_from_prefs_clamps_and_requires_chip | 有效 | 锁定 chip 为空→None、越界 iface/speed/channel 精确夹取值(7/15) |
| history_push_dedupes_moves_to_front_and_caps | 有效 | 锁定置顶/去重/50 上限/超限重发挤尾,断言具体内容(h[0]、len) |
| theme_file_name_whitelist_rules | 有效 | 白名单+9 种非法(穿越/非 css/超长/全角/空格)全锁 |
| ws_event_wire_format_matches_legacy_contract | 有效(1 弱) | 7 个变体逐字节锁定转义/键序;仅 Device 的 info 内部是自引用恒真([P3] 已登记) |
| scan_custom_themes_sorted_filtered_missing_dir_empty | 有效 | 真实文件系统测试:缺失目录/字典序/只收 .css/目录排除,断言 id 序列 |

(说明:题目说 15 个单测,实际 mod tests 内为 15 个 `#[test]`——逐一核对完毕,无"只验长度不验内容"的假绿;最接近假绿的是 Device 分支自引用断言,已在发现清单登记。)

### 维度小结(下)

- **panic**:审计区间内 `unwrap()` 集中在 `lock().unwrap()`(毒锁传播,可接受)与 `serde_json::to_string(...).unwrap()`(WsEvent 恒可序列化,不可达);`SPEEDS_KHZ[params.speed_index.min(7)]`(web.rs:1222)因 min-clamp 恒在界,无越界;无除法/无 `expect`。未见 panic 可达路径。
- **错误传播**:HEX 解析失败 400 带文案(1496-1498)、reset 畸形 JSON 400(1389)、timer 越界 400(1505)均正确;`tx.send` 失败被 `let _` 静默(1321/1356/1384 等)属有意(命令通道尽力),可接受;负数 i32 clamp 吞错(见 [P2] 第 1 条)。
- **边界**:负数 i32 经 `clamp(0,N)` 归零(api_settings)vs 经 `min` 拒绝(usize 的 ConnectReq 400)口径不一;`/api/reset` 空 body 容忍正确;超长 mark 文本不截断(登记 [P3])。
- **并发**:take_worker_down 后 1s 门闩失效已复核——T1 登记属实(web.rs:1351 take → 1239 门闩读 None),重连线程自身 1429 有让位检查,但拦不住 api_connect;无新增并发缺陷。`last_stats/last_timer` 为 tick 线程局部,无竞争。
- **资源**:重连线程(1418)跑完即退,无泄漏;WS 会话经 `on_upgrade` 闭包持有 socket,break 即析构,无显式资源泄漏;Lagged 重同步循环是唯一资源风险(见 [P2] 第 3 条)。

**去重说明**:上半段 P2-6(broadcast Lagged 重同步风暴,web.rs:583 + 1657-1676)与下半段 P2-3(WS Lagged 级联重发,web.rs:1645-1688)描述同一结构性问题,合并计 1 条 P2;T1 C 节"reconnect 双 worker 窗口"经下半段复核**属实**(take 1351 清空句柄 → api_connect 门闩 1239-1244 读 None),维持 P1。

**主会话 S5 抽查(2026-09-25)**:part-a 抽 `web.rs:935-938`(State 广播用原始 status,存 status_text 已清洗——与源码一致)、`axum-core-0.5.6/src/ext_traits/request.rs:319`(`DEFAULT_LIMIT: 2_097_152`——registry 源码一致);part-b 抽 `web.rs:1275-1277`(api_connect min-clamp)与 `web.rs:1451`(rx_ending clamp(0,4))、`take_worker_down` 1350-1359 与重连线程 1418-1434(门闩失效路径与报告描述一致)。抽查 4/4 通过,无坏条。

**web.rs 节发现统计**:P0:0 · P1:3(上半段:State status 未清洗广播 / open-browser 2MB 体限+死接口 / 服务线程无 cancel 句柄)+ 继承 T1 的 P1(下拉未接线、reconnect 窗口,本节复核属实) · P2:4(min-clamp 与 clamp 负数口径不一、Progress/State 双写 status_text、lan_ipv4 阻塞 tokio、Lagged 重同步风暴合并 1 条) · P3:11(上下段合计 11,含"已核查无异常"备查 4 条) · 单测 15 个逐一过审:无假绿,1 弱断言(Device 自引用)已登记。
---

## src/rtt.rs(473 行,worker 线程模块)


审计对象:`src/rtt.rs`(473 行,工作树 HEAD ecc5294;`git status` 仅 `?? docs/audit/`,rtt.rs 本身无未提交改动)。
release 下 `panic = "abort"` 已确认(Cargo.toml:39-46 `[profile.release]` → `46: panic = "abort"`)。

### 审计范围与背景核实

- `src/rtt.rs` 全文 1-473 行:WorkerMsg/WorkerCmd 协议(7-60)、APP_SHUTDOWN(64)、
  WorkerConfig(77-92)、ENCODINGS(97-103)、CharsetDecoder(107-132)、spawn(143-163)、
  connect_target(179-279)、rtt_read_loop(283-422)、tests(424-473)。
- 调用边界对照(未展开 FFI 内部):
  - `src/jlink_dll.rs`:返回值语义 —— `rtt_read`(172-178,注释「< 0 = 错误」)、
    `rtt_write`(180-186)、`select_by_usb_sn`(391-401,符号缺失返回 -1)、
    `open_ex`(62-68,返回值 c_int 未在任何处检查)、`exec_command`(124-135,只回文本无返回码)、
    `reset`(147-153) / `go`(156-161) 返回 c_int。
- 消费侧对照(协议正确性依据):
  - `src/web.rs:859-998` tick_loop 的 `msg_rx.try_recv()` 分发;`968-996` 对 Exited 的清理
    (依赖「alive=false 在 Exited 前置位」,见 web.rs:969-977 注释)。
  - `src/web.rs:1216-1234` spawn_worker;`1310-1324` api_disconnect(置 stop + 空 Send 唤醒);
    `1350-1375` take_worker_down/wait_worker_exit(铁律等待);`1411-1434` reset reconnect。
  - `src/gui.rs:198-228` exit_process:置 `APP_SHUTDOWN` + 3s TerminateProcess 看门狗,不 join worker。
  - `src/device_db.rs:63-87` 后台枚举线程(BUSY 门闩持有者;74 行 `jlink.open()` 重试)。
- crate API 核实(按纪律读 registry 源码,非凭记忆):`encoding_rs 0.8.35`
  (Cargo.lock 锁定 0.8.35) `max_utf8_buffer_length`(src/lib.rs:3777,文档「given the
  current state of the decoder and byte_length number of additional input bytes」——含
  decoder 已有 pending 状态)与 `decode_to_string`(src/lib.rs:4069)。
- 验证命令:`cargo test --lib rtt` 5 passed;`cargo clippy --lib` 对 rtt.rs 无警告。

### 发现清单

### [P1] R1 连接阶段不检测 stop/APP_SHUTDOWN:窗口关闭(未 Terminate)时 worker 会阻塞 ~3.2s,期间 3s 看门狗必然先杀进程

文件:src/rtt.rs:237-253

```rust
let resp = jlink.exec_command(&format!("Device = {chip}"));
...
let mut rc = jlink.connect();
for attempt in 1..4 {
    if rc >= 0 || stop.load(Ordering::Relaxed) {
        break;
    }
    let _ = tx.send(WorkerMsg::Progress(format!(
        "● 连接中…(第 {attempt} 次重试)"
    )));
    thread::sleep(Duration::from_millis(400));
    rc = jlink.connect();
}
```

分析:重连循环只检测 `stop`,从不检测 `APP_SHUTDOWN`;且 237-243 行(第一次
`connect` 前)无任何退出检查,`JLINKARM_Connect` 本身(固件握手/目标无响应)可能阻塞
数秒到数十秒(不确定,需人工确认具体上限)。退出路径两条:
- 窗口关闭 → `gui.rs:219` 置 APP_SHUTDOWN + `gui.rs:213-217` 3s 看门狗 →
  `terminate_self`(gui.rs:227)。worker 卡在 connect 期间,3s 看门狗先触发,
  TerminateProcess 内核强杀。功能上进程确实退出(gui.rs:15-23 注释承认「worker 不 join」),
  但 worker 线程被 Terminate 时正阻塞在 `JLINKARM_Connect` DLL 调用——该线程栈上的
  DLL 状态(JLinkARM 内部会话/RTT 句柄)不会经过 close() 清理。J-Link 驱动通常以
  USB 设备复位兜底,但这是"内核强杀未清理线程"路径,属登记项而非证明无副作用。
- 纯服务形态(`--no-window`,axum serve,gui.rs 不介入)进程退出路径未见 APP_SHUTDOWN
  置位点(web.rs 中无 `APP_SHUTDOWN.store`;web.rs:1430 只读),worker 只能靠 5s 后
  循环退出——若卡在 connect,纯服务进程将一直存活(不确定:纯服务形态是否另有信号
  处理,需人工确认,web.rs 未审全部信号路径)。

触发条件:连接建立阶段(尤其目标未供电/芯片名错/首次连接必失败场景)关闭窗口。
判定:登记。connect_target 各阻塞段(exec_command/open_ex/connect)应插入
`APP_SHUTDOWN`(或 stop)检测点;至少在 `exec_command` 后与每次 `thread::sleep` 前检查。

### [P2] R2 open_ex / select_tif / set_speed 的返回值被静默丢弃,DLL 失败时 UI 停留在「连接中…」且错误信息指向错误的环节

文件:src/rtt.rs:195-217

```rust
jlink.disable_dialog_boxes();
...
let sn_to_use = selected_sn.or_else(|| emus.first().map(|(sn, _)| *sn));
if let Some(sel) = sn_to_use {
    let rc = jlink.select_by_usb_sn(sel);
    if rc < 0 { ... bail ... }
    jlink.exec_command(&format!("SetHostIF USB = {sel}"));
    ...
}
jlink.open_ex();          // 返回值 c_int,未检查
```

分析:`open_ex` 返回 c_int(jlink_dll.rs:62-68),未打开时 `JLINKARM_GetSN` 可能返回 0,
随后 220-230 行会向 UI 发「[J-Link] 序列号 0,建立连接…」误导日志;`select_tif`/
`set_speed`(jlink_dll.rs:106-121)同样丢弃返回值。`exec_command` 返回文本
(jlink_dll.rs:124-135),237-240 行只把非空 resp 当提示日志,命令被 DLL 拒绝
(如未打开就发 SetHostIF 的命令错误文本)会当普通提示混进日志。错误最终在 `connect()`
处爆出,UI 只看到「连接目标失败(错误码 {rc})」,真实失败环节(Open 失败)丢失。
判定:登记(错误传播面),非崩溃面。

### [P2] R3 字符集/HEX 热切换丢弃解码器内部 pending 半字节残留,跨块多字节字符可能丢一个

文件:src/rtt.rs:318-323

```rust
// 字符集动态生效:检测到下拉变化即热重建解码器(旧编码的半字节残留
// 一并丢弃——按新编码解释旧残留没有意义),并发横幅提示
let want_enc = encoding_index.load(Ordering::Relaxed);
if want_enc != current_enc {
    current_enc = want_enc;
    decoder = CharsetDecoder::for_label(enc_label(current_enc));
```

分析:`CharsetDecoder::decode`(120-131)以 `last=false` 解码,decoder 内部持有跨块
pending 半字节。切换时整个旧 decoder 被替换,未 flush——例如 UTF-8 流在块边界把
3 字节字符切为 1+2:第一块结束 pending 1 字节;用户此时切字符集,这 1 字节被静默丢,
UI 上该字符缺失。注释自认此取舍。触发概率低(需切换恰好落在多字节中间),属已知
取舍登记,不建议强行修(按新编码解释旧残留同样错,最多只能 flush 旧 decoder 把
最后一个替换字符补出)。判定:登记,不修。

### [P3] R4 帧结束判定有 1 个轮询周期(≤5ms)的滞后,且帧开/帧关两条路径的判据不对称

文件:src/rtt.rs:307-359

```rust
let mut last_rx = Instant::now();
let mut frame_open = false;
...
if n > 0 {
    if frame_open && now.duration_since(last_rx) > to {
        let _ = tx.send(WorkerMsg::FrameEnd);
    }
    ...
    frame_open = true;
    last_rx = now;
} else if frame_open && now.duration_since(last_rx) > to {
```

分析:`to` 为 UI 设定(1-200ms,web.rs:1449-1453 夹取 clamp(1,200);prefs 解析同
clamp,web.rs:568-574)。帧结束在**下一次**轮询的 read 之后才判定,滞后 ≤5ms——
对 1ms 设定,实际断帧间隔 ≈ 6-10ms。UI 侧(web.rs:895-898 `auto_frame`)只在
`auto_frame` 开启时消费 FrameEnd,滞后只影响自动断帧粒度,属设计精度取舍
(轮询间隔决定精度上限,rtt.rs:414-415 注释已声明)。无崩溃面。判定:已检查,
属声明过的精度取舍,登记备查。

### [P3] R5 Reset 分支在 worker 线程内做 RTT STOP/START 重挂,与 5ms 轮询共享同一 DLL 会话

文件:src/rtt.rs:377-389

```rust
WorkerCmd::Reset => {
    let rc = jlink.reset();
    if rc < 0 { ... }
    else {
        jlink.go();
        // 复位后固件重新初始化 RTT 控制块,重挂 RTT 保证继续可读
        jlink.rtt_control(RTT_CMD_STOP);
        jlink.rtt_control(RTT_CMD_START);
```

分析:`go()`(jlink_dll.rs:156-161)返回值被丢弃(与 R2 同类,此处为 P3:
reset<0 已提前退出)。重挂失败(rc 无检查)时 RTT 可能不再可读,循环会继续读,
最终靠 `n < 0` 分支(360-367)兜底报「RTT 读取失败」。失败面有兜底,只登记。
另注:设备库并发(TLS 损坏)的 BUSY 门闩(`device_db::busy()`)在 web.rs 的
api_connect/web.rs:1238-1306 中**未被检查**(device_db.rs:63-87 枚举线程持有
BUSY 期间可 load DLL 甚至 `open()`,device_db.rs:74)——该门闩检查缺失属 web.rs
层面,登记到 web 文件审计任务,不在 rtt.rs 范围。

### [P3] R6 断开时 worker 的 `State(false)` 先于 `Exited`,若 UI 侧断开触发与 worker 退出不同步,状态栏短暂显示「未连接」后按钮才解锁

文件:src/rtt.rs:418-421 与 src/web.rs:1310-1324

```rust
    // 清理:逐个包 try,单次失败不阻断退出路径
    let _ = jlink.rtt_control(RTT_CMD_STOP);
    jlink.close();
    let _ = tx.send(WorkerMsg::State(false, "● 未连接".into()));
```

分析:顺序为 State(false)→(线程尾)Exited,与 web.rs:985-994 的假设一致
(「State(false) 通常已先行」)。web.rs:1321 的空 Send 只是**唤醒**手段:worker
的 cmd_rx try_recv 消费它后仍会走到 415 行 5ms sleep,退出延迟 ≤5ms,无功能性问题。
登记为顺序确认,非缺陷。

### 维度小结

### panic(unwrap/expect/索引/溢出)
- rtt.rs 本体无 unwrap/expect,除 211 行 `format!` 无 panic 源。整数运算核查:
  345 行 `n as usize * 3`(n ≤ 4096,c_int 上限 2^31-1 → 3GiB 容量,32 位才可能溢出,
  项目 x86_64,安全);312 行 `as u64` 无溢出;126 行 `bytes.len() * 3 + 4`(len ≤ 4096,
  且是 `unwrap_or` 的备选分支,仅在 max_utf8_buffer_length 返回 None 即 usize 溢出时
  取——4096 长度下 max 永不返回 None,该 fallback 实际不可达,安全)。
- 上游 FFI 的 unwrap(全部 jlink_dll.rs,如 56/65/93/129 行 `self.lib.get(..).unwrap()`)
  属 jlink_dll 文件审计范围,此处登记边界:rtt.rs 所有调用路径若 DLL 缺少对应导出符号
  → release abort 进程,UI 收不到 State(false)(线程直接死,spawn 的闭包
  153-161 行也不执行)——**Exited 丢失,UI 门闩永久关闭**。该失败面由 DLL 符号缺失
  触发(合法 SEGGER 驱动不会缺),判定 P3 登记。
- `cargo clippy --lib` 对 rtt.rs 无警告;release panic="abort" 已核(Cargo.toml:46)。

### 错误传播
- 连接失败:`connect_target` bail(207/256 行)→ spawn 闭包发
  `State(false, "● 错误: {e}")`(155-157)→ web.rs:908-938 落状态栏。链完整。
- 静默吞错面:① `open_ex`/`select_tif`/`set_speed` 返回码丢弃(R2);
  ② `rtt_write` 失败只发 `[发送失败]`(373-375),错误码丢失;
  ③ `go()` 返回码丢弃(R5);④ 所有 `tx.send` 失败忽略——`let _ =` 是 mpsc
  惯用法,接收端 web.rs tick_loop 存活时不会断;tick_loop 断(web.rs:997 `Err(_) => break`)
  后 worker 继续发,消息丢弃,UI 静默——属进程生命周期末端,登记备查。
- `rtt_read` 的 `n > 0 / n == 0 / n < 0` 三分支完整(313/356/360 行),
  `n < 0` 分支清理 RTT STOP + close 后 return,正确。

### 边界
- **UTF-8/多字节截断:正确**。`CharsetDecoder::decode` 以 `last=false` 解码
  (rtt.rs:129),encoding_rs 0.8.35 的 decoder 跨调用持有 pending 半字节;
  `max_utf8_buffer_length`(rtt.rs:123-126,encoding_rs 0.8.35 src/lib.rs:3777 文档
  「current state of the decoder」)按 decoder 当前状态+本块长度预估容量,
  覆盖跨块 pending,容量足够,无静默 OutputFull 丢输出。GBK/UTF-16LE/
  未知标签回落均有单测覆盖(429-462 行)。
- **块尾 pending 缓冲**:worker 侧**无** 512 字符行截断——单行兜底在 UI 侧
  log_model.rs:77 `MAX_LINE_CHARS = 512` 与 181-186 行,worker 只按读块
  (≤4096 字节)发 Block,行语义交给 LogPump。职责边界清晰,worker 侧无
  512 截断属设计(块边界断行),非缺陷。
- 块边界恰好落多字节中间 + 字符集热切换的组合 → R3 丢一个字符,已登记。
- `buf` 4096 固定缓冲(298 行),`rtt_read` 返回 ≤4096,`&buf[..n]` 无越界。

### 并发
- 共享状态:`frame_timeout_ms`/`encoding_index`/`hex_rx` 均 `Arc<Atomic*>`,
  Relaxed 加载(rtt.rs:301/304/320/333/312)——参数类共享,Relaxed 语义正确
  (无依赖关系,仅取值);`stop`/`alive`/`APP_SHUTDOWN` 同为 AtomicBool,
  无竞态窗口。
- `APP_SHUTDOWN` 在**读循环**每 5ms 检测(309 行),但**连接阶段不检测**(R1)。
- `msg_tx` 无界 mpsc(web.rs:584 `mpsc::channel`),worker 发送端不会阻塞;
  UI 侧 tick_loop 10ms 轮询消费(web.rs:867/872),缓冲增长速率受设备输出
  上限约束,长跑无积压风险(登记:无界通道在设备极高速率 + UI 卡死时内存
  无上界,属架构级取舍,不修)。
- worker 与 UI 的 Exited 门闩:spawn 闭包严格「先 alive=false(158)后 Exited(160)」,
  web.rs:968-996 依赖此顺序,协议正确。

### 资源
- JLinkDll 无 Drop(已在别处登记)。rtt.rs 各退出路径核实:
  - connect 失败 select 分支:206 行 close ✓
  - connect 失败主路径:255 行 close ✓
  - 成功 → 读循环 `n < 0` 分支:365-366 RTT STOP + close ✓
  - 读循环正常退出(stop/APP_SHUTDOWN):419-420 RTT STOP + close ✓
  - 连接阶段 panic/abort:DLL 句柄不 close——但 abort 走内核收尸(gui.rs:190-233
    TerminateProcess 注释),进程级兜底覆盖。
- 结论:所有**正常**退出路径均释放;异常退出靠进程终止兜底。J-Link 驱动对
  进程强杀的处理(USB 会话清理)不确定,需人工确认,登记。

### 单测状态

`src/rtt.rs` 有 `mod tests`(424-473),5 条,`cargo test --lib rtt` 全过:
| 用例 | 评价 |
|---|---|
| gbk_two_byte_chars_split_across_chunks(429-436) | 有效:GBK 双字节切两半逐块解码,断言两块拼接还原 |
| utf16le_surrogate_split_across_chunks(439-445) | 有效:UTF-16LE 代理对切半,覆盖 surrogate 边界 |
| utf8_multibyte_split_matches_old_carry_behavior(448-456) | 有效:3 字节字符逐字节喂,断言增量续接 |
| unknown_label_falls_back_to_utf8(459-462) | 有效:for_label 回落路径 |
| encodings_table_labels_are_all_resolvable(465-472) | 有效:ENCODINGS 表 5 标签均可解析 |

判定:5 条全部**有效**,无弱测(无恒真断言)、无假绿(断言均对解码产物本身)。
覆盖缺口(登记,不补):帧判定逻辑(last_rx/frame_open 状态机,313-359 行)
无单测——该逻辑与 DLL 调用交织,抽纯函数成本高,属未覆盖面;`enc_label`
越界回落仅被 for_label 测试间接覆盖。

### 阻塞清单
(无:3 轮内未失败任何事项)

### 发现统计

| 级别 | 条数 | 清单 |
|---|---|---|
| P0 | 0 | — |
| P1 | 1 | R1 连接阶段不检测 stop/APP_SHUTDOWN,阻塞窗口 >3s 看门狗先杀进程 |
| P2 | 2 | R2 open_ex/select_tif/set_speed 返回码丢弃;R3 热切换丢 pending 半字节 |
| P3 | 3 | R4 帧判定 5ms 滞后(声明过的取舍);R5 Reset 分支 go()/rtt_control 返回码丢弃;R6 State(false)→Exited 顺序确认(非缺陷) |

**行号口径说明**:本文件引用为 HEAD `ecc5294` 行号;工作树与 HEAD 对 rtt.rs 无差异(`git status` 仅 `?? docs/audit/`),引用可直接对照。

**主会话 S5 抽查(2026-09-25)**:抽 `src/rtt.rs:237`(`exec_command(&format!("Device = {chip}"))`——一致)、`:318-324`(字符集热切换热重建 decoder——一致)、`:418-421`(清理 RTT STOP+close+State(false)——一致)、`:243`(`let mut rc = jlink.connect();`——一致,报告 R1 引用块 237-253 覆盖)。抽查 4/4 通过,无坏条。
---

## src/log_model.rs(670 行,日志管线核心)


- 对象:`src/log_model.rs`(工作树,git HEAD `ecc5294d4760c0079c35e9e4521b7d9f72406314`;任务书给的 885a895 不是当前 HEAD,审计以当前工作树为准)
- 审计范围:log_model.rs 全文 1-670 行(常量/函数 1-275,单测 277-670)
- 验证:`CARGO_TARGET_DIR=targetaudit cargo test --lib log_model` → **30 passed; 0 failed**
- 环境确认:Cargo.toml:46 `panic = "abort"`([profile.release],与 AGENTS 记载一致)

## 背景核实(对照引用)

| 项 | 核实结果 | 引用 |
|---|---|---|
| 消费方形态 | 项目已 Web 化:`src/lib.rs:11-19` 模块树为 ansi/config/demo/device_db/gui/jlink_dll/log_model/rtt/web,**无 slint 模块、无旧 Ctx/tick**;`ui/` 下只有 `ui/web/index.html`,无任何 .slint 文件 | `grep -rn slint src/lib.rs Cargo.toml` 无命中 |
| LogPump 消费方 | 唯一消费方 `src/web.rs` 数据泵线程:`web.rs:870` `let mut pump = shared.pump.lock().unwrap();` … `web.rs:1000-1002` `pump.enforce_line_cap(); let dropped = pump.take_dropped(); if let Some(rows) = pump.take_new_rows()`;另有 api 侧 `push_colored_line`(`web.rs:280,646,914,1591`)、`clear`(`web.rs:1601`)、`paused`(`web.rs:1596`)、`rows_len`(`web.rs:1026,1112`)、`snapshot_rows`(`web.rs:1617,1648,1668`) | 见左 |
| 泵周期 | `web.rs:867` `std::thread::sleep(Duration::from_millis(10));` —— 10ms 是**硬编码字面量**,不引用 FLUSH_MS | 见左 |
| 前端 dropped/trim 契约 | `ui/web/index.html:550-557` rows 事件先按 `m.dropped` 从头部删 DOM 行再 append,随后 `trim()` 按 `rowCap`(初值 500,`index.html:493`;由 stats.cap 同步,`index.html:571`)裁剪;与 pump 的 500 上限同口径 | 见左 |
| seq/clear 水位 | `web.rs:1003` `shared.seq.fetch_add(1)+1`;`web.rs:1600-1607` api_clear 先 `clear()` 再 `clear_seq.store(seq)`;前端 `index.html:551` `if (m.seq <= clearSeq) return;`。tick 内 `take_new_rows`+发 Rows 事件全程持 pump 锁(`web.rs:870-1013`),api_clear 也需同一把泵锁 → clear 与 rows 事件不会交错,水位语义成立 | 见左 |
| ansi::Run 契约 | `src/ansi.rs:11-17` `Run{ text: String, fg: Option<(u8,u8,u8)> }`,fg 经 `serialize_fg` 序列化为 `"#rrggbb"`/null;`feed_line`(`ansi.rs:174-180`)逐字节喂 vte 后 flush。log_model 对 Run 的假设(字段只有 text/fg、fg 可复制、空行可为空 vec)与实现一致 | 见左 |
| 编码失败降级 | 在 worker 侧而非本文件:`src/rtt.rs:121-129` CharsetDecoder 用预分配容量 `decode_to_string`(encoding_rs 静默丢输出的坑已在 rtt.rs 处理);本文件收到的一律是合法 `&str`,无错误传播路径需审 | 见左 |
| 默认断帧超时 | `web.rs:568-572` `saved.frame_timeout…parse::<u32>().ok().map(|v| v.clamp(1, 200)).unwrap_or(20);` —— **硬编码 20**,不引用 DEFAULT_FRAME_TIMEOUT_MS | 见左 |

### 发现清单

### [P0] (无)
panic 路径、数据损坏、锁序问题均未见。panic 面逐一核查:本文件无 `unwrap/expect/[i]` 索引(`char_width_cols` 用 `unwrap_or(1).max(1)`,`log_model.rs:22`);`enforce_line_cap` 按 `Vec<char>` 的 `split_off(MAX_LINE_CHARS)` 切分(`log_model.rs:184-185`),字符边界天然安全;`take_new_rows` 的 `rows.len() - MAX_LOG_ROWS` 有 `>` 前置判断(`log_model.rs:222-224`),无 underflow;`wrap_runs` 的 `cur_w += w` 上界 cols≤512+3,无溢出。ScrollGeom 的 `line_h=0` 会 panic,但该结构无生产消费方,见 P3-2。

### [P1] (无)

### [P2-1] 单测 `pump_clear_resets_everything` 太弱,守不住「pending 残留」回归
- `src/log_model.rs:422-429`:
```rust
#[test]
fn pump_clear_resets_everything() {
    let mut pump = LogPump::default();
    pump.absorb_text("a\r\n", 0);          // "a" 已成行,new_lines/pending 都已空
    assert!(pump.take_new_rows().is_some());
    pump.clear();
    assert!(pump.take_new_rows().is_none());
}
```
- 分析:clear 前数据已全部成行,pending/new_lines 本来就是空——**若有人删掉 `clear()` 里的 `self.pending.clear()`(log_model.rs:268)或 `self.new_lines.clear()`,此测试不失败**。AGENTS「已知未修/取舍」多次强调 pending 半行缓冲残留是最易回归点。补法:clear 前 `absorb_text("partial", 0)`(不带 \n,进 pending),clear 后 `absorb_text("more\n", 0)` 并断言行内容恰为 `"more"` 而非 `"partialmore"`。
- 触发条件:回归 `clear()` 实现时静默通过。
- 判定:单测缺口(弱),不影响当前正确性。

### [P2-2] 无测试覆盖「frame-end 产出的行可超 512 字符」(512 兜底可被绕过)
- `src/log_model.rs:170-179`:
```rust
pub fn absorb_frame_end(&mut self, rx_ending: i32) {
    if self.pending.is_empty() { return; }
    split_lines(&mut self.pending, rx_ending, &mut self.new_lines);
    if !self.pending.is_empty() {
        self.new_lines.push(std::mem::take(&mut self.pending));  // 无长度检查
    }
}
```
- 分析:`enforce_line_cap`(`log_model.rs:182-191`)只裁 **pending**;`absorb_frame_end` 把 pending 整段直接推 new_lines,**单块 >512 字符且无行尾时产出的行不受 512 约束**——而 AGENTS 对「512 字符单行兜底」的表述暗示所有行都有此上限。当前 Web 前端 `#log .row { white-space: pre-wrap; word-break: break-all; }`(`ui/web/index.html:134`)长行会自然折行,无显示损坏;但「行=一个视觉行」的隐式契约(如未来做行级搜索高亮定位、导出)会失配。触发条件:worker 单块读到 >512 字符、设备不发换行、auto_frame 开着。
- 判定:登记 P2(行为与文档不符),是否修由用户定(AGENTS 纪律:不顺手修)。建议补一条锁定行为的测试(513+ 字符无行尾 + frame_end → 单行 >512),让口径显式化。

### [P3-1] `FLUSH_MS` 死代码(消费方已换成硬编码 10ms)
- `src/log_model.rs:73` `pub const FLUSH_MS: u64 = 10;` —— `grep -rn FLUSH_MS src/` 仅此一处(定义自身)。泵周期在 `src/web.rs:867` 直接写 `Duration::from_millis(10)`。头注释 `log_model.rs:3-4`「web.rs 的数据泵线程 tick_loop 每个 10ms 周期」仍是事实,但常量与实现脱钩:改常量不会改周期。
- 判定:死代码登记 P3(任务书指定口径)。

### [P3-2] `ScrollGeom` 死代码,且含一个 line_h=0 的潜在 panic 路径(不可达)
- `src/log_model.rs:98-99` 头注释自认:「UI 迁移 Web 后**暂无消费方**,仅单测锁定语义」;`grep -rn ScrollGeom src/` 仅命中定义(106/112)与单测(563/564/614)。前端滚动是浏览器原生滚动条(`ui/web/index.html:541-544`),无行格量化逻辑。
- 附带核查:`line_h = 0.0` 时 `grid_max()` 返回 NaN(`log_model.rs:119`),`scroll_by` 里 `u.clamp(0.0, grid_max)` 因 `f64::clamp` 断言 `low <= high` 而 panic(release=abort)。因无消费方,生产不可达;现有 5 个单测也未覆盖 line_h=0。
- 判定:死代码登记 P3(与 FLUSH_MS 同列 backlog;任务书「去留见 backlog/审计提案」维持原口径)。

### [P3-3] `DEFAULT_FRAME_TIMEOUT_MS` 死代码,20ms 默认值在两处硬编码
- `src/log_model.rs:75` `pub const DEFAULT_FRAME_TIMEOUT_MS: u32 = 20;` —— `grep -rn DEFAULT_FRAME_TIMEOUT_MS src/ tests/ examples/` 仅命中定义。实际默认值硬编码在 `src/web.rs:572` `.unwrap_or(20)` 与 `src/config.rs:81` `frame_timeout: "20".into()`。三处(定义+两个硬编码)将来改默认值要同步三处。
- 判定:死代码登记 P3。

### [P3-4] 换行子系统 `wrap_runs/char_width_cols/set_wrap_cols/split_lines(pub)` 整体无生产消费方
- `grep -rn "set_wrap_cols\|char_width_cols\|split_lines\|wrap_runs" src/ tests/ examples/ | grep -v log_model` → 0 命中。`set_wrap_cols` 全仓无调用(`log_model.rs:238`),`wrap_cols` 恒 0 → `take_new_rows` 的 wrap 分支(`log_model.rs:215-220`)永不执行,`wrap_runs` 仅被单测调用。任务书背景「换行/复制/搜索共用 char_width_cols 单一真源」描述的是 Slint 时代现状;当前 Web 前端换行交给 CSS pre-wrap(`index.html:134`),服务端不再按列切行。
- 判定:死代码登记 P3(其中 5 个 wrap 单测+emoji 单测仍有效锁定语义,去留随 backlog 决策)。

### [P3-5] 死代码内的正确性欠账:emoji 3 列区段覆盖不全 + ZWJ/组合符按 1 列高估
- `src/log_model.rs:17-24`:
```rust
if (0x1F000..=0x1FAFF).contains(&u) || (0x2600..=0x27BF).contains(&u) || u == 0x2764 {
    3
} else {
    UnicodeWidthChar::width(c).unwrap_or(1).max(1)
}
```
- 分析(已对照 `~/.cargo/registry/src/.../unicode-width-0.2.2/src/tables.rs` 与 lib.rs 文档:`width()` 对控制字符返回 `None`,其余按 East Asian Width):
  1. **低估方向(危险方向)的缺口**:U+231A-23F3(⌚⏰⏱)、U+2B05-2B07、U+2B50(⭐)、U+2B55、U+24C2 等 emoji 呈现字形不在 3 列区段内,unicode-width 记 2(宽字符),fallback 字体实际 ~3 列 → 按此切行会溢出 1 列/个。AGENTS 记载的「实测溢出」若换成这些字形会复现(现因 P3-4 死代码不可达)。
  2. **高估方向(安全但浪费)**:ZWJ U+200D、VS16 U+FE0F、组合附点 unicode-width 返回 `Some(0)`,被 `.max(1)` 抬到 1 列——emoji 序列(如 👨‍👩‍👧 = 3×3+2×1=11 列,实际渲染 3 列)会提前折行,且 `wrap_runs` 可能把 ZWJ 序列从中间切开,续行渲染成独立 emoji。
  3. `wrap_runs` 无最小列数保护:cols=1 时单个 CJK(2 列)会产出 2 列宽的行(`log_model.rs:38` 条件 `cur_w > 0` 放行首个超宽字符);`set_wrap_cols` 只 clamp 上限 512 不设下限(`log_model.rs:239`)。
- 判定:全部在死代码内,P3 登记;若将来恢复 wrap 功能,三项都要先修。

### [P3-6] split_lines 自动模式:Lone CR 只「trim 尾部」,行中 CR 泄漏进行内容
- `src/log_model.rs:82-96`:
```rust
let pat: &str = match rx_ending { 1 => "\r\n", 2 => "\n", 3 => "\r", 4 => return, _ => "\n" };
while let Some(pos) = p.find(pat) {
    let line: String = p.drain(..pos + pat.len()).collect();
    out.push(line.trim_end_matches(['\r', '\n']).to_string());
```
- 分析:自动模式(pat=`\n`)下行内 lone CR(如设备发 `a\rb\n`)不在 `trim_end_matches` 范围内 → 行内容含 `\r`。头注释 `log_model.rs:82` 称「0=自动(\n 断行、吞 \r)」,实际只吞**行尾**的 \r。\n 前端 `white-space: pre-wrap` 中 CR 是换行字符,该行会渲染成两行,与「行=一条记录」口径失配;搜索/导出的行全文也含裸 CR。触发条件:流里混入 LF 之外的 CR(老旧设备/串口 1 线协议)。另有对称情形:CRLF 模式(pat=`\r\n`)下 lone LF 不被认作行尾,行会一直长到 512 兜底——这是「模式只决定在哪断行」的既定语义,但两种混流场景均**无单测覆盖**(split_lines 的 5 个单测全是纯流)。
- 判定:P3(边界行为登记 + 建议补 2 条混流单测锁定现状口径)。

### [P3-7] 同 tick 内 marks 恒排在 data 行之前,用户动作与设备数据可能逆序
- `src/log_model.rs:210-213`:
```rust
let mut fresh: Vec<Vec<Run>> = std::mem::take(&mut self.marks);
for l in self.new_lines.drain(..) {
    fresh.push(self.ansi.feed_line(&l));
}
```
- 分析:`take_new_rows` 先把积压 marks 全部排出,再排 new_lines。若数据行已进 `new_lines`(上一 tick 收到、尚未 take),此时用户在浏览器点「标记」(`web.rs:1591` push_colored_line),下一次 take 产出顺序是 标记→数据——时间上数据先到,显示上标记在前。窗口为 ~10ms,且单测 `marks_merge_with_data_rows_in_order`(`log_model.rs:445-454`)锁定的正是这个顺序。桌面版 Slint 时代是同款实现(AGENTS 记载 marks 同队列),行为一致。
- 判定:P3(显示顺序瑕疵,登记不修)。

### [P3-8] `marks_survive_pause_and_clear_wipes_them` 半假绿:LogPump 本身没有 pause 逻辑
- `src/log_model.rs:469-478`:
```rust
let mut pump = LogPump { paused: true, ..Default::default() };
pump.push_colored_line("mark", (1, 2, 3));
assert!(pump.take_new_rows().is_some()); // 暂停只丢数据,不丢标记
```
- 分析:`paused` 字段(`log_model.rs:158`)在 LogPump 的任何方法里都**不读写**,消费方是 `web.rs:874/891/896` 泵线程(`if !pump.paused`)。此测试对 pump 而言等价于「push 后 take 有值」——pause 语义不在本模块,测试名与注释「暂停只丢数据」夸大了覆盖范围,易误导后续维护者以为 pause 行为有单测守护。
- 判定:P3(单测质量问题,与 P2-1 同源:命名超出实际断言)。

### [P3-9] emoji 单测的宽度不变量是「同一把尺子」自洽校验,不构成渲染正确性证明
- `src/log_model.rs:665-668`:
```rust
for r in pump.take_new_rows().unwrap() {
    let w: usize = joined(&r).chars().map(char_width_cols).sum();
    assert!(w <= 20, "emoji 行宽 {w} 超过 20 列");
}
```
- 分析:断言用 `char_width_cols` 度量、切分也用 `char_width_cols`(`log_model.rs:37`),是恒真自洽校验——能锁定「不超列」的算术,但锁不住「char_width_cols 的估值与真实渲染 advance 一致」(正是 P3-5 的缺口)。CJK 版 `pump_cjk_overlong_stream_caps_then_wraps_within_cols`(`log_model.rs:635-640`)用 unicode-width 度量,同理。
- 判定:P3(测试有效性说明,非缺陷)。

### 单测质量表(mod tests,30 条,全过)

| # | 测试 | 判定 | 理由 |
|---|---|---|---|
| 1 | split_auto_breaks_on_lf_and_eats_cr | 有效 | helper `lines`(285 行)额外断言所有行不含 \r/\n,是好的不变量;余量 rest 断言精确 |
| 2 | split_crlf_mode | 有效 | 输出+rest 双断言 |
| 3 | split_lf_mode_trims_cr | 有效 | 覆盖 LF 模式吞尾部 \r(CRLF 输入) |
| 4 | split_cr_mode | 有效 | 覆盖 CR 单断行 |
| 5 | split_none_keeps_buffer | 有效 | 覆盖 rx_ending=4 不切 |
| 6 | pump_accumulates_blocks_into_rows | 有效 | 半行 pending 语义 + frame_end 成行,断言精确到行文本 |
| 7 | pump_frame_end_flushes_partial_line | 有效 | 无行尾强制 flush |
| 8 | pump_line_cap_splits_overlong_stream | 有效(偏弱) | 主断言 512 字符精确;尾部断言 `joined.ends_with("xxxxxxxxxx")`(365 行)理论上 10 个以上 x 也过,但结合行数断言可接受 |
| 9 | pump_trims_to_max_rows_dropping_oldest | 有效 | 500 窗口 + fresh 含首尾行双断言;dropped 计数由 #10 覆盖 |
| 10 | pump_take_dropped_tracks_trimmed_rows | 有效 | 5 → 0 → 1 三步,锁定「取一次归零」契约 |
| 11 | pump_preserves_ansi_escapes_for_parser | 有效 | 精确断言 ansi 模块 BASIC16 色值 (0x0d,0xbc,0x79),跨模块契约锁定 |
| 12 | pump_color_state_carries_between_rows | 有效 | 跨行颜色延续 |
| 13 | pump_clear_resets_everything | **弱(P2-1)** | clear 前状态本已空,删 pending.clear() 也照过 |
| 14 | push_colored_line_flows_through_take_new_rows | 有效 | 文本+fg 双断言,末次 None |
| 15 | marks_merge_with_data_rows_in_order | 有效(锁定了 P3-7 的逆序行为) | 断言即现状,回归时能抓住行为变化 |
| 16 | marks_count_toward_row_cap | 有效 | fresh 全量返回 + 窗口 500 + dropped=3 三断言 |
| 17 | marks_survive_pause_and_clear_wipes_them | **半假绿(P3-8)** | pump 无 pause 逻辑,断言的是显而易见的 push→take |
| 18 | wrap_runs_splits_ascii_by_columns | 有效 | 边界 10%4 三段精确 |
| 19 | wrap_runs_counts_cjk_as_two_columns | 有效 | 4 列切 2 字 |
| 20 | wrap_runs_carries_color_into_continuation | 有效 | 续行 fg 继承 |
| 21 | wrap_runs_keeps_empty_line | 有效 | 空输入保留空行 |
| 22 | pump_rewrap_reflows_all_rows | 有效 | 含「同值不重排」「0 不重排既有行」两条边界 |
| 23 | pump_wraps_new_lines_at_current_cols | 有效 | 增量行按当前 cols 切 |
| 24 | scroll_lands_bottom_pixel_exact_not_on_grid | 有效 | 2316 vs 2304 的像素精确贴底,复刻真机 bug |
| 25 | scroll_up_from_bottom_snaps_to_grid | 有效 | 落格 + raw 不消化攒量(2252 精确断言) |
| 26 | scroll_down_from_grid_returns_pixel_bottom | 有效 | 往返同一终点 |
| 27 | scroll_clamps_at_top_and_syncs_raw | 有效 | 撞顶 raw 对齐 0 |
| 28 | scroll_short_content_is_always_follow | 有效 | 不足一屏恒 follow |
| 29 | pump_cjk_overlong_stream_caps_then_wraps_within_cols | 有效(度量尺见 P3-9) | cap→wrap 全链 + 每行宽度不变量 |
| 30 | emoji_counts_three_cols_so_wrapped_lines_never_overflow | 有效(自洽校验,P3-5/P3-9) | 区段断言 + 全 emoji 流不变量 |

**总评**:split/pump/wrap/scroll 主路径覆盖扎实(断言精确到值),弱项集中在 clear 残留(P2-1)、pause 假绿(P3-8)、宽度度量自洽(P3-9),以及 4 个空白:混流行尾(P3-6)、frame-end 超 512(P2-2)、marks 逆序仅锁现状、ScrollGeom line_h=0(死代码,不必补)。

### 维度小结

**panic**:未见。无 unwrap/expect/裸索引;`enforce_line_cap` 按 char 切分字符边界安全;`take_new_rows` 减法有前置判断;`char_width_cols` 用 `unwrap_or(1).max(1)`。唯一潜在 panic(ScrollGeom line_h=0 → clamp 断言)在无消费方的死代码内,不可达。release `panic = "abort"`(Cargo.toml:46)放大了任何未来误入 panic 的后果,保持本文件零 panic 面有价值。

**错误传播**:本文件无错误类型——输入恒为合法 `&str`,编码失败在 worker 侧降级(`src/rtt.rs:121-129` 预分配容量规避 encoding_rs OutputFull 静默丢输出);ANSI 未知序列由 vte 状态机吞掉(ansi.rs `non_sgr_csi_ignored` 测试)。log_model 本身无降级分支,无问题。

**边界**:① 512 兜底按 `Vec<char>::split_off` 切,多字节字符跨边界安全(单测 29 用 1000 个汉字覆盖);② 512 兜底可被 frame-end 路径绕过(P2-2,当前 Web 前端无显示损坏);③ 行尾符归属正确:所有模式行内容不含行尾符,helper 断言全局锁定;④ 自动模式行中 lone CR 泄漏(P3-6);⑤ emoji 3 列区段覆盖不全(P3-5,死代码内)。

**并发**:LogPump 自身是普通 `&mut` 结构,无内部同步——线程安全前提由 `Mutex<LogPump>`(web.rs:197)保证,已核实全部读写点均在锁内(泵线程 870-1013;api_mark/pause/clear/export/snapshot 各自短锁)。锁序核查:泵线程持泵锁时会依次拿 status_text/cmd_tx(worker 分支,972-995);`send_payload`(web.rs:260-287)先取 cmd_tx、**语句结束即释放**,再取泵锁,无嵌套持锁 → 无死锁。AGENTS「持 borrow 期间不回调 self」的桌面版纪律在 Web 版对应「持泵锁期间不再碰共享 RefCell/Mutex 之外」——State 分支(908-939)符合,只用已持锁的 `pump` 和独立锁。

**数据一致性**:① `dropped` 计数与前端 DOM 删除一致:前端按 `m.dropped` 从头部删(`index.html:552-556`)+ trim 到 rowCap(500,stats.cap 同源),与服务端 `rows` 窗口同口径;广播 Lagged 时整快照重同步兜底;② clear 后 pending/new_lines/marks/dropped/ansi 状态全清(267-274),前端 cleared 水位 + resetRows 双保险,无残留;③ seq 单调递增且 Rows 事件在泵锁内原子产出,clear 水位(1600-1608)与 Rows 不会交错,`m.seq <= clearSeq` 过滤正确;④ marks 与数据行同队列同上限裁剪(测试 16),唯显示顺序 marks 恒在前(P3-7);⑤ seq 语义:服务端 seq 仅用于 cleared 水位过滤,不表达「第几行」,前端未用它做行号,无歧义。

### 阻塞清单
无(30 条单测一次通过;registry 源码核查 unicode-width 0.2.2 成功)。

**主会话 S5 抽查(2026-09-25)**:抽 4 条——① 死代码三连(FLUSH_MS 73 / DEFAULT_FRAME_TIMEOUT_MS 75 / set_wrap_cols·char_width_cols·ScrollGeom 全仓 grep 仅 log_model.rs 自身+其单测命中,web.rs:867 硬编码 `from_millis(10)`)——与报告一致;② `absorb_frame_end` 170-179(无长度检查整段推 new_lines)——一致;③ `pump_clear_resets_everything` 422-430(断言确如"删 pending.clear() 也照过")——一致;④ marks 恒先排 210-213——一致。抽查 4/4 通过,无坏条。

## src/config.rs + src/ansi.rs + src/demo.rs + src/device_db.rs(四文件合审)

- 仓库:`C:\Users\liuyu\Desktop\WorkPlace\mini-rtt-viewer` @ `ecc5294`(工作树干净,除 `docs/audit/` 未跟踪)
- 审计对象(当前工作树,行号为本次读取时行号):
  - `src/config.rs` 263 行
  - `src/ansi.rs` 276 行
  - `src/demo.rs` 195 行
  - `src/device_db.rs` 121 行
- 环境事实:
  - `Cargo.toml:39-46` `[profile.release]` 含 `panic = "abort"`(仅 release;debug 仍 panic 展开)
  - 实际编译入口已迁到 `src/web.rs`(axum 服务,`src/gui.rs` 桌面壳),**AGENTS.md 的 `main.rs Ctx::tick` 描述已过时**——本文件头注释 `config.rs:7` "保存时机由 web.rs 数据泵线程 tick_loop 节流"与 `config.rs:9` "gui.rs 模块头「跳过的清理与副作用」有说明"相互矛盾:实际**不做退出补写**的是 web 数据泵(web.rs:1032-1037 只在 500ms 统计节流内落盘;进程被强退时最后 ≤500ms 改动丢失),注释自述与实现一致但两处文档指向不同文件
  - `vte = "0.13"`(Cargo.toml:24),实际源码 `vte-0.13.1`(registry 实测),`Params` 子参数上限 `MAX_PARAMS: usize = 32`(`params.rs:8`)

### 各文件审计范围

| 文件 | 行号范围 | 审计点 |
|---|---|---|
| config.rs | 109-116 | `prefs_path()` RTT_PREFS_FILE 分支 |
| config.rs | 119-140 | `load()`/`load_from()` 容错链 |
| config.rs | 143-153 | `save_to()` 原子写 tmp→remove→rename |
| ansi.rs | 11-26 | `Run`/`serialize_fg` WS 契约 |
| ansi.rs | 49-64 | `palette256` |
| ansi.rs | 95-160 | `csi_dispatch` SGR 解析(分号/冒号两式) |
| ansi.rs | 174-189 | `feed_line`/`reset` 持久状态 |
| demo.rs | 45-194 | 虚拟 worker 状态机(自动周期/手动挂起/重置序列/断帧判定) |
| device_db.rs | 21-23, 63-87 | `busy()` 门闩 + `spawn_background` 枚举线程 |

### 发现清单

#### [P2] S1 device_db 的 `busy()` 是死代码,门闩在两条连接路径上均无人检查(核实 rtt 线索)

文件:`src/device_db.rs` + `src/web.rs`

device_db.rs:21-23:
```rust
pub fn busy() -> bool {
    DEVICE_DB_BUSY.load(Ordering::Relaxed)
}
```

**核实现场**:全仓 `grep busy` 只有 `device_db.rs:21` 定义,**无任何调用方**。`web.rs:1238 api_connect` 的连接前检查只有:
```rust
// web.rs:1239-1247
if shared.worker.lock().unwrap()
    .as_ref().is_some_and(|h| h.alive.load(Ordering::Relaxed))
{
    return (StatusCode::CONFLICT, "已有连接").into_response();
}
```
`web.rs` 模块头(web.rs:52-56)声称 "BUSY 门闩……连接前检查",该注释描述的代码已不存在(推测桌面版 `on_connect_clicked` 迁移到 web 时丢失)。

分析:
1. **枚举线程持有 BUSY 期间可 `load()` + `open()` 并发 connect 的窗口客观存在**:`spawn_background`(device_db.rs:68-85)先置 BUSY=true,随后 `JLinkDll::load()`(line 69)与可选 `jlink.open()`(line 74)全程持闩;此窗口内 `api_connect` → `rtt::connect_target` 会并行执行 `JLinkDll::load()`(rtt.rs:192)。
2. **但 TLS 损坏的实际触发面比登记线索更小,判定降级**:
   - 该窗口仅在**缓存未命中**时存在(缓存命中时 load/enumerate 全被跳过,BUSY 置位到清位之间只有 `enumerate_emulators` 的 USB 扫描——纯 `EMU_GetList` 两阶段,与 AGENTS 记载的 GetInfo 1.1 万次风暴不同,风险本身较低,但代码仍置 BUSY)。
   - **两台 J-Link 且枚举线程走到 `open()` 兜底**(device_db.rs:74)时才与 connect 的 `open_ex`/`connect` 构成真并发;单台设备下两个 `Library::load` 同一 DLL + `EMU_GetList` 并发的后果无实测依据,不确定,需人工确认。
   - demo 模式不受影响(web.rs:652 else 分支,demo 不 spawn 枚举线程也不加载 DLL)。
3. `device_names` 消费侧(web.rs:1253-1257 `api_connect`)也不查 BUSY——枚举未回时 `shared.device_names` 为空,`api_connect` 直接拒连("设备库无匹配型号"),反而天然挡住了部分窗口;但若缓存命中路径先发了部分名单(不可能,DeviceNames 只发一次),无此问题。

触发条件:首次启动(无 `%APPDATA%\MiniRttViewer\device_names.txt`)+ 某些 DLL 版本 `DEVICE_GetInfo` 未 Open 前返回空 + 用户在该窗口内点连接。
判定:**登记 [P2](原 rtt 线索 [P0 疑似] 降级)**——门闩机制形同虚设,但真实并发窗口比登记时窄;修复方向是恢复 `api_connect` 对 `device_db::busy()` 的检查或给枚举线程与 worker 共享一个全局互斥,勿靠 BUSY 布尔。

#### [P3] S2 RTT_PREFS_FILE 分支对路径无校验(复验 T1)

文件:`src/config.rs:109-116`
```rust
fn prefs_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RTT_PREFS_FILE") {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    dirs::config_dir().map(|d| d.join("MiniRttViewer").join("prefs.json"))
}
```
复验成立,维持 [P3]:写目标受环境变量控制,`save_to` 的 `create_dir_all`(config.rs:145)会在任意目录建目录并写 `*.json`。实际暴露面仅本地进程自身的环境块(本机多用户下需同用户级提权才有意义),非网络可达;且该变量是测试/便携隔离的**设计用途**(config.rs:107-108 注释)。维持 T1 登记,无升级。

#### [P3] S3 `load_from` 磁盘失败/JSON 损坏静默回默认,无日志无诊断(复验 T1)

文件:`src/config.rs:135-140`
```rust
pub(crate) fn load_from(path: &std::path::Path) -> StoredPrefs {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
```
复验成立,维持 [P3]:非 UTF-8 文件(`read_to_string` 失败)、坏 JSON、字段类型不符(如旧版 `theme: "dark"` 字符串)一律 `default()`,用户无感知丢配置;`save_to` 失败同样静默(config.rs:129 `.is_ok()`)。设计意图(config.rs:3-4 "绝不阻塞启动")与行为一致,属知情取舍,登记即可。

#### [P3] S4 device_names.txt 是前端 innerHTML 注入的信任边界外文件(复验 T1)

文件:`src/device_db.rs:31-41` + `ui/web/index.html:658-661`

device_db.rs:31-41 原样读缓存行:
```rust
fn load_cache() -> Option<Vec<String>> {
    let path = cache_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let names: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    (!names.is_empty()).then_some(names)
}
```
index.html:658-661 消费:
```js
list.innerHTML = opts.map(o =>
  '<div class="opt' + (o.toUpperCase() === cur ? ' current' : '') +
  '" data-v="' + o.replace(/"/g, '&quot;') + '">' + o + '</div>'
).join('');
```
复验成立:候选名在文本节点位置**未转义**(`data-v` 属性只防 `"`),含 `<img onerror=…>` 或 `<` 的缓存行会注入脚本。设备名来自 SEGGER 设备库枚举(jlink_dll.rs:321 `to_string_lossy`),正常不含 HTML;但缓存文件在 `%APPDATA%` 下可被同用户任意进程篡改/预置,构成"本地文件 → 本地 WebView 执行"的注入链(无网络向量)。维持 T1 登记 [P3],属本地信任边界问题。

#### [P3] S5 原子写 rename 失败路径残留 `.tmp`,且 tmp 文件名无 pid/随机熵

文件:`src/config.rs:143-153`
```rust
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
```
- `remove_file` 失败被 `let _` 吞:若旧文件正被其他进程持锁(多实例互斥失效时)或属性只读,`rename` 在 Windows 上拒绝覆盖,**返回 Err 且 `.json.tmp` 永久残留**——下次 `save_to` 覆盖写 tmp 可自愈,但若 tmp 写入成功而 rename 失败,目标文件仍是旧内容(降级为"没保存"),无残留危害,仅磁盘垃圾。
- tmp 名 `prefs.json.tmp` 固定且无 pid:若进程 A 与 B 同指一个 RTT_PREFS_FILE(A 写 tmp 到 rename 之间 B 也写),B 的 rename 会把 A 的 tmp 走掉、A 的 rename 报 FileNotFound——两个写者互踩,最终文件是其中一个的合法内容(无半写,read_to_string 侧仍容错)。单实例场景(web.rs:715 非零码退出机制)下不可达。
判定 [P3]:降级路径正确,残留可自愈;tmp 无唯一后缀在单写者假设下可接受。

#### [P3] S6 ansi.rs 分号式 SGR 参数值 >255 时 `as u8` 静默回绕

文件:`src/ansi.rs:148-149`(分号式 RGB)与 `src/ansi.rs:121`(分号式 256 色走 `palette256` 安全,冒号式 line 124 同样回绕)
```rust
if ok {
    self.fg = Some((rgb[0] as u8, rgb[1] as u8, rgb[2] as u8));
}
```
分析:vte 把 CSI 参数按 `u16` 饱和解析(vte-0.13.1 `lib.rs:353` `saturating_add`、其单测 `767` 行 `params == [[u16::MAX]]` 证明超大参数饱和到 0xFFFF),`as u8` 截断取低 8 位:设备发 `\x1b[38;2;300;10;20m` 得到 (44,10,20) 而非拒绝。仅颜色错误,无越界/panic;终端规范对 >255 参数行为本就未定义。判定 [P3]:行为可接受,如需严谨可改 `min(255)` 或 `u16 as u8` 前置钳制。

#### [P3] S7 device_db 枚举线程无优雅退出,依赖 OS 线程回收;BUSY 清位顺序正确但线程不可 join

文件:`src/device_db.rs:63-87`
```rust
pub fn spawn_background(tx: std::sync::mpsc::Sender<DbResult>) {
    std::thread::spawn(move || {
        if let Some(names) = load_cache() { ... }
        DEVICE_DB_BUSY.store(true, Ordering::Relaxed);
        if let Ok(jlink) = JLinkDll::load() { ... }
        DEVICE_DB_BUSY.store(false, Ordering::Relaxed);
    });
}
```
- `JLinkDll` 无 `Drop` 实现(全文件 grep `impl Drop` 无结果),线程退出时 `jlink` 随作用域析构仅释放 `Library` 句柄;`open()` 兜底路径(device_db.rs:74)打开的 DLL 设备会话**不 close**,靠进程退出由 DLL 自身清理。`--no-window` 纯服务形态进程生命周期长,该孤儿 Open 会话在枚举完成到进程退出间持续存在(可能占用调试器资源/锁 J-Link)。
- 缓存命中路径 BUSY 置位到清位窗口内 DLL 已 load(line 69 在 BUSY 之前!)——注意 `JLinkDll::load()`(line 69)发生在 `store(true)`(line 68)之前,即**枚举线程 load DLL 的瞬间 BUSY 尚未置位**,与 connect 并发的第一个窗口(load 期)门闩根本没盖住。判定 [P3]:load 期并发窗口门闩语义有缝,叠加上一条发现——门闩本就没人查,该缝暂时无实际后果;修门闩时一并调整置位顺序。

#### [P3] S8 demo 状态机:`DemoConnect` 在 manual 挂起期间重连不回写断帧窗口

文件:`src/demo.rs:72-83`
```rust
WorkerCmd::DemoConnect => {
    manual = true;
    disconnect_at = None;
    reconnect_at = None;
    reset_at = None;
    if !connected {
        let _ = msg_tx.send(WorkerMsg::State(true, "● 已连接 (demo)".into()));
        connected = true;
        last_rx = Instant::now();
        frame_open = false;
    }
}
```
分析:状态机各迁移路径(自动断连 line 134-141、手动断开 line 84-94、重置 line 95-107)均重置 `frame_open/last_rx` 一致;断连后 `else if frame_open && now.duration_since(last_rx) > to` 补发 FrameEnd(line 186-190)收尾半帧,与真机 worker 同款(rtt.rs:312 同构)。**未发现竞态或状态漂移**。
轻微语义点:demo 发 `WorkerMsg::Block` 而非 `Log` 计入 RX 统计(demo.rs:175-179 注释已自述),而 demo 无 `DeviceInfo` 消息(真机 connect 成功后发,rtt.rs:267-277)——`api_status` 的 device 字段 demo 走 `demo_device_json()` 硬编码(web.rs:1090),与 demo 线程解耦,不构成不一致。demo 线程 `INTERVAL_MS=80` 固定 80ms 发一条,断帧判定 `frame_timeout_ms` 经 `Arc<AtomicU32>` 热生效(demo.rs:157),web.rs:1450 已 `clamp(1, 200)`,demo 侧直接 `.load()` 无 clamp 但唯一写者已钳制,不可达越界。
**判定:demo.rs 本文件未见异常**(以上为已检查项)。

#### [P3] S9 config 模块头注释与 web.rs 实际行为的一处矛盾

文件:`src/config.rs:7-9`
```
//! - 保存时机由 web.rs 数据泵线程 tick_loop 节流(500ms 快照比对,不变不落盘);
//!   进程退出(关窗强退 / Ctrl-C)不做退出补写,最后 ≤500ms 的改动可能不落盘
//!   (gui.rs 模块头「跳过的清理与副作用」有说明)
```
对照 web.rs:862 `last_prefs: Option<StoredPrefs> = None` 起步,web.rs:1032-1037 仅在统计节流窗口内 `config::save(&snap)`,**全 web.rs 无退出补写**(grep 确认),与注释自述一致;矛盾点在括号指向 `gui.rs 模块头`——web 形态的实际清理说明在 web.rs 自身模块头。属文档指错文件,非行为问题。登记 [P3] 文档修正项。

### 单测状态

| 文件 | mod tests | 评价 |
|---|---|---|
| config.rs | 156-261,5 个 | **有效**。`round_trip_preserves_all_fields` 覆盖全部 24 字段往返;`corrupt_or_missing_file_falls_back_to_default` 覆盖坏 JSON/缺失两路;`partial_json_fills_missing_fields_from_default` 覆盖 serde(default);`save_overwrites_existing_file_atomically` 覆盖覆盖写+tmp 不残留;`dirs_config_dir_matches_legacy_appdata` 环境等价性锁定(APPDATA 缺失时静默 return=跳过,可接受)。**盲点**:RTT_PREFS_FILE 覆盖分支(110-113 行)**无测试**(测试用 `tmp_path` 直接调 `load_from/save_to`,绕过了 `prefs_path`)——即 T1 登记的那条分支恰好无覆盖;`save_to` 的 rename 失败/父目录不可建路径无测试(难以注入) |
| ansi.rs | 191-275,8 个 | **有效**。plain/basic/bright/256/RGB/冒号式/跨行状态保持/跨块断序列重组/非 SGR 忽略/reset 全覆盖;`escape_split_across_feed_calls_is_reassembled` 直接锁定"序列被读块切断"核心语义。**盲点**:分号式 RGB 的 `p.len()>=6` 空子参数 base=3 分支(122 行)无独立测试;3 项 >255 回绕行为(P3-5)无测试(属"未定义行为"可不测);`serialize_fg` 无 serde 序列化断言(仅靠类型系统,实际 WS 契约格式无单测) |
| demo.rs | 无 | **无单测**。状态机(自动周期/手动挂起/重置 800ms 序列/断帧 FrameEnd)全依赖集成 `--demo-log` 冒烟,无纯逻辑可测缝(状态机内嵌 `std::thread::spawn` 闭包,不抽状态机纯函数不可单测)。登记:弱,但属设计形态,非"假绿" |
| device_db.rs | 89-120,1 个 | **弱**。仅 `normalize_sorts_case_insensitive_and_dedups` 一个纯函数测试;`load_cache`(非 UTF-8/空文件/空行过滤)、`write_cache`(目录不可建)、`spawn_background` 的 BUSY 置位/清位顺序、缓存命中零 DLL 路径**全部无测试**(DLL 调用难以隔离,load_cache 本可测但未测) |

`cargo test --lib` 全绿(67 通过,0 失败,0.02s);`cargo clippy` 四文件零告警。

### 维度小结

**panic 路径(release `panic = "abort"`,Cargo.toml:44)**
- config.rs / demo.rs / device_db.rs:生产路径无 `unwrap/expect`(unwrap 仅出现在 test 模块与 `map_err` 链,`unwrap_or_default` 是安全默认)。
- ansi.rs:生产路径零 unwrap;`p.first().copied().unwrap_or(0)` 安全。
- **四文件均无索引/算术溢出 panic 面**(ansi 的 `p[base+1..2]` 由 `p.len() >= 5` 前置守卫;line 124 冒号式 `p.len() >= 5` 同样守卫)。**已检查,未见异常。**

**错误传播(静默吞错面)**
- config:`load_from` 三层 `.ok()` 链吞 IO+解析错误(知情设计);`save_to` 的 `remove_file` `let _` 吞(rename 失败残留 tmp);`config::save` 返回 bool 但 web.rs:1035 丢弃返回值——**落盘失败用户无任何反馈面**。
- ansi:vte 解析失败面(非法 UTF-8 字节)走 `perform` 默认忽略,`feed_line` 按 `&str` 入参天然全 UTF-8,**无错误面**。
- demo:`msg_tx.send` 全 `let _`(接收端 tick 线程退出后发送失败静默)——demo 线程与 tick 线程生命周期由进程退出统一收口,可接受。
- device_db:`write_cache` 两连 `let _` 吞建目录/写盘错误,枚举线程对 `JLinkDll::load` 失败整体 `if let Ok` 吞(失败静默=下拉为空,注释 device_db.rs:61 自述"失败静默:下拉为空",知情取舍)。
- 汇总:四文件静默面均为知情设计(降级到"无候选/无配置"),无一处吞掉会导致状态错乱的错误。

**边界(UTF-8 / 原子写 / rename)**
- UTF-8:`load_cache` 用 `read_to_string`,缓存文件含非法 UTF-8 → 整个文件 `.ok()?` 返回 None → 回退 DLL 枚举,无截断(比 `lines()` 逐行 lossy 更保守);`write_cache` 的 `names.join("\n")` 写的是 `to_string_lossy` 后的 lossy 字符串(缓存往返后设备名中的替换字符 U+FFFD 永久化,一次性降级,影响极小)。
- 原子写中途崩溃:tmp 残留(可自愈,见 P3-5);**`remove_file(path)` 成功而 `rename` 失败(如磁盘满)→ 目标文件消失且 tmp 残留**——下一启动 `load_from` 回默认 + 下一次 save 重建,无数据损坏,属"丢一次保存"级别。
- Windows rename 不覆盖:代码已用 remove+rename 正确应对(config.rs:150 注释),remove 失败路径见 P3-5。

**并发**
- BUSY 门闩:置位/清位在同一线程内成对(无中途 return 跳过清位——line 85 在闭包尾,所有路径都到),但**语义已空转(busy() 零调用方,P2-1)**,且 `JLinkDll::load` 在置位之前(P3-6),双重失效。demo 线程与 worker 的竞态:demo 模式下 worker 从不 spawn(web.rs:1295 demo 分支),两者互斥由 `demo_mode` 常量保证,无竞态;demo 状态机单线程内无共享可变,`APP_SHUTDOWN` 用 Relaxed 读(演示线程,无顺序需求),**已检查,未见异常。**
- device_db 枚举线程与 web 请求线程:`device_names`/`jlinks` 经 Mutex 传递,`api_connect` 与枚举线程并发读 `device_names` 是"读到部分名单还是空名单"的一致性窗口(缓存命中时先发 DeviceNames 再发 Emulators,web.rs:658-666 按序入队,无乱序),无锁问题。

**资源**
- 临时文件:`prefs.json.tmp` 残留(可自愈,P3-5);`device_names.txt` 无句柄泄漏(`std::fs::write` 即开即关)。
- 枚举线程生命周期:**启动一次**(web.rs:655 `serve` 内 spawn,非每次连接重发;rtt.rs:197 worker 连接时自己重发 `enumerate_emulators` 刷新下拉,device_db 线程不重复)——与 AGENTS.md "worker 每次连接时重发" 一致,device_db 线程本身一次性。
- **`open()` 兜底路径(仅缓存未命中+DLL 版本需 Open 才读设备库)打开的 DLL 会话不 close,孤儿存活到进程退出**(P3-6 已述);该路径在 SEGGER 官方 DLL 上 `DEVICE_GetInfo` 不需 Open(原 pylink 同款),实际触发概率低,登记即可。
- demo 线程:随 `APP_SHUTDOWN` break 退出,无泄漏;`T0: OnceLock` 只 set 一次,重复 spawn(测试多进程下各自进程)无冲突。

### 阻塞清单

无(四文件全部读全,结论均有引用支撑;DLL 并发 TLS 损坏的真实触发面因需多设备真机,标"不确定,需人工确认"已并入 P2-1)。

### 发现统计

| 级别 | 总数 | config.rs | ansi.rs | demo.rs | device_db.rs |
|---|---|---|---|---|---|
| P0 | 0 | 0 | 0 | 0 | 0 |
| P1 | 0 | 0 | 0 | 0 | 0 |
| P2 | 1 | 0 | 0 | 0 | 1(busy 门闩死代码/并发窗口核实) |
| P3 | 6 | 4(RTT_PREFS_FILE、静默丢配置、rename 残留/无 pid、头注释指错文件) | 1(>255 回绕) | 0 | 2(XSS 信任边界、孤儿 Open 会话+BUSY 置位序) |

单测过审结论:config.rs **有效**(RTT_PREFS_FILE 分支盲点)、ansi.rs **有效**、demo.rs **无单测(设计形态,非假绿)**、device_db.rs **弱**(仅 normalize 一个纯函数)。

### 主会话 S5 抽查(2026-09-25)

抽 4 条:① P2-1 `busy()` 零调用方——全仓 `grep -rn busy src/ tests/ examples/` 实测仅 `device_db.rs:21` 定义一行,`web.rs:1238 api_connect` 连接前检查确只有 worker-alive 判断(1239-1247),无 BUSY 检查——与报告一致;② P3-6 ansi.rs:148-149 分号式 RGB `as u8` 回绕点(149 行 `self.fg = Some((rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))`)——一致;③ P3-2 config.rs:109-116 `prefs_path()` RTT_PREFS_FILE 分支原文——一致;④ P3-9 头注释矛盾(config.rs:7-9 指向 gui.rs,web 形态无退出补写)——web.rs:1032-1037 节流落盘与全文件无退出补写与报告一致。抽查 4/4 通过,无坏条。
