"""跨 MCU 型号解析的 HTTP 黑盒回归。"""

import pytest

from qa_server import QAServer, get_json, http, poll


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("chip-resolution-qa"))
    srv.start()
    yield srv
    srv.stop()


def disconnect():
    http("POST", "/api/disconnect")
    assert poll(lambda: get_json("/api/status")["connected"], False, 3.0)


def test_ambiguous_partial_chip_is_not_silently_replaced(server):
    disconnect()
    code, text = http("POST", "/api/connect", {"chip": "STM32F"})
    assert code == 400, text
    assert "多个型号" in text
    assert get_json("/api/status")["connected"] is False


def test_unique_non_f103_partial_chip_uses_full_device_name(server):
    disconnect()
    code, text = http("POST", "/api/connect", {"chip": "STM32G474VE"})
    assert code == 200, text
    assert get_json("/api/prefs")["chip"] == "STM32G474VET6"
