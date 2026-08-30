# Mini RTT Viewer — 项目经验笔记

Rust + Slint + JLinkARM.dll FFI 的极简 RTT 查看器。本文件沉淀实际踩过的坑,
改代码前先扫一遍,别再交一遍学费。

---

## J-Link DLL 连接序列是硬性要求,不能调换

**现象**:单次 `Open → Connect` 的"简洁"序列能连上但 RTT 永远没数据;连接出错时程序"永远连接中"。

**原因**:JLinkARM.dll 内部状态机有两处硬约束:
1. `JLINK_RTTERMINAL_Control(START)` 必须在 `Connect` **之前**调用;
2. 连接出错(设备名错/未供电)时 DLL 会弹**隐藏的模态对话框**等人点确认——worker 看起来就是卡死。

**处理**:
- 序列固定:`disable_dialog_boxes() → (EMU 枚举/选定) → OpenEx → RTT START → TIF_Select → SetSpeed → ExecCommand("Device = …") → Connect`
- **disable_dialog_boxes 必须在 Open 之前**:调试器选择窗、固件升级提示都发生在 Open 内部,open 之后再抑制就晚了
- **多台 J-Link 的选定三件套(缺一不可,实测 V8.24)**:
  1. `JLINKARM_EMU_SelectByUSBSN(sn)`——返回成功、GetSN 也反映选定,**但无参 `JLINKARM_Open` 仍会弹 probe 选择窗,并用弹窗里的选择(默认第一台)覆盖选定**——这正是"下拉选了 888 实际连了 758"的根源
  2. `ExecCommand("SetHostIF USB = <sn>")`——Open 路径认这个选定
  3. 用 **`JLINKARM_OpenEx(null, 0)` 代替 `JLINKARM_Open`**——OpenEx 尊重选定,不再弹窗
  打开后必须读 GetSN 与选定值比对,不符则告警(Open 前读 GetSN 只反映选定值,会误导)
- 首次 connect 失败在 worker 内自动重试 3 次(间隔 400ms),不回退 UI 状态,避免按钮闪烁

参考:`src/rtt.rs` `run()`、`src/jlink_dll.rs` `disable_dialog_boxes`

---

## 多行 TextInput 默认高度只有一行 → 贴底时整个元素滚出视口

**现象**:日志区一片空白,只剩 I 形光标。

**原因**:TextInput 放在普通 Rectangle 里(不包 ScrollView)时,不设 `height` 默认只有一行高。贴底滚动 `y = 8px - offset`(offset 是几千像素的内容高),整个单行高的元素被滚到视口外。

**处理**:`height: self.preferred-height;` 显式设为全文本高度。

参考:`src/ui/log_view.slint` `log-edit`

---

## 滚动行对齐:ScrollView/Flickable 按像素滚,做不到

**现象**:连续滚动时行看起来"上下抖动"、行位置不稳定。

**原因**:ScrollView 内部 Flickable 按像素滚、滚轮步长与行高无关,行永远停在非行高对齐的亚像素位置。

**处理**:自建 `LogView`(见 `log_view.slint`):
- 隐藏探针 TextInput 实测单行行高(`visible:false` 的元素 `preferred-height` 仍可求值;为 0 时退回字号估值)
- 滚轮 delta 累积后 `floor` 量化到整数行高,亚行残余留在累积量里(触摸板细粒度凑满一行才动)
- 手动滚动上界也 floor 到行格;贴底跟随贴死 `max-offset`
- 滚离底部失随(`follow=false`),滚回底部恢复,新数据不拉扯视口

**已知取舍**:贴底位置(内容贴死)与手动滚动的行格位置相差「内容高 mod 行高」的小数余量(<1 行),底部附近来回滚动时底边空隙会变化。尝试过"内容高补齐到整数行"来消除,因底部恒定大空隙观感更差被用户否决——**此问题已知、用户接受,不要再动**。

参考:`src/ui/log_view.slint`

---

## Slint 的 `length / length` 是无单位比值:`floor(x / line-h) * 1px` 会缩小 line-h 倍

**现象**:LogView 上线后用户实测:上滚一格视口瞬移(贴底 offset=max-offset 几千像素,第一次量化后只剩 `max-offset/line-h` 像素);下滚撞底边界后再滚一格**突然弹回顶部**;平时滚动每格只挪 ~3px(亚行距,看起来像"微小抖动")。

**原因**:代码写成了
```slint
u = floor(raw / line-h) * 1px;          // ❌
grid-max = floor(max-offset / line-h) * 1px;   // ❌
```
Slint 里 `length / length` 产出**无单位 float 比值**(探针实测 `floor(9200px / 19px) * 1px == 484px`,不是 9200px)。floor(比值) 是"整数行数",乘 `1px` 得到的是"行数×1px"而不是"行数×行高"。于是:① 手动滚动范围被压缩到真实范围的 1/line-h;② 撞底边界 clamp 后 `raw = grid-max`,下一次同方向事件 `u = floor((grid-max+delta)/line-h)*1px` 远小于 grid-max,offset 直接跳回接近顶部——这正是"滚动过快弹回顶部、断开后仍复现"的根因;③ 每格位移 ≈ delta/line-h px,永远亚行距,行对齐从未真正生效。

**处理**:量化一律乘回行高——`floor(raw / line-h) * root.line-h`(两处:u 与 grid-max)。验证手段:一次性 example 用 `slint::slint!` 宏内联组件,`out property` 承载表达式,从 Rust 打印求值结果(探针保留在 `examples/sem_probe.rs`,下次怀疑单位语义直接跑它)。

**教训**:Slint 长度运算的单位语义靠猜不可靠,涉及 `length/length` 的表达式先写探针实测再上线。

参考:`src/ui/log_view.slint` `scroll-by-px` / `grid-max`

---

## ANSI 彩色日志:解析用 vte,渲染逐行逐段;Slint 没有富文本

**约束**:Slint 的 Text/TextEdit 不支持混色(官方 issue #3728 仍开放),也没有 TextCanvas——彩色日志只能"解析成带色段 → 每段一个 Text"渲染。

**架构**(全部用现成件,不自造解析):
- 解析:`vte` crate(Alacritty/WezTerm 同款状态机),`src/ansi.rs` 持有持久 Parser——**SGR 颜色状态跨行、跨读块保持**(序列被 RTT 块切断也能续上)。SGR 变色时必须先 flush 已累积文本再换色,否则整行并成一段
- 数据:log_model 产出 `Vec<Run{ text, fg }>` 行,按行数(非字符数)裁剪上限;take_new_rows 增量返回新行、take_dropped 返回被挤掉的行数(UI 行模型必须同步从头部 remove,否则无限增长)
- 渲染:行模型 `for` 逐行逐段 Text 着色,**整块容器 `y = 8px - offset` 平移**
- 滚动:自建行格滚动——滚轮 TouchArea 统一量化 offset,行容器 y 随动

**Flickable/ListView 的 viewport-y 不能用绑定驱动(踩过,自动滚动全灭)**:Flickable 内部会自行对 viewport-y 赋值(首帧布局必然发生),而 Slint 里**赋值会永久顶掉外部绑定**——`viewport-y: -offset` 绑上即死,表现为滚轮与自动跟随全部失效。自建滚动一律用"容器 y = -offset"平移模式,不碰 Flickable。

**HorizontalLayout 挤压坑**:各段 Text 必须显式 `width: self.preferred-width; horizontal-stretch: 0` 锁死固有宽度——不锁的话,总宽超出的长行会把中间的段**压缩变形**(位置漂移),`min-width` 锁不住。

**已知取舍**:①行内不换行,超宽行从右侧整体裁切(终端语义);②彩色行模型下失去鼠标选中文本/Ctrl+C 复制(单 TextInput 时代的红利);③背景色/粗体斜体 v1 忽略,只解析前景色(16/256/RGB);④自动滚动关闭且行数触顶后,内容因头部裁剪整体前滑(无滚动锚定,与旧版 6 万字符丢最旧行为同类)。

参考:`src/ansi.rs`、`src/log_model.rs`、`src/ui/log_view.slint`

---

## 设备库枚举与 connect 并发会损坏 DLL TLS,用 BUSY 门闩串行化

**现象**:原 Python 项目的教训记录——pylink `supported_device` 枚举(1.1 万次 `JLINKARM_DEVICE_GetInfo`)若与 connect 在不同线程并发,connect 崩 0x14(DLL TLS 损坏)。

**处理**:mini 版设备下拉候选在启动后台线程枚举:磁盘缓存(`%APPDATA%\MiniRttViewer\device_names.txt`)命中则零 DLL 调用;未命中才加载 DLL 枚举,期间置 `DEVICE_DB_BUSY`,`on_connect_clicked` 检查到 BUSY 直接拒绝连接并提示稍候(窗口只有首次启动无缓存的几秒)。

参考:`src/device_db.rs`、`src/main.rs` `on_connect_clicked`

---

## 自绘弹层会被后续兄弟元素盖住,下拉选择用 std ComboBox 别手搓

**现象**:目标设备选择如果做成"LineEdit + 浮动候选弹层"(自定义 Rectangle 弹在控件下方),弹层会被布局里**后面声明的兄弟区块**(接收设置等)盖住——Slint 无 z-index,后声明者在上层绘制。

**处理**:候选列表用 std `ComboBox` 承载(其弹窗由官方组件在窗口层处理,不受兄弟遮挡)。模式:LineEdit 输入即筛选(`edited` 回调)→ Rust 重筛后 `set_device_names(ModelRc::new(VecModel))` → `current-index` 复位 -1(ComboBoxBase 显式支持 -1=显示空)→ `selected` 回填输入框。筛选重载模型后 ComboBox 的 `changed model` 会自动 clamp current-index,务必在设置完模型后把 index 拨回 -1。

参考:`src/ui/app.slint` 连接设置区、`src/main.rs` `apply_device_filter`

---

## 多行 TextInput 的内置按键行为吃掉 PageUp/PageDown,自定义 key-pressed 拦不到

**现象**:在 TextInput 上覆盖 `key-pressed` 处理 PageUp,不生效(光标移动了、视口没滚)。

**原因**:多行 TextInput 的内置行为先于实例上的自定义 `key-pressed` 执行,直接消费了翻页键;事件也不会冒泡到外层 FocusScope。

**处理**:放弃键盘滚动,滚轮/拖动是主路径。若未来要做键盘滚动,方向是把焦点给 FocusScope 而不是 TextInput(Tab 焦点落在 FocusScope 上时按键直接到它)。

---

## UI 刷新粒度 = 显示节奏:大周期会把均匀消息攒批上屏

**现象**:下位机发送均匀,界面上消息却"两条出来一次"。

**原因**:pump timer 周期 200ms,窗口内到达的 2~3 条攒成一批同时渲染。

**处理**:`FLUSH_MS = 10`(log_model.rs)。空 tick 只有一次 `try_recv` 开销;worker 读循环 5ms 轮询(数据到达延迟 ≤15ms)。`--demo-log` 模式带微秒时间戳(stderr `[tx]`/`[flush]`)可实测验证显示节奏。

参考:`src/log_model.rs`、`src/demo.rs`

---

## 断行/断帧语义(与原 PySide6 项目对齐)

- **按数据块**:worker 每次 `rtt_read` 发一条 `Block`,UI 在块边界断行(= 设备一次发送一行)
- **自动断帧**:worker 读循环里用真实时间戳判定——相邻数据到达间隔超过设定值(1~200ms,默认 20ms)→ 发 `FrameEnd`,UI 把缓冲整段切一行。判定必须在 worker(5ms 精度),放 UI 侧受刷新粒度限制(200ms 时 20ms 设定完全不生效)
- **超时可调**:间隔通过 `Arc<AtomicU32>` 共享,UI 改输入框即时生效,无需重连
- **行尾模式只决定"在哪断行",行内容一律不含行尾符**(曾出现 CRLF/CR 模式行内容带行尾符、与自动/LF 模式不一致)
- **512 字符单行兜底**:注意切分时行内容=前 512 字符、tail 是新 pending;不要把 replace 出来的整个原串当行(尾部会随下一行重复输出——曾踩)

以上逻辑全部收在 `src/log_model.rs` 的 `LogPump`,有单元测试覆盖(断行各模式/合批/帧结束/兜底/截断/清空)。改断行行为先跑 `cargo test`。

---

## 文本尾部的 \n 会渲染成一个空行

Rust 侧曾按"每行追加 `内容+\n`"维护日志文本,结尾的 `\n` 被 TextInput 渲染成空行,贴底时底部垫出一条假空隙。当前版本保留此行为(与"之前的状态"一致,用户接受);若将来要消除,改成行间分隔式追加(文本永不以 `\n` 结尾),注意截断逻辑按 `\n` 找行界仍兼容。

---

## worker 生命周期三条铁律

1. **Exited 门闩**:上一个 worker 线程存活期间(含阻塞在 `connect()` 的 DLL 调用时)严禁 spawn 新 worker——两个 worker 并发抢 J-Link 是数据损坏/状态错乱的根源。UI 收到 `WorkerMsg::Exited` 才允许再次连接。
2. **进度与结果分离**:`Progress` 只刷状态栏文字;`State` 才改变连接标志。混用会让连接按钮闪烁。
3. **退出**:窗口关闭 → `APP_SHUTDOWN` 置位 → 等 worker 退出(3 秒宽限)→ 仍活着就 `process::exit` 强退(宁可粗暴不留僵尸)。

参考:`src/rtt.rs`、`src/main.rs` 尾部

---

## 单实例互斥:libloading 的 get() 会清掉 GetLastError

`CreateMutexW` 后必须**立刻**调 `GetLastError`,中间不能插任何 `libloading::get()`(它会调其他 Win32 API 清掉 last error)。所以先把两个函数指针都取齐,再连续调用。

参考:`src/single_instance.rs`

---

## Slint 其他陷阱速查

- **TextInput 必须显式 `height`**(见上)
- **std-widgets Button 优先**:自定义 TouchArea 按钮存在点击丢失/焦点怪癖;面板不要用 ScrollView 包(内部焦点链与外层断开);SpinBox 内部 FocusScope 困 Tab → 全部用 ComboBox
- **Slint 没有 `self.parent`**:引用宿主组件尺寸用组件根的属性(`root.height`)
- **`visible:false` 元素的属性仍可求值**(行高探针利用了这一点)
- **日志区滚轮**:TouchArea(scroll-event)在下层、TextInput 在上层——TextInput 优先吃鼠标拖选(复制用),滚轮落到 TouchArea 统一量化
- **日志文本全量渲染**:`MAX_LOG_CHARS = 60_000` 上限,超出按行边界丢最旧,防长跑卡 UI;设备输出极快时(<30ms/条)偶发合批是整段重排的成本所致,根治要虚拟化渲染(未做)
- **自动化测试注意**:桌面上有多个同名窗口时,raw 输入注入(滚轮/移动)的 frame 校验会失败——测试实例用 `--demo-log`(跳过单实例互斥),且保证桌面只有一个实例
- **Segoe UI 没有 ▸/▾(U+25B8/25BE)小三角字形** → 渲染成方框乱码("设备信息"前出现长方形)。箭头/三角一律用 WGL4 集合:▼(U+25BC)/▲(U+25B2)/►(U+25BA)/◄(U+25C4)
- **下拉选择一律 std 原生件,不要自绘弹层控件**(自绘 PopupWindow 候选列表被用户两次否决:"丑无法使用")。目标设备的"可输入+下拉"单控件 = `editable_combo.slint` 的覆盖拼合:ComboBox 全宽打底(原生箭头+原生弹层),LineEdit 覆盖左侧文本区(输入即筛选),右侧留 32px 露出原生箭头。限制:std ComboBox 的 popup 无法程序化展开,"输入自动弹出"做不到,点箭头呈现筛选后的候选。改 UI 前先问"上次认可的形态是什么",别在用户没要求时重构控件
- **Bash 工具里 `&` 启动的 GUI 子进程会随命令结束被回收**——冒烟测试用 `powershell Start-Process`(分离启动)才能存活

---

## 多台 J-Link:EMU_GetList 枚举 + SelectByUSBSN 选定(都要在 Open 之前/之外)

- 枚举:`JLINKARM_EMU_GetList(host, pInfo, MaxInfos)` 两段式:先 `(host, NULL, 0)` 拿数量,再传数组填充;host USB=1(pylink JLinkHost.USB)。**无需 Open**(纯 USB 扫描,参考项目连接前预查同款)。结构体 `JLinkConnectInfo` 字段顺序照抄 pylink structs.py(SerialNumber u32 / Connection u8 / … / acProduct[32] / acFWString[112] / aPadding[34]),repr(C) 自然对齐与 ctypes 未打包布局一致
- 显示名:`"{acProduct}: {SerialNumber}"`(如 "J-Link PLUS: 602717758"),产品名空则退 acFWString,再空只显示序列号
- 选定:`JLINKARM_EMU_SelectByUSBSN(sn)` 返回 <0 = 找不到;必须在 `JLINKARM_Open` **之前**调用(pylink open(serial) 内部就是这么做的)
- 刷新时机:启动后台线程一次 + worker 每次连接时重发(用户可能插拔)

参考:`src/jlink_dll.rs` `enumerate_emulators` / `select_by_usb_sn`、`examples/emu_check.rs`(无界面验证)

---

---

## 构建工程

- **cargo target 目录会锁 exe**:运行中的实例锁住 `target/<dir>/release/*.exe`,杀不掉的僵尸进程(如 DLL 卸载卡死)会让该目录永久不可写。换 `CARGO_TARGET_DIR=target2` 等新目录绕开;`.gitignore` 已忽略所有本地 target 变体(target/target2/targetfix/targettestbuild/targetmeasure)
- **build_release.bat**:UPX 压缩;bat 里不能设名为 `UPX` 的环境变量(UPX.exe 会把它当参数读),变量用 `UPXBIN`;bat 文件写英文注释(中文 GBK 乱码)
- **发布**:推 tag `vX.Y.Z` → GitHub Actions(windows-latest 构建 + UPX)→ 自动挂 Release 资产。不要随便发版,先本地验证、用户确认
- **验证工具**:`cargo test`(log_model 纯逻辑);`cargo run --example rtt_check -- [芯片] [秒数]`(无界面 RTT 直读);`--demo-log`(带 UI 的无设备演示)

---

## 模块层次(2026-08 重构后)

- **bin/main.rs 只是装配层**:创建 AppWindow → 把回调接到 `Ctx` 的方法 → timer 调 `ctx.tick()` → 退出编排。**业务规则不写进 main**;新增功能 = 给 `Ctx` 加方法 + 接一行闭包
- **模块树唯一在 lib**:`slint::include_modules!()` 在 lib.rs 展开(AppWindow/LogRow 等生成类型经 `mini_rtt_viewer::` 导出),bin/examples 一律 `use mini_rtt_viewer::…`,**绝不**在 bin 里重复 `mod` 声明——否则同模块编译两份,还容易漏声明(踩过:bin 漏 `mod ansi`)
- 层次:main(装配)→ Ctx(共享状态 + 业务方法)→ log_model/ansi(纯逻辑,可单测)→ rtt(worker 线程,connect_target + rtt_read_loop 两段)→ jlink_dll(FFI)→ device_db(后台枚举)
- 新增 DLL 能力的路径:jlink_dll.rs 加 FFI → rtt.rs 接入序列/循环 → main.rs Ctx 加方法 → app.slint 加控件

---

## 已知未修(用户知情,勿"顺手修")

1. 底边空隙的微小变化(见"滚动行对齐"的取舍说明)
2. 键盘滚动未做(见 TextInput 按键陷阱)
3. 设备极高速率下偶发合批(虚拟化渲染是独立的大工程)
