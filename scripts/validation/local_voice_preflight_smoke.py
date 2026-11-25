"""Real main HTTP preflight refusal/retry; no capture, playback or model warmup.

An owned socket in the same WSL distribution holds the candidate port. The
subsequent owned service is cancelled while still starting. Fixed model files
are hard linked by the existing lifecycle fixture builder, never copied/deleted.
"""
import argparse
import hashlib
import http.client
import json
from pathlib import Path
import queue
import socket
import subprocess
import threading
import time

from local_voice_lifecycle_smoke import STATUS_FIELDS, stage, write_json


PORT_HOLDER = r'''
import json, selectors, socket, sys, time
listener = socket.socket()
try:
    listener.bind(("127.0.0.1", 18869))
    listener.listen(4)
except OSError as error:
    print(json.dumps({"state":"bind_failed","error_type":type(error).__name__}), flush=True)
    raise SystemExit(1)
listener.setblocking(False)
selector = selectors.DefaultSelector()
selector.register(listener, selectors.EVENT_READ)
selector.register(sys.stdin, selectors.EVENT_READ)
accepted = 0
print(json.dumps({"state":"bound","port":18869}), flush=True)
deadline = time.monotonic() + 180
stopping = False
try:
    while not stopping and time.monotonic() < deadline:
        for key, _ in selector.select(.1):
            if key.fileobj is listener:
                connection, _ = listener.accept()
                accepted += 1
                connection.close()
            else:
                line = sys.stdin.readline()
                if not line or line.strip() == "stop":
                    stopping = True
finally:
    listener.close()
    selector.close()
print(json.dumps({"state":"closed","accepted_connections":accepted}), flush=True)
'''


class PortHolder:
    def __init__(self):
        self.events = queue.Queue()
        self.child = subprocess.Popen(
            ["wsl.exe", "-d", "Ubuntu", "--", "python3", "-u", "-c", PORT_HOLDER],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW)

        def read():
            for line in self.child.stdout:
                try:
                    self.events.put(json.loads(line))
                except ValueError:
                    self.events.put({"state": "invalid_holder_output"})

        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()

    def wait_bound(self):
        receipt = self.events.get(timeout=15)
        assert receipt == {"state": "bound", "port": 18869}, "owned_port_holder_could_not_bind"
        assert self.child.poll() is None, "owned_port_holder_exited"
        return receipt

    def close(self):
        if self.child.poll() is None:
            try:
                self.child.stdin.write(b"stop\n")
                self.child.stdin.flush()
            except (BrokenPipeError, OSError):
                pass
        self.child.stdin.close()
        self.child.wait(timeout=10)
        self.reader.join(timeout=2)
        receipts = []
        while not self.events.empty():
            receipts.append(self.events.get_nowait())
        final = next((value for value in receipts if value.get("state") == "closed"), None)
        assert self.child.returncode == 0 and final is not None, "owned_port_holder_cleanup_failed"
        assert final["accepted_connections"] == 0, "preflight_contacted_the_existing_listener"
        return final


def run(root, output, ui):
    fixture, staged = stage(root, output, ui, "cosy")
    report = {"test_type": "main_owned_local_voice_preflight_recovery", "success": False,
              "production_data_used": False, "production_published": False,
              "microphone_requested": False, "speaker_requested": False,
              "camera_requested": False, "ptz_requested": False,
              "model_inference_requested": False, "model_warmup_observed": False,
              "listener_namespace": "Ubuntu WSL loopback", **staged, "transitions": []}
    executable = root / "target/release/ai-tutor.exe"
    report["executable_sha256"] = hashlib.sha256(executable.read_bytes()).hexdigest()
    holder = None
    process = None
    token = None
    descriptor = fixture / "runtime/server.json"
    started = time.perf_counter()
    with socket.socket() as reserved:
        reserved.bind(("127.0.0.1", 0))
        port = reserved.getsockname()[1]

    def api(method, path, body=None, timeout=5):
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
        try:
            connection.request(method, path, json.dumps(body) if body is not None else None,
                               {"Content-Type": "application/json", "Cookie": "ai_tutor_local=" + token})
            response = connection.getresponse()
            value = json.loads(response.read())
            assert response.status == 200, f"{path}: HTTP {response.status}"
            return value
        finally:
            connection.close()

    def state():
        value = api("GET", "/api/state")
        assert value["camera"]["state"] == "closed" and value["ptz"]["mode"] == "off"
        assert value["audio"]["recording"] is False
        local = value["local_speech"]
        assert not local.get("active") and value["audio"].get("tts_configured_engine") is None, "unexpected_client_installation"
        if local.get("warmup_completed") or local.get("ready") or local.get("phase") in ("checking", "ready_cold", "warming", "active"):
            report["model_warmup_observed"] = True
            raise AssertionError("unexpected_ready_or_warmup_before_startup_cancellation")
        snapshot = {key: local[key] for key in STATUS_FIELDS if key in local}
        snapshot["observed_ms"] = round((time.perf_counter() - started) * 1000, 3)
        previous = report["transitions"][-1] if report["transitions"] else {}
        fields = ("phase", "instance_id", "busy")
        if tuple(snapshot.get(key) for key in fields) != tuple(previous.get(key) for key in fields):
            report["transitions"].append(snapshot)
            write_json(output / "progress.json", {"transitions": report["transitions"]})
        return local

    try:
        holder = PortHolder()
        report["listener_bound"] = holder.wait_bound()
        process = subprocess.Popen([str(executable), "--root", str(fixture), "--port", str(port), "--no-browser"],
                                   creationflags=subprocess.CREATE_NO_WINDOW,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + 30
        while not descriptor.exists() and process.poll() is None and time.monotonic() < deadline:
            time.sleep(.05)
        info = json.loads(descriptor.read_text(encoding="utf-8"))
        assert info["pid"] == process.pid and info["port"] == port
        token = info["token"]
        while True:
            try:
                local = state()
            except (ConnectionRefusedError, ConnectionResetError):
                assert process.poll() is None and time.monotonic() < deadline, "main_http_startup_failed"
                time.sleep(.05)
                continue
            if local["phase"] == "degraded" and not local["busy"]:
                break
            assert local["phase"] not in ("cleanup_failed", "configuration_error", "stopped"), "unexpected_preflight_terminal"
            assert time.monotonic() < deadline, "preflight_failure_not_reported"
            time.sleep(.05)
        assert holder.child.poll() is None, "preflight_stopped_the_foreign_listener"
        stop = local["ownership_stop"]
        assert local["linux_cleanup_confirmed"] is True and local["retry_available"] is True
        assert stop["linux_cleanup_confirmed"] is True and stop["normal_owner_stop"] is False
        assert stop["receipt"]["state"] == "failed" and stop["receipt"]["process_started"] is False
        assert stop["receipt"]["linux_cleanup_confirmed"] is True
        report["preflight_receipt"] = {key: local[key] for key in STATUS_FIELDS if key in local}
        report["preflight_ms"] = round((time.perf_counter() - started) * 1000, 3)
        report["listener_untouched_until_explicit_release"] = True
        report["listener_release"] = holder.close()
        holder = None

        retry = api("POST", "/api/audio/local-speech/retry", {})
        assert retry["queued"] is True
        retry_started = time.perf_counter()
        deadline = time.monotonic() + 10
        while True:
            local = state()
            if local["phase"] == "starting":
                break
            assert time.monotonic() < deadline and local["phase"] not in ("degraded", "cleanup_failed"), "retry_did_not_start"
            time.sleep(.05)
        report["retry_instance_is_new"] = local.get("instance_id") != report["preflight_receipt"].get("instance_id")
        assert report["retry_instance_is_new"]
        while time.perf_counter() - retry_started < 1.0:
            local = state()
            assert local["phase"] == "starting", "retry_advanced_beyond_starting"
            time.sleep(.05)
        report["retry_cancel_phase"] = local["phase"]
        stop_started = time.perf_counter()
        stopped = api("POST", "/api/audio/local-speech/stop", {}, timeout=40)
        report["retry_stop_ms"] = round((time.perf_counter() - stop_started) * 1000, 3)
        report["retry_stop_receipt"] = {key: stopped[key] for key in STATUS_FIELDS if key in stopped}
        assert stopped["phase"] == "stopped" and stopped["active"] is False
        assert stopped["linux_cleanup_confirmed"] is True and stopped["client_deactivation_confirmed"] is True
        assert stopped["ownership_stop"]["normal_owner_stop"] is True
        assert not stopped["ready"] and not stopped["warmup_completed"] and not stopped["busy"]
        state()
        assert not (fixture / "runtime/tts-service.json").exists()
        report["checks_passed"] = True
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        if holder is not None:
            try:
                report["listener_release"] = holder.close()
            except Exception as error:
                report["listener_cleanup_error"] = type(error).__name__
                if holder.child.poll() is None:
                    holder.child.terminate()
                    holder.child.wait(timeout=5)
                    report["forced_owned_listener_bridge_stop"] = True
        if process is not None:
            if token and process.poll() is None:
                try:
                    api("POST", "/api/shutdown", {}, timeout=10)
                except (OSError, AssertionError, http.client.HTTPException):
                    report["shutdown_request_failed"] = True
            try:
                process.wait(timeout=35)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=5)
                report["forced_owned_main_stop"] = True
                report["linux_cleanup_after_forced_main_stop"] = "unconfirmed"
            report["main_exit_code"] = process.returncode
            report["server_descriptor_removed_normally"] = not descriptor.exists()
            if descriptor.exists() and token:
                stale = json.loads(descriptor.read_text(encoding="utf-8"))
                if stale.get("pid") == process.pid and stale.get("token") == token:
                    descriptor.unlink()
                    report["stale_owned_credential_removed_by_probe"] = True
        policy = fixture / "runtime/local-speech-policy.json"
        if policy.exists() and json.loads(policy.read_text(encoding="utf-8")) == staged["policy"]:
            policy.unlink()
            report["fixture_policy_removed"] = True
        report["success"] = bool(report.get("checks_passed") and report.get("main_exit_code") == 0
                                 and report.get("server_descriptor_removed_normally")
                                 and not report.get("forced_owned_main_stop")
                                 and not report.get("listener_cleanup_error"))
        report["elapsed_ms"] = round((time.perf_counter() - started) * 1000, 3)
        write_json(output / "report.json", report)
        print(json.dumps({"success": report["success"], "report": str(output / "report.json"),
                          "error": report.get("error"), "main_exit_code": report.get("main_exit_code")}))
    return report["success"]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real-owned-preflight", action="store_true", required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--ui", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    raise SystemExit(0 if run(args.root.resolve(), args.output.resolve(), args.ui.resolve()) else 1)
