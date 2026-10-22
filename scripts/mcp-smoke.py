#!/usr/bin/env python3
"""Acceptance checks for the diagnostic socket and the MCP server.

Two phases, both against the real binaries rather than a mocked protocol:

* phase 3 starts a headless simulator panel with `--allow-sim-control`, fakes
  the HMI socket with the few methods `linc-mcp` needs, and drives the real
  `linc-mcp` over stdio: press a button, patch the panel, inject and clear
  faults, compare the views, read events.
* phase 4 starts the standalone simulator (panel + HMI) under Xvfb and checks
  the deep-debug surface: screen model, motion history, NML/HAL/IO, crash
  records, a redacted bundle and a screenshot.

Run from anywhere:

    python3 scripts/mcp-smoke.py            # both phases, release binaries
    python3 scripts/mcp-smoke.py --debug    # use target/debug
    python3 scripts/mcp-smoke.py --phase 3
    python3 scripts/mcp-smoke.py --keep     # keep the temporary directory
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TIMEOUT = 30.0


# --------------------------------------------------------------------------
# helpers


def wait_for(path: Path, timeout: float = TIMEOUT) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if path.exists():
            return True
        time.sleep(0.05)
    return False


def start_xvfb() -> tuple[subprocess.Popen, str]:
    """Start an Xvfb on a free display and return it with its `:display`."""
    read_fd, write_fd = os.pipe()
    process = subprocess.Popen(
        ["Xvfb", "-displayfd", str(write_fd), "-screen", "0", "1280x720x24"],
        pass_fds=(write_fd,),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    os.close(write_fd)
    with os.fdopen(read_fd, "r") as reader:
        number = reader.read().strip()
    if not number:
        process.kill()
        raise RuntimeError("Xvfb did not report a display")
    return process, f":{number}"


def run(*args: str, env: dict | None = None) -> str:
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=15, env=env)
        return result.stdout.strip()
    except Exception as error:  # noqa: BLE001 - a probe must never abort the run
        return f"error: {error}"


class DiagClient:
    """One request per connection; the protocol is a single JSON line each way."""

    def __init__(self, path: Path):
        self.path = path

    def call(self, method: str, params: dict | None = None) -> dict:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(TIMEOUT)
            sock.connect(self.path)
            request = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}}
            sock.sendall((json.dumps(request) + "\n").encode())
            reply = json.loads(sock.makefile("r").readline())
        if "error" in reply:
            raise RuntimeError(f"{method}: {reply['error']}")
        return reply["result"]


class Mcp:
    """The real `linc-mcp` binary over stdio."""

    def __init__(self, binary: Path, env: dict, *args: str):
        self.process = subprocess.Popen(
            [str(binary), *args],
            cwd=ROOT,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.next_id = 1

    def call(self, name: str, arguments: dict | None = None) -> dict:
        request = {
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments or {}},
        }
        self.next_id += 1
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError(f"{name}: linc-mcp closed stdout: {self.process.stderr.read()[-2000:]}")
        result = json.loads(line)["result"]
        if result.get("isError"):
            raise RuntimeError(f"{name}: {result['content'][0]['text']}")
        return result

    def data(self, name: str, arguments: dict | None = None) -> dict:
        result = self.call(name, arguments)
        return json.loads(result["content"][-1]["text"])["data"]

    def close(self) -> None:
        if self.process.stdin:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()

    def stderr(self) -> str:
        try:
            return self.process.stderr.read()[-3000:]
        except Exception:  # noqa: BLE001
            return ""


class FakeHmi:
    """The HMI socket with just enough methods for `linc-mcp` to attach."""

    def __init__(self, path: Path):
        self.path = path
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(str(path))
        self.server.listen(4)
        self.server.settimeout(0.2)
        self.stop = False
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _panel(self) -> dict:
        # Deliberately one override away from the simulator, so the compare tool
        # has a real difference to report.
        return {
            "connected": True,
            "communication_error": False,
            "mode": {"Valid": "Auto"},
            "feed_override": 0.75,
            "rapid_override": 1.0,
            "spindle_override": 1.0,
            "program_protect": False,
            "program_protect_known": True,
            "estop": False,
            "power_on": None,
            "current_tool": 12,
            "next_tool": 0,
            "buttons": 0,
            "sseg_tool": 12,
            "sseg_next_tool": 0,
            "sseg_status": 54,
        }

    def _data(self) -> dict:
        return {
            "machine": {
                "panel": self._panel(),
                "machine": {"estop": False, "on": True, "fault": False, "homing": {}},
                "task": {
                    "power": "On", "mode": "Auto", "interp": "Idle",
                    "paused": False, "running": False, "motion_active": False,
                },
                "program": {
                    "path": "/tmp/smoke.ngc", "current_line": 4, "motion_line": 0,
                    "progress": 0.0, "dirty": False, "running": False, "paused": False,
                },
                "alarms": [],
            },
            "ui": {"page": "MACHINE"},
            "trail_included": False,
        }

    def _envelope(self, data: dict) -> dict:
        return {
            "schema_version": 1,
            "source": "linc",
            "process": {"pid": os.getpid(), "role": "hmi", "build": {"version": "smoke"}},
            "sequence": 1,
            "captured_at_ms": int(time.time() * 1000),
            "age_ms": 0,
            "stale": False,
            "dropped": {},
            "data": data,
        }

    def _serve(self) -> None:
        while not self.stop:
            try:
                conn, _ = self.server.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            with conn:
                reader = conn.makefile("r")
                for line in reader:
                    try:
                        request = json.loads(line)
                    except json.JSONDecodeError:
                        break
                    reply = {"jsonrpc": "2.0", "id": request.get("id"), "result": self._envelope(self._data())}
                    conn.sendall((json.dumps(reply) + "\n").encode())

    def close(self) -> None:
        self.stop = True
        try:
            self.server.close()
        except OSError:
            pass


class Sandbox:
    """A temporary diag/IPC/state directory for one phase."""

    def __init__(self, keep: bool):
        self.dir = Path(tempfile.mkdtemp(prefix="linc-mcp-smoke-"))
        self.keep = keep
        (self.dir / "diag").mkdir()
        (self.dir / "ipc").mkdir()
        (self.dir / "state").mkdir()

    @property
    def diag(self) -> Path:
        return self.dir / "diag"

    def env(self, **extra: str) -> dict:
        env = dict(
            os.environ,
            LINC_DIAG_DIR=str(self.diag),
            LINC_SIM_IPC=str(self.dir / "ipc"),
            XDG_STATE_HOME=str(self.dir / "state"),
            RUST_LOG=extra.pop("RUST_LOG", "warn"),
            **extra,
        )
        return env

    def cleanup(self) -> None:
        if not self.keep:
            import shutil

            shutil.rmtree(self.dir, ignore_errors=True)


def check(checks: list[tuple[str, bool]], name: str, ok: object) -> None:
    checks.append((name, bool(ok)))


def report(checks: list[tuple[str, bool]]) -> bool:
    for name, ok in checks:
        print(("ok   " if ok else "FAIL ") + name)
    return all(ok for _, ok in checks)


# --------------------------------------------------------------------------
# phase 3


def phase3(binaries: Path) -> bool:
    sandbox = Sandbox(keep=ARGS.keep)
    panel = mcp = hmi = None
    checks: list[tuple[str, bool]] = []
    try:
        env = sandbox.env()
        panel = subprocess.Popen(
            [
                str(binaries / "linc-panel"),
                "--config", str(ROOT / "configs/sim.toml"),
                "--sim", "--no-hal", "--no-window", "--no-ipc",
                "--allow-sim-control",
            ],
            cwd=ROOT, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT,
        )
        if not wait_for(sandbox.diag / "panel.sock"):
            print("panel socket never appeared")
            return False
        hmi = FakeHmi(sandbox.diag / "hmi.sock")
        mcp = Mcp(
            binaries / "linc-mcp", env,
            "--socket", str(sandbox.diag / "hmi.sock"),
            "--panel-socket", str(sandbox.diag / "panel.sock"),
            "--allow-sim-control", "--sim",
        )

        state = mcp.data("linc_sim_get_state")["state"]
        check(checks, "sim state reads", state["estop"] is True)

        state = mcp.data("linc_sim_press_button", {"button": "cycle_start", "duration_ms": 400})["state"]
        time.sleep(0.15)
        check(checks, "press reaches the state", "cycle_start" in state["pressed"])

        mcp.call("linc_sim_set_panel", {"estop": False, "power": True, "mode": "JOG"})
        time.sleep(0.1)
        panel_state = mcp.data("linc_get_panel_state")["snapshot"]
        check(
            checks,
            "patch reaches the published snapshot",
            panel_state["estop"] is False
            and panel_state["mode"]["Valid"] == "Jog"
            and panel_state["power_on"] is True,
        )

        mcp.call("linc_sim_inject_fault", {"kind": "disconnect"})
        time.sleep(0.1)
        offline = mcp.data("linc_test_wait_for", {
            "path": "panel.connected", "op": "eq", "value": False, "timeout_ms": 2000,
        })
        check(checks, "disconnect fault goes offline", offline["matched"] is True)

        mcp.call("linc_sim_clear_faults")
        time.sleep(0.1)
        back = mcp.data("linc_test_wait_for", {
            "path": "panel.connected", "op": "eq", "value": True, "timeout_ms": 2000,
        })
        check(checks, "clearing faults reconnects", back["matched"] is True)

        mcp.call("linc_sim_inject_fault", {"kind": "stale"})
        time.sleep(2.2)
        stale = mcp.call("linc_get_panel_state")
        check(checks, "stale fault ages the envelope",
              json.loads(stale["content"][0]["text"])["stale"] is True)
        mcp.call("linc_sim_clear_faults")

        compared = mcp.data("linc_compare_panel_views")
        fields = [entry["field"] for entry in compared["differences"]]
        check(checks, "compare finds the HMI's stale override", "feed_override" in fields)

        events = mcp.data("linc_get_panel_events", {"limit": 200})["events"]
        kinds = {event["kind"] for event in events}
        check(checks, "panel events recorded transitions",
              {"estop", "mode", "connected"} <= kinds)

        frames = mcp.data("linc_get_panel_frames", {"limit": 10})["frames"]
        check(checks, "the simulator records no fabricated frames", frames == [])

        mcp.close()
        ok = report(checks)
        if not ok:
            print("linc-mcp stderr:", mcp.stderr() if mcp else "")
        return ok
    finally:
        if mcp:
            mcp.close()
        if hmi:
            hmi.close()
        if panel:
            panel.terminate()
            try:
                panel.wait(timeout=5)
            except subprocess.TimeoutExpired:
                panel.kill()
        sandbox.cleanup()


# --------------------------------------------------------------------------
# phase 4


def phase4(binaries: Path) -> bool:
    sandbox = Sandbox(keep=ARGS.keep)
    xvfb = panel = hmi = mcp = None
    checks: list[tuple[str, bool]] = []
    try:
        xvfb, display = start_xvfb()
        env = sandbox.env(DISPLAY=display)
        # Without this the window goes to the operator's Wayland session instead
        # of Xvfb, and the size under test is whatever the compositor chose.
        env.pop("WAYLAND_DISPLAY", None)

        panel = subprocess.Popen(
            [
                str(binaries / "linc-panel"),
                "--config", str(ROOT / "configs/sim.toml"),
                "--sim", "--no-hal", "--no-window", "--no-ipc",
            ],
            cwd=ROOT, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT,
        )
        if not wait_for(sandbox.diag / "panel.sock"):
            print("panel socket never appeared")
            return False
        hmi = subprocess.Popen(
            [
                str(binaries / "linc"),
                "--config", str(ROOT / "configs/sim.toml"), "--sim", "--no-hal",
            ],
            cwd=ROOT, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT,
        )
        if not wait_for(sandbox.diag / "hmi.sock"):
            print("HMI socket never appeared")
            return False
        time.sleep(2.5)
        mcp = Mcp(
            binaries / "linc-mcp", env,
            "--socket", str(sandbox.diag / "hmi.sock"),
            "--panel-socket", str(sandbox.diag / "panel.sock"),
        )

        screen = mcp.data("linc_get_screen_model")
        regions = {region["name"]: region for region in screen["regions"]}
        viewport = screen["viewport"]
        fits = all(
            region["x"] >= -1
            and region["y"] >= -1
            and region["x"] + region["width"] <= viewport["width"] + 2
            and region["y"] + region["height"] <= viewport["height"] + 2
            for region in screen["regions"]
        )
        check(checks, "screen model names the shell regions",
              {"top-bar", "nav", "content", "bottom-bar"} <= set(regions))
        check(checks, "screen model reports the window and its regions fit",
              viewport["width"] > 0 and viewport["height"] > 0
              and viewport["pixels_per_point"] > 0 and fits)

        motion = mcp.data("linc_get_motion_history", {"max_samples": 50})
        check(checks, "motion history sampled the session",
              motion["sample_interval_ms"] == 100 and len(motion["samples"]) > 0)
        if motion["samples"]:
            latest = motion["samples"][-1]
            check(checks, "a motion sample carries the axes and the program line",
                  len(latest["positions"]) >= 3 and "axis" in latest["positions"][0]
                  and "program_line" in latest)

        check(checks, "metrics count the motion ring", mcp.data("linc_get_metrics")["motion"] > 0)
        check(checks, "NML says the simulator has no bridge",
              mcp.data("linc_get_nml")["available"] is False)

        hal = mcp.data("linc_get_hal")
        check(checks, "HAL reports its own outputs and no pin introspection",
              hal["available"] is True and "hmi_alive" in hal["outputs"]
              and hal["real_hal"]["pins"] is False)
        check(checks, "HAL pins are refused, not fabricated",
              mcp.data("linc_get_hal_pins")["available"] is False)

        io = mcp.data("linc_get_io")
        check(checks, "I/O state reads", io["coolant_flood"] is False and isinstance(io["limits"], list))
        check(checks, "crash records read",
              isinstance(mcp.data("linc_get_crash_records")["records"], list))

        bundle = mcp.data("linc_export_bundle")
        bundle_path = Path(bundle["path"])
        wanted = ["panel.json", "ui.json", "motion.json", "crashes.json", "snapshot.json"]
        check(checks, "bundle carries the deep-debug files",
              bundle["files"] == 13 and all((bundle_path / name).exists() for name in wanted))
        home = os.environ.get("HOME", "/nonexistent")
        leaked = [
            name for name in (entry.name for entry in bundle_path.iterdir())
            if home in (bundle_path / name).read_text(errors="ignore")
        ]
        check(checks, "bundle paths are redacted", leaked == [])

        shot = mcp.call("linc_screenshot")
        image = next((part for part in shot["content"] if part["type"] == "image"), None)
        meta = json.loads(shot["content"][-1]["text"])["data"]
        check(checks, "screenshot returns a PNG",
              image is not None and image["mimeType"] == "image/png"
              and len(image["data"]) > 1000)
        check(checks, "screenshot reports its size", meta["width"] > 0 and meta["height"] > 0)

        mcp.close()
        ok = report(checks)
        if not ok:
            print("linc-mcp stderr:", mcp.stderr() if mcp else "")
        return ok
    finally:
        if mcp:
            mcp.close()
        for process in (hmi, panel, xvfb):
            if process is not None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
        sandbox.cleanup()


# --------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--phase", choices=["3", "4", "all"], default="all")
    parser.add_argument("--debug", action="store_true", help="use target/debug binaries")
    parser.add_argument("--keep", action="store_true", help="keep the temporary directory")
    global ARGS
    ARGS = parser.parse_args()

    profile = "debug" if ARGS.debug else "release"
    binaries = ROOT / "target" / profile
    missing = [name for name in ("linc", "linc-panel", "linc-mcp")
               if not (binaries / name).exists()]
    if missing:
        print(f"missing {', '.join(missing)} in {binaries}; run: cargo build --release")
        return 2
    if ARGS.phase == "4" and not any(Path(p).exists() for p in ("/usr/bin/Xvfb", "/bin/Xvfb")):
        print("Xvfb is required for phase 4")
        return 2

    ok = True
    if ARGS.phase in ("3", "all"):
        print("== phase 3: panel diagnostics and simulator control")
        ok &= phase3(binaries)
    if ARGS.phase in ("4", "all"):
        print("== phase 4: deep debug and export")
        ok &= phase4(binaries)
    if ok:
        print("mcp smoke: ok")
        return 0
    print("mcp smoke: FAILED")
    return 1


if __name__ == "__main__":
    sys.exit(main())
