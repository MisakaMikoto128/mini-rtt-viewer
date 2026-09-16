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

PORT = 18080
BASE = f"http://127.0.0.1:{PORT}"
EXE = r"target\release\mini-rtt-viewer.exe"

# UX 规格中的 accent 色 #28afe9(getComputedStyle 返回 rgb 形式)
_ACCENT_RGB = "rgb(40, 175, 233)"


@pytest.fixture(scope="session")
def server():
    # --no-window:纯服务模式(不开窗口/托盘,供 playwright 黑盒测试)。
    # APPDATA 指向临时目录:服务与测试环境隔离,不读不写用户真实偏好
    # (面板初值恢复走的 /api/prefs 会回填持久化的 chip,干扰下拉交互用例)
    tmp_appdata = tempfile.mkdtemp(prefix="rtt-test-appdata-")
    env = {**os.environ, "APPDATA": tmp_appdata}
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
    """FR-22:demo 模式产生模拟数据流,页面收到并渲染日志行。"""
    assert wait_rows(page, 5) >= 5
    first = page.text_content("#log .row")
    assert "[demo" in first


def test_api_status_contract(server):
    """/api/status 字段契约:phase/port/rxBytes/txBytes/rowsTotal/uptimeSec。"""
    data = json.load(urllib.request.urlopen(f"{BASE}/api/status"))
    for key in ("connected", "phase", "port", "rxBytes", "txBytes", "rowsTotal", "uptimeSec"):
        assert key in data, f"缺字段 {key}"
    assert data["connected"] is True
    assert isinstance(data["rowsTotal"], int)


def test_fr20_theme_switch_updates_tokens(page):
    """FR-20:主题切换即时生效——切浅色后日志区背景变白。"""
    page.select_option("#theme", "light")
    bg = page.evaluate("getComputedStyle(document.getElementById('log')).backgroundColor")
    assert bg == "rgb(255, 255, 255)"
    page.select_option("#theme", "oled")
    bg = page.evaluate("getComputedStyle(document.getElementById('log')).backgroundColor")
    assert bg == "rgb(0, 0, 0)"
    page.select_option("#theme", "dark")
    bg = page.evaluate("getComputedStyle(document.getElementById('log')).backgroundColor")
    assert bg == "rgb(34, 34, 34)"


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
