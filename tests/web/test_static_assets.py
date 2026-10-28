"""前端静态资源必须由内嵌路由提供，并使用浏览器可执行的 MIME。"""

import urllib.request

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, QAServer, http, poll


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("static-assets-qa"))
    srv.start()
    yield srv
    srv.stop()


@pytest.mark.parametrize(
    ("path", "mime"),
    [
        ("/styles.css", "text/css"),
        ("/app.js", "text/javascript"),
        ("/api.js", "text/javascript"),
        ("/log-view.js", "text/javascript"),
        ("/search.js", "text/javascript"),
        ("/settings.js", "text/javascript"),
        ("/dialogs.js", "text/javascript"),
    ],
)
def test_embedded_assets_have_executable_mime(server, path, mime):
    with urllib.request.urlopen(BASE + path, timeout=5) as response:
        assert response.status == 200
        assert response.headers.get_content_type() == mime
        assert response.read()


def test_index_uses_external_stylesheet_and_module(server):
    status, html = http("GET", "/")
    assert status == 200
    assert '<link rel="stylesheet" href="/styles.css">' in html
    assert '<script type="module" src="/app.js"></script>' in html
    assert "<script>" not in html


def test_modules_load_without_browser_errors(server):
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page()
        errors = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.on("console", lambda message: errors.append(message.text) if message.type == "error" else None)
        page.goto(BASE)
        page.wait_for_selector("#log .row")
        page.wait_for_timeout(200)
        assert errors == []
        browser.close()


def test_websocket_reconnects_after_service_restart(server):
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page()
        page.add_init_script(
            """
            window.__sockets = [];
            const NativeWebSocket = window.WebSocket;
            window.WebSocket = class extends NativeWebSocket {
              constructor(...args) {
                super(...args);
                window.__sockets.push(this);
              }
            };
            """
        )
        page.goto(BASE)
        page.wait_for_selector("#log .row")
        server.restart()
        assert poll(
            lambda: page.evaluate(
                "window.__sockets.length >= 2 && "
                "window.__sockets[window.__sockets.length - 1].readyState === WebSocket.OPEN"
            ),
            True,
            15,
        ), "服务重启后 WebSocket 应自动重连"
        page.wait_for_selector("#log .row")
        browser.close()
