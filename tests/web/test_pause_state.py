"""暂停状态必须由服务端恢复，而不是由按钮当前文案猜测。"""

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, QAServer, get_json, http


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("pause-state-qa"))
    srv.start()
    yield srv
    srv.stop()


def test_pause_state_survives_reload_and_first_click_resumes(server):
    status, _ = http("POST", "/api/pause", {"on": True})
    assert status == 200
    assert get_json("/api/status")["paused"] is True

    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page(viewport={"width": 1440, "height": 900})
        page.goto(BASE)
        page.wait_for_function("document.querySelector('#pause-btn').textContent === '继续'")
        page.click("#pause-btn")
        page.wait_for_function("document.querySelector('#pause-btn').textContent === '暂停'")
        browser.close()

    assert get_json("/api/status")["paused"] is False
