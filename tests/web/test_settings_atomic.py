"""设置请求必须完整校验后再提交，失败不能留下部分更新。"""

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, QAServer, get_json, http


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("settings-atomic-qa"))
    srv.start()
    yield srv
    srv.stop()


def test_invalid_settings_request_does_not_apply_valid_prefix_fields(server):
    status, _ = http("POST", "/api/settings", {"rxEnding": 2})
    assert status == 200
    assert get_json("/api/prefs")["rxEnding"] == 2

    status, _ = http(
        "POST",
        "/api/settings",
        {"rxEnding": 1, "speedIndex": -5},
    )
    assert status == 400
    assert get_json("/api/prefs")["rxEnding"] == 2


def test_business_settings_use_server_not_local_storage(server):
    assert http("POST", "/api/settings", {"rxEnding": 2})[0] == 200
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page()
        page.goto(BASE)
        page.wait_for_function("document.querySelector('#rx-ending').value === '2'")
        page.select_option("#rx-ending", "1")
        page.wait_for_function(
            "fetch('/api/prefs').then(r => r.json()).then(p => p.rxEnding === 1)"
        )
        assert page.evaluate("localStorage.getItem('web-rx-ending')") is None
        page.reload()
        page.wait_for_function("document.querySelector('#rx-ending').value === '1'")
        browser.close()
