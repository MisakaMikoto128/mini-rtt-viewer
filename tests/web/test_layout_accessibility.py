"""主界面布局、响应式与键盘可访问性回归。"""

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, QAServer


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("layout-a11y-qa"))
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
    pg.wait_for_selector("#backend option", state="attached")
    yield pg
    pg.close()


@pytest.mark.parametrize(("width", "height"), [(1440, 900), (1024, 700), (800, 600)])
def test_viewport_has_no_horizontal_overflow(page, width, height):
    page.set_viewport_size({"width": width, "height": height})
    page.wait_for_timeout(150)
    assert page.evaluate("document.documentElement.scrollWidth <= innerWidth")
    for selector in ["#send-opts", "#log-toolbar", "footer"]:
        assert page.evaluate(
            "sel => { const el = document.querySelector(sel); "
            "return el.scrollWidth <= el.clientWidth + 1; }",
            selector,
        ), selector


def test_log_actions_belong_to_main_content(page):
    for selector in ["#clear-btn", "#pause-btn", "#mark-btn", "#export-btn", "#theme"]:
        assert page.evaluate(
            "sel => document.querySelector('#content').contains(document.querySelector(sel))",
            selector,
        ), selector


def test_narrow_sidebar_is_keyboard_controllable(page):
    page.set_viewport_size({"width": 800, "height": 600})
    toggle = page.locator("#sidebar-toggle")
    page.wait_for_function("document.querySelector('#sidebar-toggle').getAttribute('aria-expanded') === 'false'")
    assert toggle.get_attribute("aria-expanded") == "false"
    toggle.click()
    assert toggle.get_attribute("aria-expanded") == "true"
    assert page.locator("aside").is_visible()
    page.keyboard.press("Escape")
    assert toggle.get_attribute("aria-expanded") == "false"


def test_timer_interval_only_appears_when_timer_is_enabled(page):
    assert page.locator("#timer-fields").is_hidden()
    page.check("#timer-on")
    assert page.locator("#timer-fields").is_visible()
    page.uncheck("#timer-on")
    assert page.locator("#timer-fields").is_hidden()


def test_combobox_and_search_controls_expose_keyboard_state(page):
    page.click("#connect-btn")
    page.locator("#chip").wait_for(state="visible")
    page.wait_for_function("!document.querySelector('#chip').disabled")
    assert page.locator("#chip").get_attribute("role") == "combobox"
    assert page.locator("#chip-list").get_attribute("role") == "listbox"
    page.locator("#chip-arrow").focus()
    page.keyboard.press("Enter")
    assert page.locator("#chip").get_attribute("aria-expanded") == "true"
    page.keyboard.press("Escape")
    page.keyboard.press("Control+f")
    page.fill("#s-input", "Heartbeat")
    page.wait_for_function("!document.querySelector('#s-next').disabled")
    for selector in ["#s-case", "#s-regex", "#s-prev", "#s-next", "#s-close"]:
        assert page.locator(selector).get_attribute("tabindex") is None
    assert page.locator("#s-case").get_attribute("aria-pressed") is not None

    page.keyboard.press("Shift+Tab")
    page.keyboard.press("Shift+Tab")
    assert page.evaluate("document.activeElement.id") == "s-case"
    assert page.evaluate(
        "document.querySelector('#s-case').matches(':focus-visible') && "
        "getComputedStyle(document.querySelector('#s-case')).outlineWidth === '2px'"
    )
    for expected in ["s-regex", "s-input", "s-prev", "s-next", "s-close"]:
        page.keyboard.press("Tab")
        assert page.evaluate("document.activeElement.id") == expected


def test_closing_search_preserves_log_selection(page):
    page.click("#pause-btn")
    page.wait_for_function("document.querySelector('#pause-btn').getAttribute('aria-pressed') === 'true'")
    page.keyboard.press("Control+f")
    page.fill("#s-input", "Heartbeat")
    page.wait_for_selector("#log mark.s-hit")
    selected = page.evaluate(
        """() => {
          const mark = document.querySelector('#log mark.s-hit');
          const range = document.createRange();
          range.selectNodeContents(mark);
          const selection = window.getSelection();
          selection.removeAllRanges();
          selection.addRange(range);
          return selection.toString();
        }"""
    )
    assert selected == "Heartbeat"
    page.keyboard.press("Escape")
    page.wait_for_timeout(200)
    assert page.evaluate("window.getSelection().toString()") == selected


def test_muted_text_meets_contrast_in_every_builtin_theme(page):
    def channel(value):
        value = value / 255
        return value / 12.92 if value <= 0.04045 else ((value + 0.055) / 1.055) ** 2.4

    def luminance(rgb):
        values = [int(part) for part in rgb.replace("rgb(", "").replace(")", "").split(",")]
        r, g, b = [channel(value) for value in values]
        return 0.2126 * r + 0.7152 * g + 0.0722 * b

    for theme in ["dark", "light", "oled", "sepia"]:
        page.select_option("#theme", theme)
        page.wait_for_timeout(50)
        muted, background = page.evaluate(
            """() => {
              const style = getComputedStyle(document.documentElement);
              return [style.getPropertyValue('--muted').trim(),
                      style.getPropertyValue('--log-bg').trim()];
            }"""
        )
        # 令牌当前是十六进制，浏览器计算后统一转成 rgb，避免测试依赖表示形式。
        muted, background = page.evaluate(
            """([muted, background]) => {
              const probe = document.createElement('span');
              document.body.appendChild(probe);
              probe.style.color = muted;
              const fg = getComputedStyle(probe).color;
              probe.style.color = background;
              const bg = getComputedStyle(probe).color;
              probe.remove();
              return [fg, bg];
            }""",
            [muted, background],
        )
        first, second = luminance(muted), luminance(background)
        contrast = (max(first, second) + 0.05) / (min(first, second) + 0.05)
        assert contrast >= 4.5, (theme, contrast)
