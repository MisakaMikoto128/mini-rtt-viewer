"""验收矩阵 2:发送历史持久化(黑盒,预期来自验收矩阵条目)。

- POST /api/send 三条不同文本 → GET /api/history 含三条且最新在前
- 重复发送去重置顶
- 重启服务(同 RTT_PREFS_FILE)→ /api/history 仍有(持久化)
- 前端 ↑↓ 翻历史(playwright 键盘)
"""

import time

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, QAServer, get_json, http, poll

A, B, C = "qa-往-alpha", "qa-往-beta", "qa-往-gamma"


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("history-qa"))
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
        yield pg
        browser.close()


def _send(text: str) -> int:
    status, _ = http("POST", "/api/send", {"text": text})
    return status


def _history() -> list[str]:
    return get_json("/api/history")


def _wait_history(expect: list[str], timeout_s: float = 5.0) -> bool:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        if _history() == expect:
            return True
        time.sleep(0.2)
    return False


def test_history_2a_order_dedup_persist_restart(server):
    """三条发送 → 最新在前;重复去重置顶;重启后仍在(持久化)。"""
    # 前置:干净历史(临时 prefs 首启应为空)
    assert _history() == [], f"临时 prefs 首启历史应为空:实测 {_history()}"

    assert _send(A) == 200 and _send(B) == 200 and _send(C) == 200
    assert _wait_history([C, B, A]), (
        f"三条历史应最新在前 [C,B,A]:实测 {_history()}"
    )

    # 重复发送 A → 去重且置顶
    assert _send(A) == 200
    assert _wait_history([A, C, B]), (
        f"重复发送应去重置顶 [A,C,B]:实测 {_history()}"
    )

    # 等待节流写盘把历史落盘(进程被 terminate 无退出回调,不能依赖
    # 退出强制补写;磁盘出现 send_history 即持久化已发生)
    def disk_history():
        return server.prefs_on_disk().get("send_history")

    assert poll(disk_history, [A, C, B], 5.0), (
        f"5s 内磁盘 prefs.json 的 send_history 应为 [A,C,B]:实测 {disk_history()!r}"
    )

    # 重启服务(同一 RTT_PREFS_FILE)→ 历史仍在
    server.restart()
    assert _history() == [A, C, B], (
        f"重启后历史应保留 [A,C,B]:实测 {_history()}"
    )


def test_history_2b_arrow_key_navigation(server, page):
    """前端 ↑↓ 翻历史:↑ 逐条回溯(最新→更早),↓ 反向,输入框值随之变化。"""
    # 服务已被 2a 重启,历史 = [A, C, B](最新在前:A 是重复发送后置顶项)
    assert _history() == [A, C, B], f"前置历史应为 [A,C,B]:实测 {_history()}"

    page.reload()
    page.wait_for_selector("#send-text", state="visible")

    # 第一次 ↑ 可能与前端 /api/history 异步拉取竞速(拉取前 ↑ 是 no-op),
    # 重试直到生效;生效判定 = 输入框变为最新一条 A
    deadline = time.time() + 6
    while time.time() < deadline:
        page.focus("#send-text")
        page.keyboard.press("ArrowUp")
        if page.input_value("#send-text") == A:
            break
        if page.input_value("#send-text") != "":
            page.keyboard.press("ArrowDown")  # 回草稿,复位导航状态再试
        time.sleep(0.25)
    assert page.input_value("#send-text") == A, (
        f"第一次 ↑ 应带入最新一条 {A}:实测 {page.input_value('#send-text')!r}"
    )

    # 历史已载入内存,后续按键无竞速,直接断言
    page.keyboard.press("ArrowUp")
    page.wait_for_function(
        f"document.getElementById('send-text').value === '{C}'", timeout=3000
    )
    assert page.input_value("#send-text") == C, (
        f"第二次 ↑ 应回溯到更早一条 {C}:实测 {page.input_value('#send-text')!r}"
    )

    page.keyboard.press("ArrowDown")
    page.wait_for_function(
        f"document.getElementById('send-text').value === '{A}'", timeout=3000
    )
    assert page.input_value("#send-text") == A, (
        f"↓ 应反向回到 {A}:实测 {page.input_value('#send-text')!r}"
    )
