"""Updater HTTP integration tests using only an isolated local Release fixture."""

import hashlib
import io
import json
import socket
import threading
import time
import urllib.error
import urllib.request
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from qa_server import QAServer, get_json, http, poll


class ReleaseFixture:
    """Minimal GitHub Releases/API and asset server; no external network access."""

    def __init__(self):
        self.version = "9.0.0"
        self.asset_name = f"mini-rtt-viewer-{self.version}-windows-x86_64.zip"
        archive_buffer = io.BytesIO()
        with zipfile.ZipFile(archive_buffer, "w", zipfile.ZIP_DEFLATED) as archive:
            archive.writestr(
                f"mini-rtt-viewer-{self.version}-windows-x86_64/mini-rtt-viewer.exe",
                b"new-binary",
            )
        self.archive = archive_buffer.getvalue()
        digest = hashlib.sha256(self.archive).hexdigest()
        self.sums = f"{digest}  {self.asset_name}\n".encode()
        self.release_list_requests = 0
        self.archive_requests = 0
        self.download_started = threading.Event()
        self.download_gate = threading.Event()
        self.download_gate.set()
        self.pause_downloads = False

        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                path = self.path.split("?", 1)[0]
                if path == "/repos/MisakaMikoto128/mini-rtt-viewer/releases":
                    fixture.release_list_requests += 1
                    self.send_json([fixture.release_json()])
                elif path == (
                    "/repos/MisakaMikoto128/mini-rtt-viewer/releases/tags/"
                    f"v{fixture.version}"
                ):
                    self.send_json(fixture.release_json())
                elif path == f"/assets/{fixture.version}/release.zip":
                    fixture.archive_requests += 1
                    fixture.download_started.set()
                    if fixture.pause_downloads and not fixture.download_gate.wait(5):
                        self.send_bytes(500, b"download gate timeout", "text/plain")
                        return
                    fixture.pause_downloads = False
                    self.send_bytes(200, fixture.archive, "application/zip")
                elif path == f"/assets/{fixture.version}/SHA256SUMS":
                    self.send_bytes(200, fixture.sums, "text/plain")
                else:
                    self.send_bytes(404, b"not found", "text/plain")

            def send_json(self, value):
                self.send_bytes(
                    200, json.dumps(value).encode(), "application/json"
                )

            def send_bytes(self, status, body, content_type):
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, _format, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"

    def release_json(self):
        archive_url = f"{self.base_url}/assets/{self.version}/release.zip"
        sums_url = f"{self.base_url}/assets/{self.version}/SHA256SUMS"
        return {
            "id": 1,
            "tag_name": f"v{self.version}",
            "created_at": "2026-10-30T00:00:00Z",
            "published_at": "2026-10-30T00:00:00Z",
            "name": self.version,
            "body": "local fixture",
            "html_url": f"{self.base_url}/releases/v{self.version}",
            "draft": False,
            "prerelease": False,
            "assets": [
                {
                    "id": 1,
                    "name": self.asset_name,
                    "url": archive_url,
                    "browser_download_url": archive_url,
                    "content_type": "application/zip",
                    "size": len(self.archive),
                    "digest": None,
                },
                {
                    "id": 2,
                    "name": "SHA256SUMS",
                    "url": sums_url,
                    "browser_download_url": sums_url,
                    "content_type": "text/plain",
                    "size": len(self.sums),
                    "digest": None,
                },
            ],
        }

    def pause_next_download(self):
        self.pause_downloads = True
        self.download_started.clear()
        self.download_gate.clear()

    def release_download(self):
        self.download_gate.set()

    def close(self):
        self.download_gate.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


@pytest.fixture
def release_fixture():
    fixture = ReleaseFixture()
    try:
        yield fixture
    finally:
        fixture.close()


def make_qa(tmp_path, fixture, automatic=False):
    install_path = tmp_path / "install" / "old.exe"
    install_path.parent.mkdir()
    install_path.write_bytes(b"old-binary")
    server = QAServer(
        tmp_path,
        extra_env={
            "RTT_QA_UPDATE_API_BASE": fixture.base_url,
            "RTT_QA_UPDATE_INSTALL_PATH": str(install_path),
        },
    )
    server.prefs_file.write_text(
        json.dumps(
            {
                "chip_name": "STM32F103C8",
                "update_last_check_unix": 0 if automatic else int(time.time()),
            }
        ),
        encoding="utf-8",
    )
    return server, install_path


@pytest.fixture
def qa(tmp_path, release_fixture):
    server, install_path = make_qa(tmp_path, release_fixture)
    try:
        server.start()
        yield server, install_path, release_fixture
    finally:
        server.stop()


def disconnect(server):
    assert http("POST", "/api/disconnect")[0] == 200
    assert poll(lambda: get_json("/api/status")["connected"], False, 3.0)


def check_available():
    status, text = http("POST", "/api/update/check")
    assert status == 200, text
    checked = json.loads(text)
    assert checked["phase"] == "available"
    return checked


def test_update_check_does_not_install_and_status_uses_server_truth(qa):
    _server, install_path, fixture = qa
    status = get_json("/api/update/status")
    assert status["currentVersion"] == "0.5.2"
    assert status["phase"] == "idle"

    checked = check_available()
    assert checked["latestVersion"] == "9.0.0"
    assert checked["phase"] != "readyToRestart"
    assert fixture.archive_requests == 0
    assert install_path.read_bytes() == b"old-binary"


def test_connected_install_returns_409_without_side_effects(qa):
    _server, install_path, fixture = qa
    assert poll(lambda: get_json("/api/status")["connected"], True, 3.0)
    before = get_json("/api/update/status")
    assert http("POST", "/api/update/install")[0] == 409
    assert get_json("/api/update/status") == before
    assert fixture.release_list_requests == 0
    assert install_path.read_bytes() == b"old-binary"


def non_loopback_ipv4():
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        probe.connect(("8.8.8.8", 80))
        address = probe.getsockname()[0]
    finally:
        probe.close()
    if address.startswith("127."):
        pytest.skip("a non-loopback local IPv4 address is required")
    return address


def remote_http(base, path):
    request = urllib.request.Request(f"{base}{path}", data=b"", method="POST")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(request, timeout=5) as response:
            return response.status
    except urllib.error.HTTPError as error:
        return error.code


def test_non_loopback_install_and_restart_are_forbidden(tmp_path, release_fixture):
    server, _install_path = make_qa(tmp_path, release_fixture)
    try:
        server.start(listen="0.0.0.0")
        remote_base = f"http://{non_loopback_ipv4()}:18099"
        assert remote_http(remote_base, "/api/update/install") == 403
        assert remote_http(remote_base, "/api/update/restart") == 403
        assert release_fixture.release_list_requests == 0
    finally:
        server.stop()


def test_concurrent_and_repeated_install_return_409(qa):
    server, _install_path, fixture = qa
    disconnect(server)
    check_available()
    fixture.pause_next_download()
    first_result = []

    def first_install():
        first_result.append(http("POST", "/api/update/install"))

    first = threading.Thread(target=first_install)
    first.start()
    assert fixture.download_started.wait(3)
    assert http("POST", "/api/update/install")[0] == 409
    fixture.release_download()
    first.join(timeout=10)
    assert not first.is_alive()
    assert first_result[0][0] == 200, first_result
    assert get_json("/api/update/status")["phase"] == "readyToRestart"

    archive_requests = fixture.archive_requests
    assert http("POST", "/api/update/install")[0] == 409
    assert fixture.archive_requests == archive_requests


def test_restart_without_completed_install_returns_409(qa):
    assert get_json("/api/update/status")["phase"] == "idle"
    assert http("POST", "/api/update/restart")[0] == 409


def test_headless_install_succeeds_but_never_requests_restart(qa):
    server, install_path, _fixture = qa
    disconnect(server)
    check_available()
    status, text = http("POST", "/api/update/install")
    assert status == 200, text
    assert json.loads(text)["phase"] == "readyToRestart"
    assert install_path.read_bytes() == b"new-binary"

    assert http("POST", "/api/update/restart")[0] == 409
    assert get_json("/api/update/status")["phase"] == "readyToRestart"
    assert get_json("/api/status")["uptimeSec"] >= 0


def test_qa_log_dir_overrides_inherited_environment(
    tmp_path, release_fixture, monkeypatch
):
    qa_dir = tmp_path / "qa"
    qa_dir.mkdir()
    inherited_dir = tmp_path / "inherited-logs"
    monkeypatch.setenv("RTT_LOG_DIR", str(inherited_dir))
    server, _install_path = make_qa(qa_dir, release_fixture)
    try:
        server.start()
        status, text = http("POST", "/api/logs/save", {})
        assert status == 200, text
        name = json.loads(text)["name"]
        assert (qa_dir / "logs" / name).is_file()
        assert not (inherited_dir / name).exists()
    finally:
        server.stop()


def test_automatic_check_happens_only_once_per_twenty_four_hours(
    tmp_path, release_fixture
):
    server, _install_path = make_qa(tmp_path, release_fixture, automatic=True)
    try:
        server.start()
        assert poll(
            lambda: get_json("/api/update/status")["phase"], "available", 5.0
        )
        assert release_fixture.release_list_requests == 1
        assert poll(
            lambda: server.prefs_on_disk().get("update_last_check_unix", 0) > 0,
            True,
            3.0,
        )

        server.restart()
        time.sleep(0.8)
        assert release_fixture.release_list_requests == 1
        assert get_json("/api/update/status")["phase"] == "idle"
    finally:
        server.stop()
