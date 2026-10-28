"""正式程序只保留 J-Link 后端的黑盒契约。"""

import os
import subprocess

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, EXE, QAServer, get_json, http, poll


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("backends-qa"))
    srv.start(production=True)
    yield srv
    srv.stop()


def test_backends_only_jlink(server):
    data = get_json("/api/backends")
    assert data["selected"] == "jlink"
    assert len(data["items"]) == 1
    item = data["items"][0]
    assert item["id"] == "jlink"
    assert item["name"]
    assert item["description"]
    assert item["isVirtual"] is False
    assert get_json("/api/status")["backend"] == "jlink"


def test_removed_and_unknown_backends_rejected(server):
    for backend in ["demo", "qa", "nope"]:
        code, _ = http("POST", "/api/settings", {"backend": backend})
        assert code == 400, (backend, code)
        assert get_json("/api/status")["backend"] == "jlink"


def test_jlink_backend_setting_is_idempotent_and_persisted(server):
    code, text = http("POST", "/api/settings", {"backend": "jlink"})
    assert code == 200, text
    assert poll(lambda: server.prefs_on_disk().get("backend"), "jlink", 3.0)


def test_backend_selector_removed_from_ui(server):
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page(viewport={"width": 1440, "height": 900})
        page.goto(BASE)
        page.wait_for_selector("#jlink", state="visible")
        assert page.locator("#backend").count() == 0
        assert "Demo" not in page.locator("body").inner_text()
        browser.close()


def test_demo_cli_flag_rejected(tmp_path):
    env = {
        **os.environ,
        "APPDATA": str(tmp_path),
        "RTT_PREFS_FILE": str(tmp_path / "prefs.json"),
        "RTT_WEB_NO_BROWSER": "1",
    }
    result = subprocess.run(
        [str(EXE), "--demo-log", "--no-window"],
        env=env,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=10,
        check=False,
    )
    assert result.returncode == 2
    assert "未知参数" in result.stderr
