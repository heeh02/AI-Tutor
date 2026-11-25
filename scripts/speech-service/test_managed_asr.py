"""Owned ASR lifecycle checks with a fake engine; never import sherpa/NumPy."""
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from uuid import uuid4

from managed_asr import MAX_LINE, ParentControl, run

TOKEN = "synthetic-owner-token-000000000000"


class Source:
    def __init__(self):
        self.lines = queue.Queue()

    def readline(self, _limit):
        return self.lines.get(timeout=5)

    def send(self, value):
        self.lines.put(json.dumps(value) + "\n")


class Events:
    def __init__(self):
        self.lines, self.all = queue.Queue(), []

    def write(self, line):
        self.all.append(line)
        self.lines.put(json.loads(line))

    def flush(self):
        pass

    def next(self, state):
        item = self.lines.get(timeout=3)
        assert item["state"] == state, item
        return item


class FakeEngine:
    provider = "synthetic in-process ASR"

    def __init__(self, warm_gate=None):
        self.warm_gate = warm_gate

    def warm(self):
        if self.warm_gate is not None:
            assert self.warm_gate.wait(3)

    def create(self):
        return []

    def chunk(self, stream, pcm):
        stream.append(pcm)
        return "synthetic"

    def finish(self, stream):
        return "synthetic final"


class ManagedTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.instance = uuid4().hex
        self.source, self.events = Source(), Events()
        self.loaded, self.exit_code = [], []
        self.thread = None

    def start(self, factory=None, timeout=1):
        def load(path):
            self.loaded.append(path)
            return factory() if factory else FakeEngine()

        self.thread = threading.Thread(target=lambda: self.exit_code.append(run(
            self.root, self.instance, self.source, self.events, load, timeout)), daemon=True)
        self.thread.start()
        self.events.next("awaiting_owner")

    def bootstrap(self, **change):
        self.source.send({"protocol_version": 1, "instance_id": self.instance,
                          "bearer_token": TOKEN, **change})

    def warm(self):
        self.bootstrap()
        endpoint = self.events.next("loading")["base_url"]
        self.events.next("warming")
        self.events.next("warm_ready")
        return endpoint

    def stop(self):
        self.source.send({"op": "stop", "instance_id": self.instance})
        result = self.events.next("stopped")
        self.thread.join(3)
        self.assertFalse(self.thread.is_alive())
        self.assertEqual(self.exit_code, [0])
        self.assertTrue(result["http_closed"])
        self.assertNotIn(TOKEN, "".join(self.events.all))

    def request(self, endpoint, path="/health", body=None, instance=None, token=TOKEN):
        headers = {"Authorization": "Bearer " + token,
                   "X-ASR-Instance": instance if instance is not None else self.instance}
        data = json.dumps(body).encode() if body is not None else None
        req = Request(endpoint + path, data, headers)
        try:
            response = urlopen(req, timeout=2)
        except HTTPError as error:
            response = error
        with response:
            return response.status, json.loads(response.read())

    def test_no_owner_handshake_never_loads_model_or_binds_http(self):
        self.start(timeout=0.05)
        failed = self.events.next("failed")
        self.thread.join(1)
        self.assertEqual(self.loaded, [])
        self.assertEqual(self.exit_code, [1])
        self.assertEqual(failed["reason"], "owner_bootstrap_timeout")
        self.assertNotIn("base_url", failed)
        self.source.lines.put("")

    def test_wrong_instance_bootstrap_is_rejected_before_model(self):
        self.start()
        self.bootstrap(instance_id=uuid4().hex)
        self.events.next("failed")
        self.thread.join(1)
        self.assertEqual(self.loaded, [])
        self.assertEqual(self.exit_code, [1])

    def test_ready_and_warm_are_distinct_and_inference_waits_for_warm(self):
        gate = threading.Event()
        self.start(lambda: FakeEngine(gate))
        self.bootstrap()
        endpoint = self.events.next("loading")["base_url"]
        warming = self.events.next("warming")
        self.assertTrue(warming["ready"])
        self.assertFalse(warming["warmup_completed"])
        code, health = self.request(endpoint)
        self.assertEqual(code, 200)
        self.assertTrue(health["ready"])
        self.assertFalse(health["warmup_completed"])
        self.assertEqual(self.request(endpoint, "/v1/asr/start", {})[0], 503)
        gate.set()
        self.events.next("warm_ready")
        self.assertTrue(self.request(endpoint)[1]["warmup_completed"])
        self.stop()

    def test_instance_and_token_are_checked_without_touching_active_decoder(self):
        self.start()
        endpoint = self.warm()
        self.assertEqual(self.request(endpoint, token="wrong-token")[0], 401)
        self.assertEqual(self.request(endpoint, instance=uuid4().hex)[0], 409)
        body = dict(protocol_version=1, session_id=str(uuid4()), recording_generation=9,
                    sample_rate=16000, channels=1, encoding="pcm_s16le",
                    language="auto", max_duration_ms=1000)
        code, started = self.request(endpoint, "/v1/asr/start", body)
        self.assertEqual(code, 200)
        self.assertEqual(started["instance_id"], self.instance)
        self.assertEqual(self.request(endpoint, "/v1/asr/start", body, instance=uuid4().hex)[0], 409)
        self.stop()

    def test_managed_entry_never_replaces_or_deletes_external_descriptor(self):
        descriptor = self.root / "runtime/speech-service.json"
        descriptor.parent.mkdir()
        previous = b'{"synthetic_external_owner":"leave untouched"}'
        descriptor.write_bytes(previous)
        self.start()
        endpoint = self.warm()
        self.assertTrue(endpoint.startswith("http://127.0.0.1:"))
        self.assertNotEqual(endpoint, "http://127.0.0.1:18867")
        self.stop()
        self.assertEqual(descriptor.read_bytes(), previous)

    def test_foreign_stop_does_not_stop_this_instance(self):
        self.start()
        endpoint = self.warm()
        self.source.send({"op": "stop", "instance_id": uuid4().hex})
        self.assertEqual(self.request(endpoint)[0], 200)
        self.stop()

    def test_cancel_during_warm_never_publishes_warm_ready(self):
        gate = threading.Event()
        controls = []
        def create_control(source, instance):
            control = ParentControl(source, instance)
            controls.append(control)
            return control
        with patch("managed_asr.ParentControl", side_effect=create_control):
            self.start(lambda: FakeEngine(gate))
            self.bootstrap()
            self.events.next("loading")
            self.events.next("warming")
            self.source.lines.put("")
            # A deterministic cancellation barrier, not a timing assumption.
            self.assertTrue(controls[0].stop.wait(1))
            gate.set()
            self.events.next("stopped")
            self.thread.join(1)
        self.assertFalse(self.thread.is_alive())
        self.assertNotIn('"warm_ready"', "".join(self.events.all))

    def test_model_load_failure_closes_only_owned_http_and_returns_failure(self):
        def fail():
            raise RuntimeError("synthetic private failure detail")
        self.start(fail)
        self.bootstrap()
        self.events.next("loading")
        failure = self.events.next("failed")
        self.thread.join(1)
        self.assertTrue(failure["http_closed"])
        self.assertEqual(self.exit_code, [1])
        self.assertNotIn("private failure", "".join(self.events.all))
        self.source.lines.put("")

    def test_eof_terminates_real_fake_child_without_loading_native_model(self):
        # The process uses the same venv interpreter as this test command; its
        # imports are stdlib plus these source files, not sherpa or NumPy.
        script = self.root / "fake_entry.py"
        script.write_text("import sys\n"
                          + "sys.path.insert(0," + repr(str(Path(__file__).resolve().parent)) + ")\n"
                          + "from managed_asr import run\nfrom test_managed_asr import FakeEngine\n"
                          + "raise SystemExit(run(sys.argv[1],sys.argv[2],engine_factory=lambda _:FakeEngine()))\n",
                          encoding="utf-8")
        child = subprocess.Popen([sys.executable, "-u", str(script), str(self.root), self.instance],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 text=True, encoding="utf-8")
        try:
            initial = json.loads(child.stdout.readline())
            self.assertEqual(initial["state"], "awaiting_owner")
            self.assertGreater(initial["interpreter_pid"], 0)
            child.stdin.write(json.dumps({"protocol_version": 1, "instance_id": self.instance,
                                          "bearer_token": TOKEN}) + "\n")
            child.stdin.flush()
            for state in ["loading", "warming", "warm_ready"]:
                self.assertEqual(json.loads(child.stdout.readline())["state"], state)
            child.stdin.close()
            self.assertEqual(json.loads(child.stdout.readline())["state"], "stopped")
            self.assertEqual(child.wait(timeout=3), 0)
            self.assertEqual(child.stderr.read(), "")
        finally:
            if child.poll() is None:
                child.kill()  # Only the test's own fake child; never a service PID.
                child.wait(timeout=3)
            child.stdout.close()
            child.stderr.close()


@unittest.skipUnless(os.name == "nt", "Windows CRT pipe regression")
class WindowsOwnerPipeTests(unittest.TestCase):
    def start_child(self):
        # This child imports only stdlib and our protocol, with no model/HTTP.
        script = r'''
import json, os, sys, threading
sys.path.insert(0, sys.argv[1])
from managed_asr import ParentControl
control = ParentControl(sys.stdin, sys.argv[2])
idle = threading.Event()
original = control.lines._available
def available():
    count = original()
    if count == 0 and control.bootstrap.is_set():
        idle.set()
    return count
control.lines._available = available
reader = threading.Thread(target=control.read, daemon=True)
reader.start()
print(json.dumps({"state":"pipe_ready", "binary_pipe":control.lines.fd is not None}), flush=True)
assert control.bootstrap.wait(3)
if not control.error:
    assert idle.wait(3)
    done = threading.Event()
    def probe():
        os.fstat(0)
        done.set()
    threading.Thread(target=probe, daemon=True).start()
    print(json.dumps({"state":"fstat", "completed":done.wait(1)}), flush=True)
assert control.stop.wait(3) or control.error
reader.join(1)
print(json.dumps({"state":"stopped", "error":control.error, "reader_exited":not reader.is_alive()}), flush=True)
'''
        instance = uuid4().hex
        child = subprocess.Popen([sys.executable, "-u", "-c", script,
                                  str(Path(__file__).resolve().parent), instance],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 creationflags=subprocess.CREATE_NO_WINDOW)
        events = queue.Queue()
        def collect():
            for line in child.stdout:
                events.put(json.loads(line))
        thread = threading.Thread(target=collect, daemon=True)
        thread.start()
        def cleanup():
            if child.poll() is None:
                child.kill()  # Exact fake Popen child only; no service lookup.
                child.wait(timeout=3)
            if not child.stdin.closed:
                child.stdin.close()
            thread.join(1)
            child.stdout.close()
            child.stderr.close()
        self.addCleanup(cleanup)
        self.assertEqual(events.get(timeout=3), {"state":"pipe_ready", "binary_pipe":True})
        return child, events, instance

    def send(self, child, data):
        child.stdin.write(data)
        child.stdin.flush()

    def test_waiting_pipe_does_not_block_crt_fstat_and_stop_or_eof_remain_live(self):
        for stop in (True, False):
            with self.subTest(stop_command=stop):
                child, events, instance = self.start_child()
                bootstrap = json.dumps({"protocol_version":1, "instance_id":instance,
                                        "bearer_token":TOKEN}).encode() + b"\r\n"
                self.send(child, bootstrap[:17])
                with self.assertRaises(queue.Empty):
                    events.get(timeout=.05)  # Partial input cannot release bootstrap.
                self.send(child, bootstrap[17:])
                self.assertEqual(events.get(timeout=3), {"state":"fstat", "completed":True})
                if stop:
                    # Multiple buffered lines must retain exact owner matching.
                    wrong = json.dumps({"op":"stop", "instance_id":uuid4().hex}).encode() + b"\n"
                    right = json.dumps({"op":"stop", "instance_id":instance}).encode() + b"\n"
                    self.send(child, wrong + right)
                else:
                    child.stdin.close()
                self.assertEqual(events.get(timeout=3),
                                 {"state":"stopped", "error":None, "reader_exited":True})
                self.assertEqual(child.wait(timeout=3), 0)
                self.assertEqual(child.stderr.read(), b"")

    def test_real_pipe_rejects_oversized_and_unterminated_owner_lines(self):
        for payload in (b"x" * (MAX_LINE + 1), b'{"protocol_version":1}'):
            with self.subTest(bytes=len(payload)):
                child, events, _ = self.start_child()
                self.send(child, payload)
                if len(payload) <= MAX_LINE:
                    child.stdin.close()
                terminal = events.get(timeout=3)
                self.assertEqual(terminal, {"state":"stopped", "error":"invalid_owner_protocol",
                                            "reader_exited":True})
                self.assertEqual(child.wait(timeout=3), 0)


if __name__ == "__main__":
    unittest.main()
