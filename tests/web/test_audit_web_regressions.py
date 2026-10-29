"""Web 状态、统计与持久化回归；全部服务配置均隔离到 pytest 临时目录。"""

import json
import re
import time

import pytest

from qa_server import QAServer, get_json, http, poll


@pytest.fixture
def server(tmp_path):
    srv = QAServer(
        tmp_path,
        extra_env={
            "RTT_LOG_DIR": str(tmp_path / "logs"),
            "RTT_DISABLE_SHELL_OPEN": "1",
        },
    )
    srv.start()
    yield srv
    srv.stop()


def test_disconnected_send_has_no_state_side_effects(server):
    assert poll(lambda: get_json("/api/status")["connected"], True, 3.0)
    assert http("POST", "/api/send", {"text": "timer-seed"})[0] == 200
    assert http("POST", "/api/disconnect")[0] == 200
    assert poll(lambda: get_json("/api/status")["connected"], False, 3.0)

    before_status = get_json("/api/status")
    before_history = get_json("/api/history")
    before_export = http("GET", "/api/export")[1]
    before_timer = get_json("/api/prefs")["timerText"]

    status, _ = http("POST", "/api/send", {"text": "must-not-be-echoed"})
    assert status == 409
    assert get_json("/api/status")["txBytes"] == before_status["txBytes"]
    assert get_json("/api/history") == before_history
    assert "must-not-be-echoed" not in http("GET", "/api/export")[1]
    assert "must-not-be-echoed" not in before_export
    assert get_json("/api/prefs")["timerText"] == before_timer

    # HEX 输入错误仍是 400，不被不可用发送通道错误覆盖。
    assert http("POST", "/api/send", {"text": "abc", "hex": True})[0] == 400


def test_settings_validate_backend_before_applying_any_field(server):
    assert http("POST", "/api/settings", {"rxEnding": 2})[0] == 200

    status, _ = http(
        "POST", "/api/settings", {"rxEnding": 1, "backend": "not-registered"}
    )
    assert status == 400
    assert get_json("/api/prefs")["rxEnding"] == 2

    status, _ = http("POST", "/api/settings", {"rxEnding": 1, "backend": "qa"})
    assert status == 409
    assert get_json("/api/prefs")["rxEnding"] == 2

    assert poll(lambda: get_json("/api/status")["connected"], True, 3.0)
    status, _ = http("POST", "/api/settings", {"rxEnding": 1, "backend": "qa"})
    assert status == 409
    assert get_json("/api/prefs")["rxEnding"] == 2


def test_timer_text_and_hex_persist_across_restart(server):
    status, _ = http(
        "POST",
        "/api/timer",
        {"on": False, "intervalSec": 0.25, "text": "41 42", "hex": True},
    )
    assert status == 200

    def saved():
        prefs = server.prefs_on_disk()
        return prefs.get("timer_text"), prefs.get("timer_hex")

    assert poll(saved, ("41 42", True), 3.0, tick=0.1)
    server.restart()
    prefs = get_json("/api/prefs")
    assert prefs["timerText"] == "41 42"
    assert prefs["timerHex"] is True
    assert prefs["timerIntervalSec"] == 0.25

    assert http("POST", "/api/timer", {"on": True})[0] == 200
    assert poll(lambda: server.prefs_on_disk().get("timer_send"), True, 3.0)
    server.restart()
    prefs = get_json("/api/prefs")
    assert prefs["timerOn"] is True
    assert prefs["timerText"] == "41 42"
    assert prefs["timerHex"] is True


def test_legacy_empty_timer_does_not_restart_into_send_loop(server):
    server.stop()
    prefs = server.prefs_on_disk()
    prefs.update({"timer_send": True, "timer_text": "", "timer_hex": False})
    server.prefs_file.write_text(json.dumps(prefs), encoding="utf-8")
    server.start()

    prefs = get_json("/api/prefs")
    assert prefs["timerOn"] is False
    assert prefs["timerText"] == ""
    tx_before = get_json("/api/status")["txBytes"]
    time.sleep(0.2)
    assert get_json("/api/status")["txBytes"] == tx_before


def test_timer_invalid_enabled_payload_has_no_partial_update(server):
    assert http(
        "POST",
        "/api/timer",
        {"on": False, "intervalSec": 2, "text": "seed", "hex": False},
    )[0] == 200
    before = get_json("/api/prefs")

    status, _ = http(
        "POST",
        "/api/timer",
        {"on": True, "intervalSec": 0.01, "text": ""},
    )
    assert status == 400
    assert get_json("/api/prefs") == before

    status, _ = http(
        "POST",
        "/api/timer",
        {"on": True, "intervalSec": 0.01, "text": "abc", "hex": True},
    )
    assert status == 400
    assert get_json("/api/prefs") == before


def test_hex_rx_counts_raw_bytes_not_display_characters(server):
    assert http("POST", "/api/settings", {"hexRx": True})[0] == 200
    time.sleep(0.5)  # 等待切换前已入队的文本块排空
    assert http("POST", "/api/clear")[0] == 200
    time.sleep(0.2)

    rx_before = get_json("/api/status")["rxBytes"]
    export_before = http("GET", "/api/export")[1]
    time.sleep(0.6)
    rx_after = get_json("/api/status")["rxBytes"]
    export_after = http("GET", "/api/export")[1]

    token = re.compile(r"(?<![0-9A-Fa-f])[0-9A-F]{2}(?![0-9A-Fa-f])")
    raw_before = len(token.findall(export_before))
    raw_after = len(token.findall(export_after))
    displayed_raw_delta = raw_after - raw_before
    assert displayed_raw_delta > 0
    assert rx_after - rx_before == displayed_raw_delta


def test_keep_awake_setting_round_trips_across_restart(server):
    assert get_json("/api/prefs")["keepAwake"] is False
    assert http("POST", "/api/settings", {"keepAwake": True})[0] == 200
    assert get_json("/api/prefs")["keepAwake"] is True
    assert poll(lambda: server.prefs_on_disk().get("keep_awake"), True, 3.0)
    server.restart()
    assert get_json("/api/prefs")["keepAwake"] is True
