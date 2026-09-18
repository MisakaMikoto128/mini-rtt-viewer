"""QA 验收公共件:被测服务生命周期管理。

纪律(docs/team/charter.md):黑盒验收,预期只来自验收矩阵/spec 条目;
一律临时 RTT_PREFS_FILE(连 APPDATA 一起指向临时目录),不碰真实 %APPDATA%;
固定 --port 18099;RTT_WEB_NO_BROWSER=1 禁止拉起浏览器。
"""

import json
import os
import socket
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXE = ROOT / "target" / "release" / "mini-rtt-viewer.exe"
PORT = 18099  # QA 纪律:固定 18099
BASE = f"http://127.0.0.1:{PORT}"
# 主题插件目录 = exe 旁 themes/(验收矩阵 1 的落点)
THEMES_DIR = EXE.parent / "themes"


class QAServer:
    """可重启的服务实例:同一 RTT_PREFS_FILE 跨重启,供持久化断言。"""

    def __init__(self, tmpdir: Path):
        self.tmpdir = tmpdir
        self.prefs_file = tmpdir / "prefs.json"
        self.proc: subprocess.Popen | None = None

    def start(self) -> None:
        # 前置:端口必须空闲,否则会连到僵尸进程,污染黑盒断言
        s = socket.socket()
        s.settimeout(0.5)
        try:
            s.connect(("127.0.0.1", PORT))
            s.close()
            raise RuntimeError(
                f"端口 {PORT} 已被占用(疑似残留 mini-rtt-viewer 进程),拒绝开测"
            )
        except OSError:
            pass
        finally:
            s.close()
        env = {
            **os.environ,
            "APPDATA": str(self.tmpdir),  # 隔离:真实 %APPDATA% 不碰
            "RTT_PREFS_FILE": str(self.prefs_file),
            "RTT_WEB_NO_BROWSER": "1",
        }
        self.proc = subprocess.Popen(
            [str(EXE), "--demo-log", "--no-window", "--port", str(PORT)],
            cwd=str(ROOT), env=env,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        deadline = time.time() + 20
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError("mini-rtt-viewer 提前退出(端口被占或启动失败)")
            try:
                urllib.request.urlopen(f"{BASE}/api/status", timeout=1)
                return
            except Exception:
                time.sleep(0.3)
        self.stop()
        raise RuntimeError("mini-rtt-viewer 未在 20s 内就绪")

    def stop(self) -> None:
        if self.proc is not None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
            self.proc = None
        # 等端口释放,避免下一个用例起服失败
        deadline = time.time() + 10
        while time.time() < deadline:
            s = socket.socket()
            s.settimeout(0.5)
            try:
                s.connect(("127.0.0.1", PORT))
                s.close()
                time.sleep(0.2)
            except OSError:
                s.close()
                return

    def restart(self) -> None:
        self.stop()
        self.start()

    def prefs_on_disk(self) -> dict:
        """直接读 RTT_PREFS_FILE 磁盘文件(落盘验收用)。"""
        return json.loads(self.prefs_file.read_text(encoding="utf-8"))


def http(method: str, path: str, body: dict | None = None) -> tuple[int, str]:
    """返回 (status, body_text)。4xx/5xx 不抛异常(QA 要看状态码)。"""
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        f"{BASE}{path}", data=data, method=method,
        headers={"Content-Type": "application/json"} if data else {},
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")


def get_json(path: str):
    status, text = http("GET", path)
    assert status == 200, f"GET {path} -> {status}"
    return json.loads(text)


def poll(fn, expect, timeout_s: float, tick: float = 0.2) -> bool:
    """轮询 fn() 直到 fn() == expect 或超时。"""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            if fn() == expect:
                return True
        except Exception:
            pass
        time.sleep(tick)
    return False
