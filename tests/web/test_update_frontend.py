"""Browser tests for the explicit, server-driven updater controls in About."""

import json
import re
import pytest
from playwright.sync_api import expect, sync_playwright

from qa_server import BASE, QAServer


NOTES_URL = (
    "https://github.com/MisakaMikoto128/mini-rtt-viewer/releases/tag/v9.0.0"
)


def update_status(**overrides):
    """Mirror every field of the camelCase UpdateStatus API contract."""
    status = {
        "currentVersion": "0.5.2",
        "latestVersion": None,
        "phase": "idle",
        "notesUrl": None,
        "message": None,
    }
    status.update(overrides)
    return status


def fulfill_json(route, payload, status=200):
    route.fulfill(
        status=status,
        content_type="application/json",
        body=json.dumps(payload),
    )


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("update-frontend-qa"))
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
    pg.set_default_timeout(5000)
    yield pg
    pg.close()


def load(page, status=None, desktop=False):
    status = status or update_status()
    page.route("**/api/update/status", lambda route: fulfill_json(route, status))
    if desktop:
        page.add_init_script("window.__MINI_SHELL = true;")
    page.goto(BASE)
    page.wait_for_selector("#version", state="visible")


def open_about(page):
    page.click("#version")
    expect(page.locator("#about")).to_be_visible()


def test_available_update_requires_two_explicit_user_actions(page):
    available = update_status(
        phase="available", latestVersion="9.0.0", notesUrl=NOTES_URL
    )
    ready = update_status(
        phase="readyToRestart", latestVersion="9.0.0", notesUrl=NOTES_URL
    )
    requests = {"check": 0, "install": 0, "restart": 0}

    def action(name, response):
        def handle(route):
            requests[name] += 1
            if response is None:
                route.fulfill(status=200, content_type="text/plain", body="")
            else:
                fulfill_json(route, response)

        return handle

    page.route("**/api/update/check", action("check", available))
    page.route("**/api/update/install", action("install", ready))
    page.route("**/api/update/restart", action("restart", None))
    load(page, desktop=True)
    open_about(page)

    expect(page.locator("#update-check")).to_be_enabled()
    expect(page.locator("#update-install")).to_be_hidden()
    expect(page.locator("#update-restart")).to_be_hidden()
    assert requests == {"check": 0, "install": 0, "restart": 0}

    page.click("#update-check")
    expect(page.locator("#update-status")).to_have_text("发现新版本 9.0.0")
    expect(page.locator("#update-install")).to_be_enabled()
    expect(page.locator("#update-restart")).to_be_hidden()
    assert requests == {"check": 1, "install": 0, "restart": 0}

    page.click("#update-install")
    expect(page.locator("#update-restart")).to_be_visible()
    expect(page.locator("#update-restart")).to_be_enabled()
    assert not page.locator("#update-install").is_enabled()
    assert requests == {"check": 1, "install": 1, "restart": 0}

    page.click("#update-restart")
    expect(page.locator("#update-restart")).to_be_disabled()
    expect(page.locator("#update-status")).to_have_text("重启请求已发送")
    assert requests == {"check": 1, "install": 1, "restart": 1}


def test_failed_check_preserves_current_version_and_recovers(page):
    attempts = []

    def check(route):
        attempts.append(route.request)
        if len(attempts) == 1:
            route.fulfill(
                status=500, content_type="text/plain", body="检查失败:网络不可用"
            )
        else:
            fulfill_json(
                route,
                update_status(phase="available", latestVersion="9.0.0"),
            )

    page.route("**/api/update/check", check)
    load(page)
    open_about(page)
    page.click("#update-check")

    expect(page.locator("#update-status")).to_have_text("检查失败:网络不可用")
    expect(page.locator("#about-ver")).to_have_text("v0.5.2")
    expect(page.locator("#update-check")).to_be_enabled()
    expect(page.locator("#update-install")).to_be_hidden()

    page.click("#update-check")
    expect(page.locator("#update-status")).to_have_text("发现新版本 9.0.0")
    expect(page.locator("#update-install")).to_be_enabled()
    assert len(attempts) == 2


def test_repeated_click_while_checking_sends_only_one_request(page):
    requests = []

    def check(route):
        requests.append(route.request)
        fulfill_json(route, update_status(phase="available", latestVersion="9.0.0"))

    page.route("**/api/update/check", check)
    load(page)
    open_about(page)
    pending = page.evaluate(
        "() => { const button = document.querySelector('#update-check'); "
        "button.click(); const pending = {disabled: button.disabled, "
        "status: document.querySelector('#update-status').textContent}; "
        "button.click(); return pending; }"
    )
    assert pending == {"disabled": True, "status": "正在检查…"}

    expect(page.locator("#update-status")).to_have_text("发现新版本 9.0.0")
    expect(page.locator("#update-check")).to_be_enabled()
    assert len(requests) == 1


def test_connected_install_409_is_shown_and_requires_explicit_retry(page):
    attempts = []

    def install(route):
        attempts.append(route.request)
        if len(attempts) == 1:
            route.fulfill(
                status=409,
                content_type="text/plain",
                body="更新安装冲突 (409)：请先断开目标设备",
            )
        else:
            fulfill_json(
                route,
                update_status(phase="readyToRestart", latestVersion="9.0.0"),
            )

    page.route("**/api/update/install", install)
    load(page, update_status(phase="available", latestVersion="9.0.0"))
    open_about(page)
    page.click("#update-install")

    expect(page.locator("#update-status")).to_have_text(
        "更新安装冲突 (409)：请先断开目标设备"
    )
    expect(page.locator("#about-ver")).to_have_text("v0.5.2")
    expect(page.locator("#update-install")).to_be_enabled()
    expect(page.locator("#update-restart")).to_be_hidden()
    page.wait_for_timeout(150)
    assert len(attempts) == 1

    page.click("#update-install")
    expect(page.locator("#update-status")).to_have_text("请手动重启应用")
    assert len(attempts) == 2


def test_headless_ready_state_never_offers_or_requests_restart(page):
    restarts = []
    page.route(
        "**/api/update/restart",
        lambda route: (restarts.append(route.request), route.abort()),
    )
    load(
        page,
        update_status(phase="readyToRestart", latestVersion="9.0.0"),
        desktop=False,
    )
    open_about(page)

    expect(page.locator("#update-status")).to_have_text("请手动重启应用")
    expect(page.locator("#update-restart")).to_be_hidden()
    assert not page.locator("#update-install").is_enabled()
    assert restarts == []


def test_startup_status_is_non_blocking_and_never_installs(page):
    actions = []
    for action in ("check", "install", "restart"):
        page.route(
            f"**/api/update/{action}",
            lambda route: (actions.append(route.request.path), route.abort()),
        )
    load(
        page,
        update_status(
            phase="available", latestVersion="9.0.0", notesUrl=NOTES_URL
        ),
    )

    expect(page.locator("#version")).to_have_class(re.compile(r"update-available"))
    expect(page.locator("#version")).to_have_attribute("title", "发现新版本 9.0.0")
    expect(page.locator("#about")).to_be_hidden()
    page.wait_for_timeout(150)
    assert actions == []


def test_stale_background_status_cannot_overwrite_manual_check(page):
    deferred = []
    status_requests = []

    def status(route):
        status_requests.append(route.request)
        if len(status_requests) == 1:
            fulfill_json(route, update_status())
        else:
            deferred.append(route)

    page.route("**/api/update/status", status)
    page.route(
        "**/api/update/check",
        lambda route: fulfill_json(
            route, update_status(phase="available", latestVersion="9.0.0")
        ),
    )
    page.goto(BASE)
    page.wait_for_selector("#version", state="visible")
    for _ in range(40):
        if deferred:
            break
        page.wait_for_timeout(100)
    assert len(deferred) == 1

    open_about(page)
    page.click("#update-check")
    expect(page.locator("#update-status")).to_have_text("发现新版本 9.0.0")
    fulfill_json(deferred[0], update_status())
    expect(page.locator("#update-status")).to_have_text("发现新版本 9.0.0")
    expect(page.locator("#update-install")).to_be_enabled()


@pytest.mark.parametrize("desktop", [False, True], ids=["browser", "desktop-shell"])
def test_release_link_uses_server_notes_url_and_safe_open_flow(page, desktop):
    posted = []

    def open_browser(route):
        posted.append(route.request.post_data_json)
        route.fulfill(status=200, content_type="text/plain", body="")

    page.route("**/api/open-browser", open_browser)
    load(
        page,
        update_status(
            phase="available", latestVersion="9.0.0", notesUrl=NOTES_URL
        ),
        desktop=desktop,
    )
    page.evaluate(
        "() => { window.open = (url, target, features) => "
        "window.__openedUpdateLink = {url, target, features}; }"
    )
    open_about(page)
    expect(page.locator("#update-notes")).to_be_visible()
    page.click("#update-notes")

    if desktop:
        assert posted == [{"url": NOTES_URL}]
        assert page.evaluate("window.__openedUpdateLink || null") is None
    else:
        assert posted == []
        assert page.evaluate("window.__openedUpdateLink") == {
            "url": NOTES_URL,
            "target": "_blank",
            "features": "noopener",
        }


def test_release_data_is_rendered_as_text_not_html(page):
    hostile_version = '<img src=x onerror="window.__updateXss = true">'
    hostile_current = '<b id="injected-current">old</b>'
    load(
        page,
        update_status(
            currentVersion=hostile_current,
            phase="available",
            latestVersion=hostile_version,
            notesUrl=NOTES_URL,
        ),
    )
    open_about(page)

    expect(page.locator("#update-status")).to_have_text(
        f"发现新版本 {hostile_version}"
    )
    expect(page.locator("#about-ver")).to_have_text(f"v{hostile_current}")
    assert page.locator("#update-status img").count() == 0
    assert page.locator("#injected-current").count() == 0
    assert page.evaluate("window.__updateXss || false") is False


def test_delayed_initial_status_renders_with_about_already_open(page):
    deferred = []
    page.route("**/api/update/status", lambda route: deferred.append(route))
    page.goto(BASE)
    open_about(page)
    expect(page.locator("#update-check")).to_be_disabled()
    assert len(deferred) == 1

    fulfill_json(
        deferred[0], update_status(phase="available", latestVersion="9.0.0")
    )
    expect(page.locator("#update-status")).to_have_text("发现新版本 9.0.0")
    expect(page.locator("#update-install")).to_be_enabled()


def test_background_status_advances_while_about_is_open(page):
    status_requests = []
    deferred = []

    def status(route):
        status_requests.append(route.request)
        if len(status_requests) == 1:
            fulfill_json(route, update_status())
        else:
            deferred.append(route)

    page.route("**/api/update/status", status)
    page.goto(BASE)
    expect(page.locator("#update-check")).to_be_enabled()
    open_about(page)
    for _ in range(40):
        if deferred:
            break
        page.wait_for_timeout(100)
    assert len(deferred) == 1

    fulfill_json(
        deferred[0], update_status(phase="available", latestVersion="9.0.0")
    )
    expect(page.locator("#update-status")).to_have_text("发现新版本 9.0.0")
    expect(page.locator("#update-install")).to_be_enabled()


def test_failed_install_reconciles_error_then_check_allows_real_retry(page):
    server_state = {
        "status": update_status(phase="available", latestVersion="9.0.0")
    }
    install_attempts = []

    def status(route):
        fulfill_json(route, server_state["status"])

    def install(route):
        install_attempts.append(route.request)
        if len(install_attempts) == 1:
            server_state["status"] = update_status(
                phase="error", latestVersion="9.0.0", message="下载校验失败"
            )
            route.fulfill(
                status=500, content_type="text/plain", body="安装请求失败"
            )
        else:
            server_state["status"] = update_status(
                phase="readyToRestart", latestVersion="9.0.0"
            )
            fulfill_json(route, server_state["status"])

    def check(route):
        server_state["status"] = update_status(
            phase="available", latestVersion="9.0.0"
        )
        fulfill_json(route, server_state["status"])

    page.route("**/api/update/status", status)
    page.route("**/api/update/install", install)
    page.route("**/api/update/check", check)
    page.goto(BASE)
    expect(page.locator("#update-install")).to_be_enabled()
    open_about(page)
    page.click("#update-install")

    expect(page.locator("#update-status")).to_have_text("下载校验失败")
    expect(page.locator("#update-install")).to_be_hidden()
    expect(page.locator("#update-check")).to_be_enabled()

    page.click("#update-check")
    expect(page.locator("#update-status")).to_have_text("发现新版本 9.0.0")
    expect(page.locator("#update-install")).to_be_enabled()
    page.click("#update-install")
    expect(page.locator("#update-status")).to_have_text("请手动重启应用")
    assert len(install_attempts) == 2


def test_closed_about_polls_cannot_apply_newer_then_older_status(page):
    status_requests = []
    deferred = []

    def status(route):
        status_requests.append(route.request)
        if len(status_requests) == 1:
            fulfill_json(route, update_status())
        else:
            deferred.append(route)

    page.route("**/api/update/status", status)
    page.goto(BASE)
    expect(page.locator("#update-check")).to_be_enabled()
    for _ in range(50):
        if len(deferred) >= 2:
            break
        page.wait_for_timeout(100)
    assert len(deferred) >= 2

    fulfill_json(
        deferred[1], update_status(phase="available", latestVersion="9.0.0")
    )
    expect(page.locator("#version")).to_have_attribute("title", "发现新版本 9.0.0")
    fulfill_json(deferred[0], update_status())
    expect(page.locator("#version")).to_have_attribute("title", "发现新版本 9.0.0")
    expect(page.locator("#version")).to_have_class(re.compile(r"update-available"))


def test_ready_status_poll_reenables_restart_after_successful_request(page):
    ready = update_status(phase="readyToRestart", latestVersion="9.0.0")
    status_requests = []
    deferred = []
    restart_attempts = []

    def status(route):
        status_requests.append(route.request)
        if len(status_requests) == 1:
            fulfill_json(route, ready)
        else:
            deferred.append(route)

    def restart(route):
        restart_attempts.append(route.request)
        route.fulfill(status=200, content_type="text/plain", body="")

    page.route("**/api/update/status", status)
    page.route("**/api/update/restart", restart)
    page.add_init_script("window.__MINI_SHELL = true;")
    page.goto(BASE)
    page.wait_for_selector("#version", state="visible")
    open_about(page)
    page.click("#update-restart")

    expect(page.locator("#update-restart")).to_be_disabled()
    expect(page.locator("#update-status")).to_have_text("重启请求已发送")
    assert len(restart_attempts) == 1
    for _ in range(40):
        if deferred:
            break
        page.wait_for_timeout(100)
    assert deferred, "a fresh authoritative ready status is required"

    fulfill_json(deferred[0], ready)
    expect(page.locator("#update-restart")).to_be_enabled()
    expect(page.locator("#update-status")).to_have_text("更新已安装，请重启完成更新")
    assert len(restart_attempts) == 1

    page.click("#update-restart")
    expect(page.locator("#update-restart")).to_be_disabled()
    assert len(restart_attempts) == 2


def test_restart_conflict_allows_an_explicit_retry(page):
    attempts = []

    def restart(route):
        attempts.append(route.request)
        if len(attempts) == 1:
            route.fulfill(
                status=409, content_type="text/plain", body="当前状态不允许重启"
            )
        else:
            route.fulfill(status=200, content_type="text/plain", body="")

    page.route("**/api/update/restart", restart)
    load(
        page,
        update_status(phase="readyToRestart", latestVersion="9.0.0"),
        desktop=True,
    )
    open_about(page)
    page.click("#update-restart")

    expect(page.locator("#update-status")).to_have_text("当前状态不允许重启")
    expect(page.locator("#update-restart")).to_be_enabled()
    page.click("#update-restart")
    expect(page.locator("#update-status")).to_have_text("重启请求已发送")
    expect(page.locator("#update-restart")).to_be_disabled()
    assert len(attempts) == 2
