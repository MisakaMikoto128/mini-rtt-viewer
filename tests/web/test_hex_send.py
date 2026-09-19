"""验收矩阵 4:hex-send 接线(黑盒,预期来自验收矩阵条目)。

- 勾选 HEX 发送 + 输入合法 hex("61 62")发送 → TX 计数 +2
- 非法 hex("zz")→ 400
"""

import json

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, QAServer, get_json, http


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("hex-qa"))
    srv.start()
    yield srv
    srv.stop()


@pytest.fixture(scope="module")
def page(server):
    with sync_playwright() as p:
        browser = p.chromium.launch()
        pg = browser.new_page(viewport={"width": 1440, "height": 900})
        pg.goto(BASE)
        pg.wait_for_selector("#send-text", state="visible")
        # FR-12(F7):发送行尾会附加字节;置「无」隔离出 HEX 解析计数,
        # 使 +2 断言只反映 0x61 0x62 两个字节
        pg.select_option("#tx-ending", "3")
        if not pg.is_checked("#hex-send"):
            pg.click("#hex-send")  # 勾选 HEX 发送(矩阵前置)
        assert pg.is_checked("#hex-send"), "前置:HEX 发送应处于勾选态"
        yield pg
        browser.close()


def _tx_bytes() -> int:
    return get_json("/api/status")["txBytes"]


def test_hex_4a_valid_hex_sends_two_bytes(page):
    """勾选 HEX + "61 62" 发送 → TX 计数 +2(0x61 0x62 两个字节)。"""
    tx0 = _tx_bytes()
    page.fill("#send-text", "61 62")
    page.click("#send-btn")
    page.wait_for_function(
        "document.body.innerText.includes('» 61 62')", timeout=5000
    )
    import time

    deadline = time.time() + 3
    tx1 = _tx_bytes()
    while time.time() < deadline and tx1 - tx0 == 0:
        time.sleep(0.2)
        tx1 = _tx_bytes()
    delta = tx1 - tx0
    assert delta == 2, f"合法 hex '61 62' 发送后 TX 计数应 +2:实测 +{delta}({tx0}→{tx1})"


def test_hex_4b_invalid_hex_rejected_400(page):
    """勾选 HEX + 非法 hex("zz")发送 → HTTP 400。"""
    with page.expect_response(
        lambda r: "/api/send" in r.url, timeout=5000
    ) as ri:
        page.fill("#send-text", "zz")
        page.click("#send-btn")
    resp = ri.value
    assert resp.status == 400, (
        f"非法 hex 'zz' 发送应返回 400:实测 {resp.status}"
        f"(请求体 {resp.request.post_data})"
    )
