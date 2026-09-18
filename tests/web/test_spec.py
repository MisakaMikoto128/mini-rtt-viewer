"""浏览器管理台黑盒测试(pytest + Playwright)。

纪律(参照 docs/team/charter.md):预期只来自 docs/product/spec.md 条目,
测试名按条目命名(test_fr<N>_<行为>)。服务以 --demo-log 启动(fixture)。

运行前置:
    pip install pytest playwright
    playwright install chromium
    cargo build --release --locked
"""

import json
import os
import shutil
import subprocess
import tempfile
import time
import urllib.request

import pytest
from playwright.sync_api import sync_playwright

PORT = 18099  # QA 纪律:固定 18099,避开开发常用端口
BASE = f"http://127.0.0.1:{PORT}"
EXE = r"target\release\mini-rtt-viewer.exe"

# UX 新色板 accent #2B6E8F(降饱和钢蓝,2026-09 色板重构;
# getComputedStyle 返回 rgb 形式,2026-09-19 实测确认)
_ACCENT_RGB = "rgb(43, 110, 143)"


@pytest.fixture(scope="session")
def server():
    # --no-window:纯服务模式(不开窗口/托盘,供 playwright 黑盒测试)。
    # APPDATA 指向临时目录:服务与测试环境隔离,不读不写用户真实偏好
    # (面板初值恢复走的 /api/prefs 会回填持久化的 chip,干扰下拉交互用例)
    tmp_appdata = tempfile.mkdtemp(prefix="rtt-test-appdata-")
    env = {
        **os.environ,
        "APPDATA": tmp_appdata,  # FR-21 prefs 走 %APPDATA%,指向临时目录隔离
        "RTT_PREFS_FILE": os.path.join(tmp_appdata, "prefs.json"),  # 纪律:显式 prefs 隔离
        "RTT_WEB_NO_BROWSER": "1",
    }
    proc = subprocess.Popen(
        [EXE, "--demo-log", "--no-window", "--port", str(PORT)], env=env
    )
    for _ in range(50):
        try:
            urllib.request.urlopen(f"{BASE}/api/status", timeout=1)
            break
        except Exception:
            time.sleep(0.3)
    else:
        proc.terminate()
        shutil.rmtree(tmp_appdata, ignore_errors=True)
        pytest.fail("mini-rtt-viewer 未在 15s 内就绪")
    yield proc
    proc.terminate()
    shutil.rmtree(tmp_appdata, ignore_errors=True)


@pytest.fixture(scope="session")
def page(server):
    with sync_playwright() as p:
        browser = p.chromium.launch()
        pg = browser.new_page(viewport={"width": 1440, "height": 900})
        pg.goto(BASE)
        yield pg
        browser.close()


def wait_rows(page, min_rows: int, timeout_ms: int = 15000) -> int:
    page.wait_for_function(
        f"document.querySelectorAll('#log .row').length >= {min_rows}",
        timeout=timeout_ms,
    )
    return page.evaluate("document.querySelectorAll('#log .row').length")


def test_fr22_demo_stream_appends_rows(page):
    """FR-22:demo 模式产生模拟数据流,页面收到并渲染日志行。
    首行可能是「── 已连接 ──」标记行(连接/断开自动标记),不能作为 demo
    数据判定;断言改为:页面存在含 [demo 前缀的数据行。"""
    assert wait_rows(page, 5) >= 5
    has_demo_row = page.evaluate(
        "[...document.querySelectorAll('#log .row')]"
        ".some(r => r.textContent.includes('[demo'))"
    )
    assert has_demo_row, "页面应存在含 [demo 的数据行"


def test_api_status_contract(server):
    """/api/status 字段契约:phase/port/rxBytes/txBytes/rowsTotal/uptimeSec。"""
    data = json.load(urllib.request.urlopen(f"{BASE}/api/status"))
    for key in ("connected", "phase", "port", "rxBytes", "txBytes", "rowsTotal", "uptimeSec"):
        assert key in data, f"缺字段 {key}"
    assert data["connected"] is True
    assert isinstance(data["rowsTotal"], int)


def test_fr20_theme_switch_updates_tokens(page):
    """FR-20:主题切换即时生效,日志区背景随主题 token 变化。
    2026-09 色板重构后实测:dark #0E1013 / light #FFFFFF / OLED #000000 /
    sepia #F6F0DF。"""
    for theme, bg in (
        ("light", "rgb(255, 255, 255)"),
        ("oled", "rgb(0, 0, 0)"),
        ("sepia", "rgb(246, 240, 223)"),
        ("dark", "rgb(14, 16, 19)"),  # #0E1013,结束回到默认 dark
    ):
        page.select_option("#theme", theme)
        bg_actual = page.evaluate(
            "getComputedStyle(document.getElementById('log')).backgroundColor"
        )
        assert bg_actual == bg, f"{theme} 日志底应为 {bg}:实测 {bg_actual}"


def test_fr2_device_combo_filters_and_picks(page):
    """FR-2:目标设备可编辑下拉——输入筛选候选,点击候选填充完整型号。"""
    page.fill("#chip", "STM32F1")
    opts = page.evaluate(
        "[...document.querySelectorAll('#chip-list .opt')].map(e => e.dataset.v)"
    )
    assert opts, "输入 STM32F1 应有候选"
    assert all("STM32F1" in o.upper() for o in opts)
    page.click("#chip-list .opt")
    assert page.input_value("#chip") == opts[0]
    # 箭头展开全部候选
    page.click("#chip-arrow")
    n_all = page.evaluate("document.querySelectorAll('#chip-list .opt').length")
    assert n_all >= 5


def test_fr2_combo_visual_p0(page):
    """FR-2(视觉 P0,UX 报告):聚焦时容器 2px accent 边框、浮层默认隐藏、
    候选项圆角非 0、与当前输入值相同的候选项常驻 accent 高亮。"""
    page.goto(BASE)  # 全新加载,排除前序用例的交互残留
    page.wait_for_selector("#chip", state="visible")
    # 浮层默认隐藏(页面加载未交互时不展开)
    assert (
        page.evaluate("getComputedStyle(document.getElementById('chip-list')).display")
        == "none"
    ), "页面加载未交互时候选浮层应默认隐藏"

    # 聚焦(真实点击)展开,容器边框 2px solid accent
    page.click("#chip")
    page.wait_for_function(
        "getComputedStyle(document.getElementById('chip-list')).display !== 'none'",
        timeout=3000,
    )
    bw = page.evaluate(
        """() => {
            const cs = getComputedStyle(
                document.getElementById('chip').closest('.combo'));
            return [cs.borderTopStyle, cs.borderTopWidth, cs.borderRightWidth,
                    cs.borderBottomWidth, cs.borderLeftWidth, cs.borderTopColor];
        }"""
    )
    assert bw[0] == "solid" and bw[1:5] == ["2px"] * 4, (
        f"聚焦后容器边框应为 2px solid:实测 {bw}"
    )
    assert bw[5] == _ACCENT_RGB, f"聚焦后容器边框应为 accent 色:实测 {bw[5]}"

    # 候选项胶囊圆角非 0
    radius = page.evaluate(
        "parseFloat(getComputedStyle(document.querySelector('#chip-list .opt'))"
        ".borderTopLeftRadius)"
    )
    assert radius > 0, f"候选项圆角应非 0:实测 {radius}px"

    # 与当前输入值相同的候选项:常驻 accent 高亮(非 hover 态)
    val = page.evaluate("document.querySelector('#chip-list .opt').dataset.v")
    page.fill("#chip", val)
    page.wait_for_timeout(100)
    if (
        page.evaluate("getComputedStyle(document.getElementById('chip-list')).display")
        == "none"
    ):
        page.click("#chip-arrow")  # 输入精确值后若浮层被过滤关闭,用箭头重开
    bg = page.evaluate(
        """() => {
            const val = document.getElementById('chip').value;
            const opt = [...document.querySelectorAll('#chip-list .opt')]
                .find(o => o.dataset.v === val);
            return opt ? getComputedStyle(opt).backgroundColor : null;
        }"""
    )
    assert bg == _ACCENT_RGB, f"当前值候选项应有 accent 常驻高亮:实测 bg={bg}"


def test_fr11_send_echoes_line(page):
    """FR-11:发送内容以回显行(» 前缀)出现在日志。"""
    rows_before = page.evaluate("document.querySelectorAll('#log .row').length")
    page.fill("#send-text", "hello-web")
    page.click("#send-btn")
    page.wait_for_function(
        "document.body.innerText.includes('» hello-web')", timeout=5000
    )
    rows_after = page.evaluate("document.querySelectorAll('#log .row').length")
    assert rows_after > rows_before


def test_fr14_pause_stops_stream(page):
    """FR-14:暂停接收后日志行数不再增长(新数据被丢弃)。"""
    wait_rows(page, 3)
    page.click("#pause-btn")  # 按钮 UI 文本「暂停」→ 暂停态
    n1 = page.evaluate("document.querySelectorAll('#log .row').length")
    time.sleep(1.5)
    n2 = page.evaluate("document.querySelectorAll('#log .row').length")
    assert n2 == n1, f"暂停后行数仍在增长:{n1} → {n2}"
    page.click("#pause-btn")  # 恢复
    page.wait_for_function(
        f"document.querySelectorAll('#log .row').length > {n2}", timeout=10000
    )


def test_fr14_clear_wipes_log(page):
    """FR-14(清空):清空按钮清掉全部行,随后 demo 流继续产生新行。"""
    wait_rows(page, 3)
    before = page.evaluate("window.__clearedCount || 0")
    page.click("#clear-btn")
    # cleared 消息应用瞬间 DOM 清零(之后 demo 流的新行才继续 append)
    page.wait_for_function(
        f"(window.__clearedCount || 0) > {before}", timeout=5000
    )
    assert page.evaluate("document.querySelectorAll('#log .row').length") < 10
    wait_rows(page, 1)  # demo 流继续


def test_fr18_web_search_p0(page):
    """FR-18(UX P0):VS Code 式搜索条——Ctrl+F 打开并聚焦、输入即搜词级命中
    高亮(底色取 --search-hit,不污染整行底色)、n/m 计数(无命中 0/0 err 色、
    空 query 显示 —)、Enter 导航计数递增、Esc 清高亮且 query 保留在内存。"""
    page.goto(BASE)  # 全新加载,排除前序用例的交互残留
    wait_rows(page, 3)

    def rgb(s: str) -> list:
        """'rgba(255, 213, 79, 0.28)' / '#cc4455' → [r, g, b, a] 数值列表。"""
        s = s.strip()
        if s.startswith("#"):
            return [int(s[i : i + 2], 16) for i in (1, 3, 5)] + [1.0]
        parts = s[s.index("(") + 1 : s.index(")")].split(",")
        vals = [float(p) for p in parts[:3]]
        vals.append(float(parts[3]) if len(parts) > 3 else 1.0)
        return vals

    # Ctrl+F:初始隐藏 → 打开,焦点入输入框
    assert (
        page.evaluate("getComputedStyle(document.getElementById('search-bar')).display")
        == "none"
    ), "未按 Ctrl+F 时搜索条应隐藏"
    page.keyboard.press("Control+f")
    page.wait_for_function(
        "getComputedStyle(document.getElementById('search-bar')).display !== 'none'",
        timeout=3000,
    )
    assert (
        page.evaluate("document.activeElement && document.activeElement.id")
        == "s-input"
    ), "Ctrl+F 后焦点应落入搜索输入框"

    # 输入即搜:出现词级命中 mark,底色 == --search-hit,计数为 n/m 格式
    page.keyboard.type("Heartbeat", delay=10)
    page.wait_for_function(
        "document.querySelectorAll('#log mark.s-hit').length > 0", timeout=5000
    )
    page.wait_for_function(
        r"/^\d+\/\d+$/.test(document.getElementById('s-count').textContent)",
        timeout=5000,
    )
    style = page.evaluate(
        """() => {
            const m = document.querySelector('#log mark.s-hit');
            const hitRow = m.closest('.row');
            const cleanRow = [...document.querySelectorAll('#log .row')]
                .find(r => !r.querySelector('mark.s-hit'));
            return {
                hits: document.querySelectorAll('#log mark.s-hit').length,
                varHit: getComputedStyle(document.documentElement)
                    .getPropertyValue('--search-hit').trim(),
                hitBg: getComputedStyle(m).backgroundColor,
                hitRowClass: hitRow.getAttribute('class'),
                hitRowBg: getComputedStyle(hitRow).backgroundColor,
                cleanRowBg: cleanRow ? getComputedStyle(cleanRow).backgroundColor : null};
        }"""
    )
    assert style["hits"] > 0, "应有词级命中高亮元素"
    assert rgb(style["hitBg"]) == rgb(style["varHit"]), (
        f"命中底色应等于 --search-hit:实测 {style['hitBg']} vs 变量 {style['varHit']}"
    )
    assert "hit" not in (style["hitRowClass"] or ""), (
        f"命中所在行不应被加行级高亮类:实测 class={style['hitRowClass']}"
    )
    if style["cleanRowBg"] is not None:
        assert style["hitRowBg"] == style["cleanRowBg"], (
            f"命中行底色不应被整行染色:命中行 {style['hitRowBg']} vs 普通行 {style['cleanRowBg']}"
        )

    # 无命中:0/0 且 err 色 == --err;空 query:— 且无 err
    page.fill("#s-input", "")
    page.wait_for_function(
        "document.getElementById('s-count').textContent === '—'", timeout=5000
    )
    page.keyboard.type("zzzznohitxyz", delay=5)
    page.wait_for_function(
        "document.getElementById('s-count').textContent === '0/0'", timeout=5000
    )
    nohit = page.evaluate(
        """() => ({
            cls: document.getElementById('s-count').className,
            color: getComputedStyle(document.getElementById('s-count')).color,
            varErr: getComputedStyle(document.documentElement)
                .getPropertyValue('--err').trim()})"""
    )
    assert "err" in nohit["cls"], f"无命中计数应带 err 类:实测 {nohit['cls']}"
    assert rgb(nohit["color"]) == rgb(nohit["varErr"]), (
        f"无命中计数颜色应为 --err:实测 {nohit['color']} vs {nohit['varErr']}"
    )

    # Enter 导航:当前命中序号递增(流式追加只影响分母,不影响分子)
    # 注意:debounce 窗口内计数可能残留上一轮的 "0/0",必须等分母 ≥3 的真结果,
    # 否则 Enter 落在空命中态,导航永不发生(假超时)。
    page.fill("#s-input", "")
    page.keyboard.type("Heartbeat", delay=10)
    page.wait_for_function(
        """() => {
            const t = document.getElementById('s-count').textContent;
            const m = Number(t.split('/')[1]);
            return /^\\d+\\/\\d+$/.test(t) && m >= 3;
        }""",
        timeout=5000,
    )
    page.focus("#s-input")
    page.keyboard.press("Enter")
    page.wait_for_function(
        "document.getElementById('s-count').textContent.startsWith('1/')",
        timeout=3000,
    )
    page.keyboard.press("Enter")
    page.wait_for_function(
        "document.getElementById('s-count').textContent.startsWith('2/')",
        timeout=3000,
    )

    # Esc:关闭 + 清高亮;重开 Ctrl+F 后 query 保留(内存)
    page.keyboard.press("Escape")
    page.wait_for_function(
        "document.querySelectorAll('#log mark.s-hit').length === 0", timeout=3000
    )
    assert (
        page.evaluate("getComputedStyle(document.getElementById('search-bar')).display")
        == "none"
    ), "Esc 后搜索条应关闭"
    page.keyboard.press("Control+f")
    page.wait_for_function(
        "getComputedStyle(document.getElementById('search-bar')).display !== 'none'",
        timeout=3000,
    )
    assert page.input_value("#s-input") == "Heartbeat", (
        f"Esc 后重开应保留 query:实测 {page.input_value('#s-input')!r}"
    )
    page.keyboard.press("Escape")  # 收尾:关闭搜索条,还给后续用例干净状态
    page.wait_for_timeout(200)


def test_ui_layout_p0(page):
    """UI P0 视觉断言(2026-09 UI 重构规格,2026-09-19 getComputedStyle 实测定值):
    左面板宽 420±10、控件高 40±2(按钮基线 42 亦达标)、日志行高 32±1、
    日志字号 15、无顶部 header、发送条在日志区下方通栏、
    单连接切换按钮(连接/断开同钮)、通道选项 16 项。"""
    page.goto(BASE)  # 全新加载,排除前序用例的交互残留
    page.wait_for_selector("#log .row", timeout=15000)
    page.wait_for_timeout(1500)  # 等 WS 状态/prefs 回填落定,避免测量到中间态

    # 1) 左面板宽 420±10
    aside_w = page.evaluate("document.querySelector('aside').getBoundingClientRect().width")
    assert 410 <= aside_w <= 430, f"左面板宽应 420±10:实测 {aside_w}"

    # 2) 控件高 40±2(select/input/combo 容器 40,按钮 42;全部在容差内)
    #    例外:#send-text/#send-btn 为加高的多行发送区,单独断言 72±2
    heights = page.evaluate(
        """() => {
            const h = el => el.getBoundingClientRect().height;
            const ids = ['jlink', 'chip-combo', 'iface', 'speed', 'channel',
                         'connect-btn', 'reset-btn', 'clear-btn', 'pause-btn',
                         'mark-btn', 'export-btn',
                         'rx-ending', 'encoding', 'tx-ending', 'timer-interval',
                         'frame-timeout', 'theme'];
            return Object.fromEntries(ids.map(id => [id, h(document.getElementById(id))]));
        }"""
    )
    bad = {k: v for k, v in heights.items() if not (38 <= v <= 42)}
    assert not bad, f"控件高应 40±2:越界 {bad}"
    send_h = page.evaluate(
        "(() => { const t = document.getElementById('send-text').getBoundingClientRect().height;"
        " const b = document.getElementById('send-btn').getBoundingClientRect().height;"
        " return {t: Math.round(t), b: Math.round(b), aligned: Math.abs(t - b) < 2}; })()"
    )
    assert 70 <= send_h["t"] <= 74 and 70 <= send_h["b"] <= 74 and send_h["aligned"], \
        f"发送区应 72±2 且按钮贴底:实测 {send_h}"

    # 3) 日志行高 32±1(computed line-height + 未换行短行实际高度)
    lh = page.evaluate("parseFloat(getComputedStyle(document.getElementById('log')).lineHeight)")
    assert 31 <= lh <= 33, f"日志行 line-height 应 32±1:实测 {lh}"
    # 快照环形缓冲可能只剩长数据行;标记行(「── 已连接 ──」,demo 循环周期产生)
    # 是稳定的短行来源,等待其出现后量实际高度
    page.wait_for_function(
        "[...document.querySelectorAll('#log .row')]"
        ".some(r => r.textContent.length > 0 && r.textContent.length < 60)",
        timeout=20000,
    )
    row_h = page.evaluate(
        """() => {
            const s = [...document.querySelectorAll('#log .row')]
                .find(r => r.textContent.length > 0 && r.textContent.length < 60);
            return s.getBoundingClientRect().height;
        }"""
    )
    assert 31 <= row_h <= 33, f"日志短行实际高应 32±1:实测 {row_h}"

    # 4) 日志字号 15
    fs = page.evaluate("parseFloat(getComputedStyle(document.getElementById('log')).fontSize)")
    assert fs == 15, f"日志字号应 15px:实测 {fs}"

    # 5) 无顶部 header:不存在 header 元素,main 顶到视口顶
    top = page.evaluate(
        """() => ({
            hasHeader: !!document.querySelector('body > header'),
            mainTop: document.querySelector('main').getBoundingClientRect().top})"""
    )
    assert not top["hasHeader"] and abs(top["mainTop"]) < 0.5, (
        f"不应有顶部 header 且 main 应贴顶:实测 {top}"
    )

    # 6) 发送条在日志区下方通栏(顶边贴日志区底边,宽度 = 右列宽)
    sb = page.evaluate(
        """() => {
            const r = el => el.getBoundingClientRect();
            const bar = r(document.getElementById('send-bar'));
            const wrap = r(document.getElementById('log-wrap'));
            const content = r(document.getElementById('content'));
            return {barTop: bar.top, wrapBottom: wrap.bottom,
                    dw: Math.abs(bar.width - content.width),
                    dx: Math.abs(bar.left - content.left)};
        }"""
    )
    assert abs(sb["barTop"] - sb["wrapBottom"]) < 1.5, (
        f"发送条应紧贴日志区下方:实测 bar.top={sb['barTop']} log.bottom={sb['wrapBottom']}"
    )
    assert sb["dw"] < 1.5 and sb["dx"] < 1.5, (
        f"发送条应通栏(宽/左缘对齐右列):实测 dw={sb['dw']} dx={sb['dx']}"
    )

    # 7) 单连接切换按钮:唯一连接控件,连接/断开同钮,文案与配色联动
    single = page.evaluate(
        """() => {
            const btns = [...document.querySelectorAll('button')];
            return {
                connectCount: document.querySelectorAll('#connect-btn').length,
                hasDisconnectBtn: btns.some(b =>
                    /disconnect/i.test(b.id) || b.textContent.trim() === '断开连接'),
            };
        }"""
    )
    assert single["connectCount"] == 1, "应只有一个连接按钮"
    assert not single["hasDisconnectBtn"], "不应存在独立断开按钮"

    def btn_state_matches_text() -> bool:
        # 断开 ⟺ danger 配色,连接 ⟺ primary 配色(同钮切换的呈现契约)
        return page.evaluate(
            """() => {
                const b = document.getElementById('connect-btn');
                const t = b.textContent.trim(), cls = b.classList;
                if (t === '断开') return cls.contains('danger') && !cls.contains('primary');
                if (t === '连接') return cls.contains('primary') && !cls.contains('danger');
                return false;
            }"""
        )

    assert btn_state_matches_text(), "连接按钮文案与配色应联动(连接=primary/断开=danger)"
    pre = page.evaluate("document.getElementById('connect-btn').textContent.trim()")
    page.click("#connect-btn")  # 发送切换意图(demo 连接态自主循环,翻转可能来自任一方)
    page.wait_for_function(
        f"document.getElementById('connect-btn').textContent.trim() !== '{pre}'",
        timeout=12000,
    )
    assert btn_state_matches_text(), "切换后文案与配色应仍联动"
