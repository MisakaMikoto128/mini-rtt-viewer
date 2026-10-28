"""三套仪器视觉主题的切换、持久化与滚动条回归。"""

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, QAServer


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("visual-themes-qa"))
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
    pg.wait_for_selector("#jlink", state="visible")
    yield pg
    pg.close()


@pytest.mark.parametrize("theme", ["instrument", "console", "laboratory"])
def test_new_visual_theme_persists_reload(page, theme):
    page.select_option("#theme", theme)
    assert page.locator("html").get_attribute("data-theme") == theme
    page.reload()
    page.wait_for_selector("#theme")
    assert page.locator("#theme").input_value() == theme
    assert page.locator("html").get_attribute("data-theme") == theme
    assert page.evaluate("localStorage.getItem('web-theme')") == theme


def test_scrollbars_are_three_pixels(page):
    for selector in ["#log", ".aside-scroll", "#chip-list"]:
        width = page.evaluate(
            "sel => getComputedStyle(document.querySelector(sel), "
            "'::-webkit-scrollbar').width",
            selector,
        )
        assert width == "3px", (selector, width)
