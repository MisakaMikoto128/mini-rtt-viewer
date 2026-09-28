"""后端可选面黑盒测试(docs/product/backend-abstraction.md §6 验证矩阵)。

两个 module 级服务形态,顺序执行(文件内普通形态用例在前、强制形态在后):
- 普通形态(无 --demo-log):jlink+demo 双后端可用;经 /api/settings 切到
  demo,走通 连接→收流→发送→连接态 409→复位→断开→切回/非法 id→偏好落盘
  全链。不触碰真实 J-Link 硬件(矩阵条目:普通形态不测 jlink connect 路径)。
- 强制形态(--demo-log):仅 demo 可用、启动即连接、选 jlink → 400。
  demo 启动自动周期 20s 后会自动断开——本模块断言在就绪后 15s 内完成,
  connected 断言带手动 connect 兜底(手动干预永久挂起自动周期,状态更确定)。

端口纪律:固定 18099,同一 module 内两个服务不能并存(否则撞端口,见
test_spec 的 server 注释)——forced_server 复用前先 stop 普通形态实例。
预期只来自 backend-abstraction.md §4 API 契约;隔离纪律同 qa_server.py
(RTT_PREFS_FILE+APPDATA 双临时目录、RTT_WEB_NO_BROWSER=1)。
"""

import time

import pytest
from playwright.sync_api import sync_playwright

from qa_server import BASE, QAServer, get_json, http, poll

CONNECT_TIMEOUT_S = 10.0
# /api/status device 8 字段键名(web.rs device_info_json 单一构造点,与
# device_info_json_keys_match_status_contract 单测同源)
DEVICE_KEYS = ("firmware", "hardware", "serial", "core",
               "cpu", "target", "iface", "speedKhz")


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    """普通形态:jlink+demo 双后端(start(demo_log=False),默认行为不变)。"""
    srv = QAServer(tmp_path_factory.mktemp("backends-qa"))
    srv.start(demo_log=False)
    yield srv
    srv.stop()


@pytest.fixture(scope="module")
def forced_server(server, tmp_path_factory):
    """强制形态:先停掉普通形态实例(释放 18099),再以 --demo-log 拉起
    独立实例(独立 prefs 临时目录,两形态互不残留)。依赖 server fixture
    保证普通形态先行起服/先执行(文件内用例顺序即依赖顺序)。"""
    server.stop()
    srv = QAServer(tmp_path_factory.mktemp("backends-forced-qa"))
    srv.start()  # 默认 demo_log=True → --demo-log 强制形态
    yield srv
    srv.stop()
    # server fixture 自身的 teardown 会再 stop 一次:no-op(proc 已 None)


def _status() -> dict:
    return get_json("/api/status")


def _backends() -> dict:
    return get_json("/api/backends")


def _wait_connected(timeout_s: float) -> bool:
    return poll(lambda: _status()["connected"], True, timeout_s)


# ---- 普通形态(无 --demo-log):jlink+demo 双后端 ----

def test_backends_1a_normal_backends_contract(server):
    """/api/backends:selected=jlink、items 恰 2 项,每项含 id/name/
    description/isVirtual 且值正确(jlink False、demo True)。"""
    data = _backends()
    assert data["selected"] == "jlink", (
        f"普通形态默认应选定 jlink:实测 {data['selected']!r}"
    )
    items = data["items"]
    assert len(items) == 2, f"items 应恰 2 项:实测 {len(items)} {items}"
    by_id = {it["id"]: it for it in items}
    assert set(by_id) == {"jlink", "demo"}, (
        f"item ids 应为 jlink+demo:实测 {sorted(by_id)}"
    )
    for it in items:
        for key in ("id", "name", "description", "isVirtual"):
            assert key in it, f"item 缺字段 {key}:实测 {it}"
        assert isinstance(it["name"], str) and it["name"], f"name 应非空:{it}"
        assert isinstance(it["description"], str) and it["description"], (
            f"description 应非空:{it}"
        )
    assert by_id["jlink"]["isVirtual"] is False, (
        f"jlink 应 isVirtual=False:实测 {by_id['jlink']}"
    )
    assert by_id["demo"]["isVirtual"] is True, (
        f"demo 应 isVirtual=True:实测 {by_id['demo']}"
    )


def test_backends_1b_normal_status_has_backend_jlink(server):
    """/api/status 含 backend 键且普通形态默认 =="jlink"。"""
    status = _status()
    assert "backend" in status, (
        f"/api/status 应含 backend 键:实测 {sorted(status)}"
    )
    assert status["backend"] == "jlink", (
        f"普通形态默认 backend 应为 jlink:实测 {status['backend']!r}"
    )


def test_backends_3a_settings_switch_to_demo(server):
    """POST /api/settings {"backend":"demo"} → 200;随后 status.backend=="demo"。"""
    code, text = http("POST", "/api/settings", {"backend": "demo"})
    assert code == 200, f"断开态切 demo 应 200:实测 {code} {text}"
    assert poll(lambda: _status()["backend"], "demo", 3.0), (
        f"切换后 status.backend 应为 demo:实测 {_status()['backend']!r}"
    )


def test_backends_3b_connect_demo_device_info_8_fields(server):
    """POST /api/connect {"chip":"STM32F103C8"} → 200;轮询 connected==True
    (10s 超时);device 非 null 且 8 字段全非空。"""
    code, text = http("POST", "/api/connect", {"chip": "STM32F103C8"})
    assert code == 200, f"demo connect 应 200:实测 {code} {text}"
    assert _wait_connected(CONNECT_TIMEOUT_S), (
        f"{CONNECT_TIMEOUT_S}s 内应 connected=true:实测 {_status()}"
    )

    def connected_with_device():
        d = _status()
        return d["connected"] is True and d["device"] is not None

    assert poll(connected_with_device, True, 3.0), (
        f"连接后 device 应非 null:实测 {_status()['device']!r}"
    )
    device = _status()["device"]
    empty = {k: device.get(k) for k in DEVICE_KEYS
             if device.get(k) is None or device.get(k) == ""}
    assert not empty, f"device 8 字段应全非空:缺失/空 {empty}(全量 {device})"


def test_backends_3c_rows_total_grows(server):
    """连接后 rowsTotal 增长(demo 流进泵;两次采样,远离 500 环形上限)。"""
    n1 = _status()["rowsTotal"]
    time.sleep(1.5)
    n2 = _status()["rowsTotal"]
    assert n2 > n1, f"rowsTotal 应增长:实测 {n1} → {n2}"


def test_backends_3d_send_grows_tx_bytes(server):
    """POST /api/send {"text":"hi","ending":1} → 200 且 txBytes 增长。

    (SendReq 契约字段为 text/hex,多余键被 serde 忽略;行尾实际取
    sendEnding 设置——本用例只断言发送成功与 TX 计数增长。)"""
    tx0 = _status()["txBytes"]
    code, text = http("POST", "/api/send", {"text": "hi", "ending": 1})
    assert code == 200, f"/api/send 应 200:实测 {code} {text}"
    tx1 = _status()["txBytes"]
    deadline = time.time() + 3
    while time.time() < deadline and tx1 - tx0 == 0:
        time.sleep(0.2)
        tx1 = _status()["txBytes"]
    assert tx1 > tx0, f"发送后 txBytes 应增长:实测 {tx0} → {tx1}"


def test_backends_3e_switch_backend_while_connected_409(server):
    """连接态 POST /api/settings {"backend":"jlink"} → 409。"""
    assert _status()["connected"] is True, "前置:应处于连接态"
    code, text = http("POST", "/api/settings", {"backend": "jlink"})
    assert code == 409, f"连接态切后端应 409:实测 {code} {text}"


def test_backends_3f_reset_in_place_200(server):
    """连接态 POST /api/reset {} → 200(demo 不支持 reconnect,回落 in-place)。"""
    code, text = http("POST", "/api/reset", {})
    assert code == 200, f"连接态 reset 应 200:实测 {code} {text}"


def test_backends_3g_disconnect_polls_not_connected(server):
    """POST /api/disconnect → 200,轮询 connected==False。"""
    code, text = http("POST", "/api/disconnect")
    assert code == 200, f"disconnect 应 200:实测 {code} {text}"
    assert poll(lambda: _status()["connected"], False, 5.0), (
        f"断开后应 connected=false:实测 {_status()['connected']!r}"
    )


def test_backends_3h_switch_back_jlink_ok_invalid_id_400(server):
    """断开态切回 jlink → 200;{"backend":"nope"}(未知 id)→ 400。"""
    code, text = http("POST", "/api/settings", {"backend": "jlink"})
    assert code == 200, f"断开态切回 jlink 应 200:实测 {code} {text}"
    assert poll(lambda: _status()["backend"], "jlink", 3.0), (
        f"切回后 status.backend 应为 jlink:实测 {_status()['backend']!r}"
    )
    code, text = http("POST", "/api/settings", {"backend": "nope"})
    assert code == 400, f"未知后端 id 应 400:实测 {code} {text}"


def test_backends_3i_backend_pref_persisted_within_3s(server):
    """偏好落盘:settings demo 后 ≤3s 内 prefs 文件出现 "backend":"demo"。"""
    code, text = http("POST", "/api/settings", {"backend": "demo"})
    assert code == 200, f"断开态切 demo 应 200:实测 {code} {text}"
    # prefs_on_disk 自带 remove→rename 窗口抖动重试;文件未建时抛错由
    # poll 吞掉重试,3s 覆盖 500ms tick 快照落盘节奏
    assert poll(lambda: server.prefs_on_disk().get("backend"), "demo",
                3.0, tick=0.1), (
        f"3s 内 prefs 应落盘 backend=demo:实测 "
        f"{server.prefs_on_disk().get('backend')!r}"
    )


# ---- 界面闭环(浏览器真实操作「数据源」下拉;WP-B 前端接线的黑盒验证)----

def test_backends_4_ui_backend_selector_end_to_end(server):
    """浏览器实测界面选后端:下拉渲染双后端 → 切 demo(400ms 防抖
    postPref 入库,status.backend 跟变,J-Link 下拉联动禁用)→ 连接后
    后端下拉随连接态禁用 → 断开恢复且 UI 选择保持。
    自建全新浏览器上下文(localStorage 必为空)+ 先经 API 归位 jlink,
    排除同 server 前序用例(3i 把偏好切到 demo)的回填干扰。"""
    code, text = http("POST", "/api/settings", {"backend": "jlink"})
    assert code == 200, f"前置归位 jlink 应 200:实测 {code} {text}"
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_context(
            viewport={"width": 1440, "height": 900}).new_page()
        page.goto(BASE)
        page.wait_for_function(
            "document.querySelectorAll('#backend option').length === 2",
            timeout=10000,
        )  # fillBackends 初次拉取 /api/backends(网络往返)后 options 就位
        assert poll(lambda: page.locator("#backend").input_value(), "jlink",
                    3.0), (
            f"初值应 jlink:实测 {page.locator('#backend').input_value()!r}"
        )

        page.locator("#backend").select_option("demo")
        assert poll(lambda: _status()["backend"], "demo", 5.0), (
            f"UI 切换应入库并生效:实测 {_status()['backend']!r}"
        )
        page.wait_for_function(
            "document.getElementById('jlink').disabled", timeout=3000
        )  # demo 选中时 J-Link 下拉联动禁用(demo 无硬件探针列表)

        page.fill("#chip", "STM32F103C8")
        page.click("#connect-btn")
        assert _wait_connected(CONNECT_TIMEOUT_S), (
            f"UI 连接后应 connected=true:实测 {_status()}"
        )
        page.wait_for_function(
            "document.getElementById('backend').disabled", timeout=3000
        )  # 连接态后端下拉禁用(setState 禁用列表)

        page.click("#connect-btn")
        assert poll(lambda: _status()["connected"], False, 5.0), (
            f"UI 断开后应 connected=false:实测 {_status()['connected']!r}"
        )
        page.wait_for_function(
            "!document.getElementById('backend').disabled", timeout=3000
        )  # 断开后后端下拉恢复可选
        assert page.locator("#backend").input_value() == "demo", (
            f"断开后 UI 选择应保持:实测 {page.locator('#backend').input_value()!r}"
        )
        browser.close()


# ---- 强制形态(--demo-log):仅 demo、启动即连接 ----

def test_backends_2a_forced_backends_only_demo(forced_server):
    """强制形态 /api/backends:items 恰 1 项(demo)、selected=="demo"。"""
    data = _backends()
    assert data["selected"] == "demo", (
        f"强制形态应选定 demo:实测 {data['selected']!r}"
    )
    items = data["items"]
    assert len(items) == 1, f"强制形态 items 应恰 1 项:实测 {items}"
    assert items[0]["id"] == "demo", f"唯一 item 应为 demo:实测 {items[0]}"
    assert items[0]["isVirtual"] is True, (
        f"demo 应 isVirtual=True:实测 {items[0]}"
    )


def test_backends_2b_forced_connected_at_startup(forced_server):
    """强制形态就绪后 connected==True(15s 窗口内;若撞上 20s 自动断开
    窗口则以手动 connect 兜底——手动干预挂起自动周期,后续状态确定)。"""
    if not _wait_connected(1.0):
        code, text = http("POST", "/api/connect", {"chip": "STM32F103C8"})
        assert code == 200, f"兜底手动 connect 应 200:实测 {code} {text}"
    assert _wait_connected(CONNECT_TIMEOUT_S), (
        f"强制形态就绪后应 connected=true:实测 {_status()}"
    )


def test_backends_2c_forced_select_jlink_rejected_400(forced_server):
    """强制形态选 jlink(已知 id 但被形态排除)→ 400。"""
    code, text = http("POST", "/api/settings", {"backend": "jlink"})
    assert code == 400, f"强制形态选 jlink 应 400:实测 {code} {text}"
