"""极简音效:用户手势解锁、短正弦波、静音持久化。"""

import json
import urllib.request

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, QAServer


FAKE_AUDIO = """
window.__tones = [];
window.__audioContextCount = 0;
class FakeParam {
  constructor() { this.value = 0; }
  setValueAtTime(value) { this.value = value; }
  exponentialRampToValueAtTime(value) { this.value = value; }
}
class FakeOscillator {
  constructor() { this.frequency = new FakeParam(); this.type = ''; }
  connect() {}
  disconnect() {}
  start(time) { this.startedAt = time; window.__tones.push(this); }
  stop(time) { this.stoppedAt = time; }
}
class FakeGain {
  constructor() { this.gain = new FakeParam(); }
  connect() {}
  disconnect() {}
}
class FakeAudioContext {
  constructor() {
    window.__audioContextCount++;
    this.state = 'suspended';
    this.currentTime = 10;
    this.destination = {};
  }
  resume() { this.state = 'running'; return Promise.resolve(); }
  createOscillator() { return new FakeOscillator(); }
  createGain() { return new FakeGain(); }
}
window.AudioContext = FakeAudioContext;
window.webkitAudioContext = FakeAudioContext;
"""


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("sound-qa")
    srv = QAServer(
        tmp,
        extra_env={
            "RTT_LOG_DIR": str(tmp / "logs"),
            "RTT_DISABLE_SHELL_OPEN": "1",
        },
    )
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
    pg.add_init_script(FAKE_AUDIO)
    pg.goto(BASE)
    status = json.load(urllib.request.urlopen(BASE + "/api/status"))
    if not status["connected"]:
        pg.click("#connect-btn")
    pg.wait_for_selector("#send-btn:not([disabled])")
    yield pg
    pg.close()


def send(page, text="ping"):
    page.fill("#send-text", text)
    with page.expect_response(lambda r: "/api/send" in r.url, timeout=5000) as response:
        page.click("#send-btn")
    assert response.value.status == 200


def test_fr29_no_audio_or_log_noise_before_user_gesture(page):
    page.wait_for_selector("#log .row")
    page.wait_for_timeout(200)
    assert page.evaluate("window.__audioContextCount") == 0
    assert page.evaluate("window.__tones.length") == 0


def test_fr29_successful_send_plays_one_short_sine_tone(page):
    send(page)
    page.wait_for_function("window.__tones.length === 1")
    tone = page.evaluate("""() => {
      const t = window.__tones[0];
      return {type: t.type, frequency: t.frequency.value,
              durationMs: (t.stoppedAt - t.startedAt) * 1000};
    }""")
    assert tone["type"] == "sine"
    assert 800 <= tone["frequency"] <= 1000
    assert 20 <= tone["durationMs"] <= 100
    assert page.input_value("#send-text") == "ping"


def test_fr29_connection_and_disconnection_use_distinct_single_tones(page):
    page.click("#connect-btn")
    page.wait_for_function("document.querySelector('#connect-btn').textContent.trim() === '连接'")
    page.wait_for_function("window.__tones.length === 1")
    page.click("#connect-btn")
    page.wait_for_function("document.querySelector('#connect-btn').textContent.trim() === '断开'")
    page.wait_for_function("window.__tones.length === 2")
    tones = page.evaluate(
        "window.__tones.map(t => ({type: t.type, frequency: t.frequency.value}))"
    )
    assert [tone["type"] for tone in tones] == ["sine", "sine"]
    assert tones[0]["frequency"] < tones[1]["frequency"]


def test_fr29_sound_switch_persists_and_mutes_feedback(page):
    page.uncheck("#sound-enabled")
    assert page.evaluate("localStorage.getItem('web-sound')") == "0"
    page.reload()
    page.wait_for_selector("#send-btn:not([disabled])")
    assert not page.locator("#sound-enabled").is_checked()
    send(page)
    page.wait_for_timeout(200)
    assert page.evaluate("window.__tones.length") == 0

    page.check("#sound-enabled")
    assert page.evaluate("localStorage.getItem('web-sound')") == "1"
    page.wait_for_function("window.__tones.length === 1")
    page.reload()
    page.wait_for_selector("#sound-enabled")
    assert page.locator("#sound-enabled").is_checked()


def test_fr29_send_error_uses_one_low_short_tone(page):
    page.route("**/api/send", lambda route: route.fulfill(status=500, body="测试失败"))
    page.fill("#send-text", "ping")
    page.click("#send-btn")
    page.wait_for_selector("#toast.show")
    page.wait_for_function("window.__tones.length === 1")
    tone = page.evaluate("""() => {
      const t = window.__tones[0];
      return {type: t.type, frequency: t.frequency.value,
              durationMs: (t.stoppedAt - t.startedAt) * 1000};
    }""")
    assert tone["type"] == "sine"
    assert 180 <= tone["frequency"] <= 300
    assert 20 <= tone["durationMs"] <= 120


def test_fr29_receive_actions_each_play_exactly_once(page):
    for index, selector in enumerate(["#clear-btn", "#pause-btn", "#pause-btn", "#mark-btn"], 1):
        page.click(selector)
        page.wait_for_function(f"window.__tones.length === {index}")
    page.wait_for_timeout(300)
    assert page.evaluate("window.__tones.length") == 4
    assert page.evaluate("window.__tones.every(t => t.type === 'sine')")


def test_fr29_reset_plays_once_without_connection_tone_duplicates(page):
    page.click("#reset-btn")
    page.wait_for_timeout(1400)
    assert page.evaluate("window.__tones.length") == 1
    assert page.locator("#connect-btn").inner_text() == "断开"


def test_fr29_display_and_setting_changes_each_play_once(page):
    page.click("#A-plus")
    page.select_option("#theme", "console")
    page.uncheck("#auto-scroll")
    page.check("#hex-rx")
    page.wait_for_function("window.__tones.length === 4")
    page.wait_for_timeout(300)
    assert page.evaluate("window.__tones.length") == 4


def test_fr29_export_and_open_actions_each_play_once(page):
    with page.expect_response(lambda r: "/api/logs/save" in r.url, timeout=5000):
        page.click("#export-btn")
    page.wait_for_function("window.__tones.length === 1")
    with page.expect_response(lambda r: "/api/logs/open" in r.url, timeout=5000):
        page.click("#open-logs")
    page.wait_for_function("window.__tones.length === 2")
    page.route("**/api/open-browser", lambda route: route.fulfill(status=200, body=""))
    page.click("#open-browser")
    page.wait_for_function("window.__tones.length === 3")
    page.wait_for_timeout(300)
    assert page.evaluate("window.__tones.length") == 3


def test_fr29_dialog_open_and_clipboard_copy_each_play_once(page):
    page.context.grant_permissions(["clipboard-read", "clipboard-write"])
    page.click("#version")
    page.wait_for_function("window.__tones.length === 1")
    page.click("#about-close")
    page.click("#remote-btn")
    page.wait_for_function("window.__tones.length === 2")
    page.click("#remote-card-local")
    page.wait_for_function("window.__tones.length === 3")
    page.wait_for_timeout(300)
    assert page.evaluate("window.__tones.length") == 3


def test_fr29_power_timestamp_and_timer_settings_play_without_timer_send_noise(page):
    page.check("#timestamps")
    page.wait_for_function("window.__tones.length === 1")
    page.check("#power")
    page.wait_for_function("window.__tones.length === 2")
    page.fill("#send-text", "ping")
    page.check("#timer-on")
    page.wait_for_function("window.__tones.length === 3")
    page.wait_for_timeout(1200)
    assert page.evaluate("window.__tones.length") == 3
    page.uncheck("#timer-on")
    page.wait_for_function("window.__tones.length === 4")


def test_fr29_failed_workspace_actions_each_play_only_one_error_tone(page):
    for index, (selector, pattern) in enumerate([
        ("#export-btn", "**/api/logs/save"),
        ("#open-logs", "**/api/logs/open"),
        ("#open-browser", "**/api/open-browser"),
    ], 1):
        page.route(pattern, lambda route: route.fulfill(status=500, body="测试失败"))
        page.click(selector)
        page.wait_for_function(f"window.__tones.length === {index}")
        page.unroute(pattern)
    page.wait_for_timeout(300)
    assert page.evaluate("window.__tones.length") == 3
    assert page.evaluate(
        "window.__tones.every(t => t.type === 'sine' && t.frequency.value === 220)"
    )
