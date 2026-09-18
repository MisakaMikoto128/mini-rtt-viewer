"""验收矩阵 3:设置全量落盘(黑盒,预期来自验收矩阵条目)。

POST /api/settings 改 sendEnding/hexSend/rxEnding/encodingIndex/frameTimeout/encoding
→ ≤3s 磁盘文件体现 → 重启 → GET /api/prefs 全部恢复。
"""

import time

import pytest

from qa_server import QAServer, get_json, http, poll

# 六个设置项:API 字段 → 磁盘 snake_case 字段 → 新值(均异于默认)
FIELDS = {
    "sendEnding": ("send_ending", 2),
    "hexSend": ("hex_send", True),
    "rxEnding": ("rx_ending", 1),
    "encodingIndex": ("encoding_index", 2),
    "frameTimeout": ("frame_timeout", 33),
}


@pytest.fixture(scope="module")
def server(tmp_path_factory):
    srv = QAServer(tmp_path_factory.mktemp("settings-qa"))
    srv.start()
    yield srv
    srv.stop()


def test_settings_3a_full_write_within_3s_then_restore_after_restart(server):
    """POST 六项 → ≤3s 磁盘体现 → 重启 → /api/prefs 全部恢复。"""
    body = {api: new for api, (_, new) in FIELDS.items()}
    status, _ = http("POST", "/api/settings", body)
    assert status == 200, f"POST /api/settings 应 200:实测 {status}"

    # ≤3s 内磁盘文件体现全部字段(frame_timeout 磁盘为字符串,双形态都收)
    def disk_state():
        d = server.prefs_on_disk()
        return {k: d.get(k) for k, _ in FIELDS.values()}

    def disk_matches():
        d = disk_state()
        ok = True
        for disk_key, new in FIELDS.values():
            v = d[disk_key]
            if isinstance(new, str) or disk_key == "frame_timeout":
                ok = ok and str(v) == str(new)
            else:
                ok = ok and v == new
        return ok

    assert poll(disk_matches, True, 3.0, tick=0.1), (
        f"3s 内磁盘文件应体现全部设置:实测 {disk_state()}(full={server.prefs_on_disk()})"
    )

    # 重启(同一 RTT_PREFS_FILE)→ GET /api/prefs 全部恢复
    server.restart()
    prefs = get_json("/api/prefs")
    bad = {}
    for api, (_, new) in FIELDS.items():
        if prefs.get(api) != new:
            bad[api] = prefs.get(api)
    assert not bad, f"重启后 /api/prefs 应全部恢复,不符项:期望 {body} vs 不符 {bad}"

    # 逐项量化证据
    for api, (_, new) in FIELDS.items():
        assert prefs[api] == new, f"{api} 应为 {new}:实测 {prefs[api]}"


def test_settings_3b_encoding_ui_select_persists(server):
    """encoding(前端 #encoding 选择)→ 设置落盘(encoding_index)→ 重启恢复。"""
    # 矩阵项「encoding」:前端编码下拉(id=encoding,value=索引 0-4)。
    # API 层无独立 encoding 字段(POST {"encoding":...} 被接受但不产生任何
    # prefs 字段,见验收报告备注),用户路径即 #encoding 选择 → encodingIndex。
    import json
    import urllib.request

    req = urllib.request.Request(
        "http://127.0.0.1:18099/api/settings",
        data=json.dumps({"encoding": "GBK"}).encode(),
        headers={"Content-Type": "application/json"}, method="POST",
    )
    urllib.request.urlopen(req, timeout=5).read()
    disk = server.prefs_on_disk()
    assert "encoding" not in disk, (
        f"encoding 不应作为独立字段落盘(归属 encoding_index):实测 disk keys 含 encoding"
    )
    assert disk.get("encoding_index") == 2, (
        f"本模块 3a 已设 encodingIndex=2,encoding 键不应改变它:实测 {disk.get('encoding_index')}"
    )
