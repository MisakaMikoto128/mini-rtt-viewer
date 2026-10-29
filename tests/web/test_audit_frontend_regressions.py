"""前端竞态与设置回归:真实页面 + 可控 WS/HTTP 边界。"""

import json

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, QAServer


FAKE_WEBSOCKET = """
window.__qaSockets = [];
window.WebSocket = class {
  constructor(url) {
    this.url = url;
    this.readyState = 1;
    window.__qaSockets.push(this);
  }
  close() {
    this.readyState = 3;
    if (this.onclose) this.onclose();
  }
  emit(message) {
    if (this.onmessage) this.onmessage({data: JSON.stringify(message)});
  }
};
"""


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("audit-frontend-qa"))
    srv.start()
    yield srv
    srv.stop()


@pytest.fixture(scope="module")
def browser():
    with sync_playwright() as p:
        instance = p.chromium.launch()
        yield instance
        instance.close()


@pytest.fixture
def page(browser, server):
    pg = browser.new_page(viewport={"width": 1440, "height": 900})
    pg.add_init_script(FAKE_WEBSOCKET)
    pg.goto(BASE)
    pg.wait_for_function("window.__qaSockets.length === 1")
    yield pg
    pg.close()


def emit(page, message):
    page.evaluate(
        "message => window.__qaSockets[window.__qaSockets.length - 1].emit(message)",
        message,
    )


def log_row(text):
    return {"runs": [{"text": text, "fg": None}]}


def log_text(page):
    return page.locator("#log").inner_text()


def test_snapshot_clear_watermark_filters_old_rows_and_allows_restart(page):
    emit(page, {"type": "snapshot", "clearSeq": 0, "rows": [log_row("before clear")]})
    emit(page, {"type": "cleared", "seq": 40})
    emit(page, {"type": "rows", "seq": 39, "dropped": 0, "rows": [log_row("stale")]})
    assert "stale" not in log_text(page)

    # 同服务的 lagged snapshot 带回当前 clear 水位，旧 rows 仍须过滤。
    emit(page, {"type": "snapshot", "clearSeq": 40, "rows": []})
    emit(page, {"type": "rows", "seq": 40, "dropped": 0, "rows": [log_row("still stale")]})
    assert "still stale" not in log_text(page)

    # 新服务的 snapshot 以 0 为同步基准，低 seq rows 不得被旧水位吞掉。
    emit(page, {"type": "snapshot", "clearSeq": 0, "rows": []})
    emit(page, {"type": "rows", "seq": 1, "dropped": 0, "rows": [log_row("after restart")]})
    assert "after restart" in log_text(page)


def test_timer_posts_current_text_and_hex_mode_after_switch(page):
    page.route("**/api/timer", lambda route: route.fulfill(status=200, body=""))
    page.fill("#send-text", "A")
    page.check("#hex-send")
    assert page.input_value("#send-text") == "41"
    with page.expect_request("**/api/timer") as enabled:
        page.click("#timer-on")
    assert json.loads(enabled.value.post_data) == {"on": True, "text": "41", "hex": True}

    page.uncheck("#hex-send")
    assert page.input_value("#send-text") == "A"
    page.fill("#timer-interval", "2")
    with page.expect_request("**/api/timer") as changed:
        page.dispatch_event("#timer-interval", "change")
    assert json.loads(changed.value.post_data) == {
        "on": True, "intervalSec": 2, "text": "A", "hex": False,
    }


def test_timer_empty_content_error_does_not_leave_fake_enabled_state(page):
    page.route(
        "**/api/timer",
        lambda route: route.fulfill(status=400, body="定时发送内容不能为空"),
    )
    with page.expect_request("**/api/timer") as request_info:
        page.click("#timer-on")
    page.wait_for_selector("#toast.show")
    assert json.loads(request_info.value.post_data) == {"on": True}
    assert "定时发送内容不能为空" in page.locator("#toast").inner_text()
    assert not page.is_checked("#timer-on")
    assert page.locator("#timer-fields").is_hidden()


def test_timer_empty_input_does_not_overwrite_restored_content(page):
    def prefs(route):
        response = route.fetch()
        payload = response.json()
        payload.update({"timerOn": True, "timerText": "41", "timerHex": True})
        route.fulfill(status=200, content_type="application/json", body=json.dumps(payload))

    page.route("**/api/prefs", prefs)
    page.reload()
    page.wait_for_function("document.querySelector('#timer-on').checked")
    assert page.input_value("#send-text") == ""

    page.route("**/api/timer", lambda route: route.fulfill(status=200, body=""))
    with page.expect_request("**/api/timer") as changed:
        page.evaluate(
            """() => {
                const input = document.querySelector('#timer-interval');
                input.value = '2';
                input.dispatchEvent(new Event('change', {bubbles: true}));
            }"""
        )
    assert json.loads(changed.value.post_data) == {"on": True, "intervalSec": 2}

    with page.expect_request("**/api/timer") as stopped:
        page.click("#timer-on")
    assert json.loads(stopped.value.post_data) == {"on": False}


def test_power_http_failure_rolls_back_checkbox(page):
    emit(page, {"type": "state", "connected": True, "status": "● 已连接 (qa)"})
    page.route("**/api/power", lambda route: route.fulfill(status=500, body="power unavailable"))
    with page.expect_response("**/api/power"):
        page.click("#power")
    page.wait_for_selector("#toast.show")
    assert "power unavailable" in page.locator("#toast").inner_text()
    assert not page.is_checked("#power")


def test_power_websocket_result_is_authoritative_and_disconnect_clears(page):
    emit(page, {"type": "state", "connected": True, "status": "● 已连接 (qa)"})
    page.route("**/api/power", lambda route: route.fulfill(status=200, body=""))

    with page.expect_response("**/api/power"):
        page.check("#power")
    emit(page, {"type": "power", "on": False, "ok": True, "message": "实际未开启"})
    assert not page.is_checked("#power")

    with page.expect_response("**/api/power"):
        page.check("#power")
    emit(page, {"type": "power", "on": True, "ok": False, "message": "电源切换失败"})
    page.wait_for_selector("#toast.show")
    assert not page.is_checked("#power")
    assert "电源切换失败" in page.locator("#toast").inner_text()

    emit(page, {"type": "power", "on": True, "ok": True, "message": "已开启"})
    assert page.is_checked("#power")
    emit(page, {"type": "state", "connected": False, "status": "● 未连接 (qa)"})
    assert not page.is_checked("#power")


@pytest.mark.parametrize("status", [200, 500])
def test_stale_custom_theme_response_cannot_override_new_selection(page, status):
    pending = []
    page.route("**/api/theme-css/slow", lambda route: pending.append(route))
    page.evaluate("document.querySelector('#theme').add(new Option('Slow', 'slow'))")
    with page.expect_request("**/api/theme-css/slow"):
        page.select_option("#theme", "slow")
    page.select_option("#theme", "light")
    with page.expect_response("**/api/theme-css/slow"):
        pending.pop().fulfill(status=status, body=":root{--panel:#123456}")
    page.wait_for_timeout(100)
    assert page.locator("html").get_attribute("data-theme") == "light"
    assert page.locator("style[data-custom-theme]").count() == 0


def test_keep_awake_restores_and_posts_server_preference(page):
    def prefs(route):
        response = route.fetch()
        payload = response.json()
        payload["keepAwake"] = True
        route.fulfill(status=200, content_type="application/json", body=json.dumps(payload))

    page.route("**/api/prefs", prefs)
    page.reload()
    page.wait_for_selector("#keep-awake", state="visible")
    assert page.is_checked("#keep-awake")
    assert page.locator("#receive-panel").is_visible()

    page.route("**/api/settings", lambda route: route.fulfill(status=200, body=""))
    with page.expect_request("**/api/settings") as request_info:
        page.uncheck("#keep-awake")
    assert json.loads(request_info.value.post_data) == {"keepAwake": False}
