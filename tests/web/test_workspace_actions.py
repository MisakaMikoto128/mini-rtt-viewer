"""FR-30:浏览器后台入口与可持久保存的日志工作目录。"""

import json

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, QAServer


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("workspace-actions-qa")
    srv = QAServer(
        tmp,
        extra_env={
            "RTT_LOG_DIR": str(tmp / "logs"),
            "RTT_DISABLE_SHELL_OPEN": "1",
        },
    )
    srv.log_dir = tmp / "logs"
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
    pg.goto(BASE)
    pg.wait_for_selector("#log .row")
    yield pg
    pg.close()


def test_fr30_browser_backend_and_log_folder_buttons_are_visible(page):
    assert page.locator("#open-browser").is_visible()
    assert page.locator("#open-browser").inner_text() == "浏览器后台"
    assert page.locator("#open-logs").is_visible()
    assert page.locator("#open-logs").inner_text() == "日志文件夹"


def test_fr30_browser_backend_button_uses_existing_local_endpoint(page):
    page.route("**/api/open-browser", lambda route: route.fulfill(status=200, body=""))
    with page.expect_request("**/api/open-browser", timeout=5000) as request:
        page.click("#open-browser")
    assert request.value.method == "POST"


def test_fr30_export_saves_unique_files_in_log_folder(page, server):
    names = []
    for _ in range(2):
        with page.expect_response(lambda r: "/api/logs/save" in r.url, timeout=5000) as response:
            page.click("#export-btn")
        assert response.value.status == 200
        names.append(response.value.json()["name"])
    assert names[0] != names[1]
    files = sorted(server.log_dir.glob("*.log"))
    assert [path.name for path in files] == sorted(names)
    for name in names:
        assert "[qa " in (server.log_dir / name).read_text(encoding="utf-8")


def test_fr30_open_log_folder_creates_isolated_directory(page, server):
    for path in server.log_dir.glob("*.log"):
        path.unlink()
    with page.expect_response(lambda r: "/api/logs/open" in r.url, timeout=5000) as response:
        page.click("#open-logs")
    assert response.value.status == 200
    assert server.log_dir.is_dir()
    assert json.loads(response.value.body())["opened"] is True
