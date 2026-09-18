"""验收矩阵 1:主题插件化(黑盒,预期来自验收矩阵条目)。

1a. 服务运行中 exe 旁 themes/ 放自定义 css → /api/themes 出现(custom:true)
    → 前端下拉出现并可选 → 选后面板变该色 → 服务运行中删除该 css → 消失
1b. 与内置主题同名的 css 不出现在列表(防覆盖内置)
1c. 主题 css 路径穿越 /api/theme-css/..%2F..%2FREADME.md → 404
"""

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, THEMES_DIR, QAServer, get_json, http, poll

# 矩阵给定的自定义主题内容(--panel:#112233 等 13 个 token)
QA_CSS = (
    ":root{--panel:#112233;--bg:#0a0f0a;--card:#182018;--text:#cceecc;"
    "--input:#0e140e;--border:#223322;--accent:#44cc88;--secondary:#88aa88;"
    "--muted:#668866;--ok:#44cc66;--err:#ff6666;--log-bg:#0a120a;--log-fg:#cceecc}"
)
PANEL_RGB = "rgb(17, 34, 51)"  # #112233 的 getComputedStyle 形式
BUILTIN_IDS = {"dark", "light", "oled", "sepia"}


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("theme-qa"))
    srv.start()
    yield srv
    srv.stop()


@pytest.fixture(scope="module")
def page(server):
    with sync_playwright() as p:
        browser = p.chromium.launch()
        pg = browser.new_page(viewport={"width": 1440, "height": 900})
        pg.goto(BASE)
        pg.wait_for_selector("#theme", state="visible")
        yield pg
        browser.close()


def _theme_ids() -> set[str]:
    return {t["id"] for t in get_json("/api/themes")}


def _custom_ids() -> set[str]:
    return {t["id"] for t in get_json("/api/themes") if t.get("custom")}


def _write_theme(name: str, content: str) -> None:
    THEMES_DIR.mkdir(exist_ok=True)
    (THEMES_DIR / name).write_text(content, encoding="utf-8")


def _remove_theme(name: str) -> None:
    (THEMES_DIR / name).unlink(missing_ok=True)


@pytest.fixture(autouse=True)
def _cleanup_themes():
    """每个用例前后清理测试主题文件(README.md 不在 exe 旁目录,无冲突)。"""
    for f in ("qa_custom.css", "dark.css"):
        _remove_theme(f)
    yield
    for f in ("qa_custom.css", "dark.css"):
        _remove_theme(f)


def test_theme_1a_custom_hot_lifecycle(server, page):
    """1a:运行中放置 → 列表+custom:true → 下拉可选 → 面板变色 → 运行中删除 → 消失。"""
    _write_theme("qa_custom.css", QA_CSS)
    assert poll(_theme_ids, {"qa_custom"} | BUILTIN_IDS, 5), (
        f"放置后 5s 内 /api/themes 应出现 qa_custom:实测 {_theme_ids()}"
    )
    entry = next(t for t in get_json("/api/themes") if t["id"] == "qa_custom")
    assert entry["custom"] is True, f"自定义主题应 custom:true:实测 {entry}"
    assert entry["name"] == "qa_custom.css", f"文件名应透出为 name:实测 {entry}"

    status, css = http("GET", "/api/theme-css/qa_custom")
    assert status == 200 and "--panel:#112233" in css, (
        f"自定义主题 css 应可获取:status={status} body[:80]={css[:80]!r}"
    )

    # 前端:下拉出现并可选 → 选后面板变该色
    page.reload()
    page.wait_for_selector("#theme", state="visible")
    opts = page.evaluate("[...document.querySelectorAll('#theme option')].map(o=>o.value)")
    assert "qa_custom" in opts, f"前端下拉应出现 qa_custom:实测 {opts}"
    page.select_option("#theme", "qa_custom")
    page.wait_for_function(
        "getComputedStyle(document.querySelector('aside')).backgroundColor"
        " === 'rgb(17, 34, 51)'", timeout=3000,
    )
    colors = page.evaluate(
        """(() => {
            const v = n => getComputedStyle(document.documentElement)
                .getPropertyValue(n).trim();
            return {panel: v('--panel'), asideBg:
                getComputedStyle(document.querySelector('aside')).backgroundColor};
        })()"""
    )
    assert colors["panel"] == "#112233", f"--panel 应为 #112233:实测 {colors}"
    assert colors["asideBg"] == PANEL_RGB, f"面板底色应为 #112233:实测 {colors}"
    page.select_option("#theme", "dark")  # 还原,避免残留进 1b/1c

    # 服务运行中删除 → /api/themes 消失
    _remove_theme("qa_custom.css")
    assert poll(_custom_ids, set(), 5), (
        f"删除后 5s 内 qa_custom 应从 /api/themes 消失:实测 {_custom_ids()}"
    )


def test_theme_1b_builtin_name_not_shadowed(server, page):
    """1b:与内置同名的 css 不出现在列表(防覆盖内置)。

    断言分层:列表不含同名 custom 项 + UI 选中内置 dark 时仍是内置色板
    (前端内置主题不走 /api/theme-css,无覆盖)。
    注:/api/theme-css/dark 会解析到用户同名文件(200),属 API 层潜在遮蔽,
    前端未触发,记录于验收报告,不作为本条 FAIL 依据。
    """
    _write_theme("dark.css", QA_CSS)
    entries = get_json("/api/themes")
    darks = [t for t in entries if t["id"] == "dark"]
    assert len(darks) == 1, f"dark 应只有内置一项,不得被同名 css 覆盖/重复:实测 {darks}"
    assert darks[0]["custom"] is False, f"内置 dark 应保持 custom:false:实测 {darks}"
    assert "dark" not in _custom_ids(), (
        f"同名 css 不应作为 custom 主题出现:实测 custom 列表 {_custom_ids()}"
    )
    # UI:选中内置 dark,面板仍为内置色板(#0E1013),未被用户文件覆盖
    page.reload()
    page.wait_for_selector("#theme", state="visible")
    page.select_option("#theme", "dark")
    page.wait_for_function(
        "getComputedStyle(document.querySelector('aside')).backgroundColor"
        " === 'rgb(14, 16, 19)'", timeout=3000,
    )
    panel = page.evaluate(
        "getComputedStyle(document.documentElement).getPropertyValue('--panel').trim()"
    )
    assert panel == "#0E1013", f"内置 dark 色板不得被同名 css 覆盖:实测 --panel={panel}"


def test_theme_1c_path_traversal_404(server):
    """1c:主题 css 路径穿越 → 404(README.md 及其他任意穿越形态)。"""
    _write_theme("qa_custom.css", QA_CSS)  # 端点在有自定义主题时也应保持拒绝
    assert poll(_custom_ids, {"qa_custom"}, 5)
    for evil in (
        "..%2F..%2FREADME.md",          # 矩阵原始形态
        "..%5C..%5CREADME.md",          # 反斜杠变体
        "%2e%2e%2f%2e%2e%2fREADME.md",  # 点号编码变体
        "qa_custom%2F..%2F..%2FREADME.md",
    ):
        status, body = http("GET", f"/api/theme-css/{evil}")
        assert status == 404, f"/api/theme-css/{evil} 应 404:实测 {status} {body[:80]!r}"
