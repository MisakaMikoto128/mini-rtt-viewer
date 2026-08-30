# Mini RTT Viewer

轻量级 SEGGER RTT 日志查看器 —— 为 **UTF-8 / emoji** 而生。

![screenshot](docs/screenshot.png)

## 为什么做这个

官方 J-Link RTT Viewer 对 UTF-8 的支持是坏的:中文乱码、emoji 直接丢失。这个项目用 Rust + Slint 重写了核心的"连接 + 看日志 + 发数据"体验:

- **单文件 exe,约 4 MB**,无需安装,拷给同事就能用
- **启动 < 100ms**,没有 Python 运行时、没有 WebView
- **UTF-8 完整支持**,中文 / emoji 原样显示,跨读取块的多字节序列自动拼接
- **实时流畅**:10ms 界面刷新粒度,均匀发送的消息逐条均匀上屏;日志文本上限 6 万字符(超出丢最旧),长时间流式输出不卡 UI
- 无换行符的裸流(如裸 printf 数值)可开启**自动断帧**:相邻数据到达间隔超过设定值(1~200ms,5ms 精度判定)自动换行
- **滚轮逐行对齐**:滚动位置量化到整数行高,连续上翻时行位置稳定;滚离底部失随、滚回底部恢复跟随

## 功能

- 连接设置:目标芯片型号 / SWD / JTAG / 速率 (100–12000 kHz) / RTT 通道 0–15
- 接收:ANSI 转义序列剥离、暂停接收、自动断帧 + 超时可调、断行方式可选(关闭 / 按数据块 / 时间窗)、接收行尾独立设置
- 发送:向所选下行通道发送数据,Enter 快捷发送,行尾可选(CRLF / LF / CR / 无)
- 暗色主题,日志可鼠标拖选 + Ctrl+C 复制
- 无设备演示:`mini-rtt-viewer.exe --demo-log` 启动内置演示数据流(中英混排 + emoji),用于体验滚动/断行/渲染

## 使用前提

- Windows 10/11 x64
- 已安装 [SEGGER J-Link 软件包](https://www.segger.com/downloads/jlink/)(程序运行时加载 `JLink_x64.dll`)
- J-Link 调试器 + 目标板固件已初始化 SEGGER RTT(`SEGGER_RTT` 组件)

芯片型号需填写 J-Link 支持的完整型号名(如 `STM32F030C8`、`STM32H750VB`),与官方 RTT Viewer 中的写法一致。

## 构建

```bat
cargo build --release
cargo test                    :: 日志泵纯逻辑单元测试
build_release.bat   :: 构建 + 可选 UPX 压缩 + 输出到 dist\
```

Rust 1.75+。`tools/` 目录放入 UPX(可选)后脚本会自动压缩。提交前跑 `cargo clippy --all-targets`,当前零警告。

## 发布

推送 tag 自动构建并发布到 GitHub Releases(Windows x64):

```bash
git tag v0.1.0
git push origin v0.1.0
```

CI 配置见 [.github/workflows/release.yml](.github/workflows/release.yml)。

## 技术栈

| 组件 | 选择 | 理由 |
|---|---|---|
| UI | Slint 1.x | 编译期声明式 UI,启动秒开,exe 小 |
| J-Link 访问 | FFI 直调 `JLink_x64.dll` | 与官方工具/驱动共存,不抢 USB(纯 USB 协议实现需要 Zadig 换驱动,会破坏 SEGGER 工具链) |
| 并发模型 | std::thread + mpsc + 10ms 消息泵 | worker 读线程(5ms 轮询)不碰 UI,泵只做断行与文本合并 |

连接时序沿用了经过验证的 J-Link DLL 状态机要求(RTT START 在 connect 之前建立)。

## 代码结构

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 装配:单实例检查、UI 回调、10ms 消息泵调度、退出编排 |
| `src/log_model.rs` | 消息泵纯逻辑(断行/缓冲/截断),有单元测试 |
| `src/rtt.rs` | worker 线程:连接序列、RTT 读循环、断帧判定、UTF-8 增量解码 |
| `src/jlink_dll.rs` | JLinkARM.dll 最小 FFI 绑定 |
| `src/single_instance.rs` | 单实例互斥 |
| `src/demo.rs` | `--demo-log` 演示数据源 + 微秒时序打点 |
| `src/ui/log_view.slint` | 日志滚动区(滚轮行高对齐、贴底跟随) |
| `AGENTS.md` | 实际踩坑经验笔记(改代码前先读) |

## License

[MIT](LICENSE)
