"""浏览器管理台黑盒测试(pytest + Playwright)。

纪律(参照 docs/team/charter.md):预期只来自 docs/product/spec.md 条目,
测试名按条目命名(test_fr<N>_<行为>)。服务以 --demo-log 启动(fixture)。

运行前置:
    pip install pytest playwright
    playwright install chromium
    cargo build --release --locked
"""

import json
import subprocess
import time
import urllib.request

import pytest
from playwright.sync_api import sync_playwright

PORT = 18080
BASE = f"http://127.0.0.1:{PORT}"
EXE = r"target\release\rtt-web.exe"


@pytest.fixture(scope="session")
def server():
    proc = subprocess.Popen([EXE, "--demo-log", "--port", str(PORT)])
    for _ in range(50):
        try:
            urllib.request.urlopen(f"{BASE}/api/status", timeout=1)
            break
        except Exception:
            time.sleep(0.3)
    else:
        proc.terminate()
        pytest.fail("rtt-web 未在 15s 内就绪")
    yield proc
    proc.terminate()


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
