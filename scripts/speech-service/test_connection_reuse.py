"""Real loopback HTTP with a fake engine; no native models or audio devices."""
from http.client import HTTPConnection, RemoteDisconnected
from http.server import HTTPServer
import json
from pathlib import Path
import queue
import socket
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from uuid import uuid4

import managed_asr
import asr_service
from asr_service import Handler, Recognition, RecognitionServer

TOKEN = "synthetic-owner-token-000000000000"
OBSERVATIONS = {}


class Engine:
    provider = "synthetic connection reuse engine"

    def __init__(self):
        self.calls = []
        self.fail_chunk = False

    def warm(self):
        pass

    def create(self):
        self.calls.append(("start", threading.get_ident()))
        return []

    def chunk(self, stream, pcm):
        self.calls.append(("chunk", threading.get_ident()))
        if self.fail_chunk:
            raise RuntimeError("private synthetic detail")
        stream.append(pcm)
        return "短 yes" if len(stream) > 1 else ""

    def finish(self, stream):
        self.calls.append(("final", threading.get_ident()))
        return "短 yes final"


class ObservedHandler(Handler):
    def setup(self):
        super().setup()
        self.server.socket_options.append({
            "timeout": self.connection.gettimeout(),
            "tcp_nodelay": self.connection.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY),
        })

    def handle_one_request(self):
        if getattr(self, "requests_seen", 0):
            self.server.next_read.set()
        self.requests_seen = getattr(self, "requests_seen", 0) + 1
        super().handle_one_request()


class ObservedServer(RecognitionServer):
    def __init__(self, address, _handler=ObservedHandler):
        self.accepted_addresses = []
        self.handler_calls = 0
        self.socket_options = []
        self.next_read = threading.Event()
        self.closed_request = threading.Event()
        self.errors = []
        super().__init__(address, ObservedHandler)

    def get_request(self):
        connection, address = super().get_request()
        self.accepted_addresses.append(address)
        return connection, address

    def finish_request(self, request, address):
        self.handler_calls += 1
        super().finish_request(request, address)

    def close_request(self, request):
        super().close_request(request)
        self.closed_request.set()

    def handle_error(self, request, address):
        # Observe unexpected handler failures without hiding them from tests.
        import sys
        self.errors.append(type(sys.exc_info()[1]).__name__)


def start_body():
    return {"protocol_version": 1, "session_id": str(uuid4()), "recording_generation": 1,
            "sample_rate": 16000, "channels": 1, "encoding": "pcm_s16le",
            "language": "auto", "max_duration_ms": 20000}


def request(client, instance, path, body=None, headers=None):
    fields = {"Authorization": "Bearer " + TOKEN, "X-ASR-Instance": instance}
    fields.update(headers or {})
    if isinstance(body, dict):
        body = json.dumps(body).encode()
        fields["Content-Type"] = "application/json"
    began = time.perf_counter()
    client.request("POST" if body is not None else "GET", path, body, fields)
    response = client.getresponse()
    data = response.read()
    return response.status, dict(response.getheaders()), json.loads(data), (time.perf_counter() - began) * 1000


class ConnectionTests(unittest.TestCase):
    def setUp(self):
        self.engine = Engine()
        self.server = ObservedServer(("127.0.0.1", 0))
        self.server.recognition = Recognition(self.engine)
        self.server.token, self.server.instance_id = TOKEN, uuid4().hex
        self.server.ready = self.server.warmup_completed = True
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=.01), daemon=True)
        self.thread.start()
        self.client = HTTPConnection("127.0.0.1", self.server.server_port, timeout=2)
        self.body = start_body()
        self.path = "/v1/asr/" + self.body["session_id"]

    def tearDown(self):
        self.client.close()
        if self.thread.is_alive():
            self.server.shutdown()
        self.thread.join(2)
        self.server.server_close()
        self.assertFalse(self.thread.is_alive())
        self.assertEqual(self.server.errors, [])

    def request(self, path, body=None, headers=None):
        return request(self.client, self.server.instance_id, path, body, headers)

    def start(self, headers=None):
        result = self.request("/v1/asr/start", self.body, headers)
        self.assertEqual(result[0], 200)
        return result

    def chunk(self, sequence, headers=None):
        fields = {"X-Recording-Generation": "1", **(headers or {})}
        return self.request(self.path + f"/chunk?sequence={sequence}", b"\0" * 640, fields)

    def assert_closed(self, result, status):
        self.assertEqual(result[0], status)
        self.assertEqual(result[1]["Connection"], "close")
        self.assertIsNone(self.client.sock)
        self.assertTrue(self.server.closed_request.wait(1))

    def test_start_and_many_chunks_reuse_connection_then_explicit_final_close(self):
        started = self.start()
        self.assertEqual(started[1]["Connection"], "keep-alive")
        active_socket = self.client.sock
        address = active_socket.getsockname()
        times = []
        for sequence in range(24):
            result = self.chunk(sequence)
            self.assertEqual(result[0], 200)
            self.assertEqual(result[1]["Connection"], "keep-alive")
            self.assertEqual(result[2]["sequence"], sequence)
            self.assertEqual(result[2]["session_id"], self.body["session_id"])
            self.assertEqual(result[2]["instance_id"], self.server.instance_id)
            self.assertIs(self.client.sock, active_socket)
            self.assertEqual(self.client.sock.getsockname(), address)
            times.append(result[3])
        result = self.request(self.path + "/final", {"recording_generation": 1, "sequence": 24},
                              {"Connection": "close"})
        self.assert_closed(result, 200)
        self.assertTrue(result[2]["final"])
        self.assertEqual(self.server.accepted_addresses, [address])
        self.assertEqual(self.server.socket_options, [{"timeout": 3.0, "tcp_nodelay": 1}])
        self.assertEqual(len({thread for _, thread in self.engine.calls}), 1)
        self.assertIsNone(self.server.recognition.active)
        OBSERVATIONS["sequential_24_chunks"] = {
            "accepted_connections": len(self.server.accepted_addresses), "chunk_roundtrip_ms": times,
            "sum_chunk_ms": sum(times), "maximum_chunk_ms": max(times),
            "scope": "Fake-engine loopback observation, not real ASR throughput; no tiny wall-time assertion",
        }

    def test_healthy_final_releases_decoder_and_keeps_connection_for_new_identity(self):
        first = self.start()
        connection_id = int(first[1]["X-ASR-Connection-Id"])
        self.assertGreater(connection_id, 0)
        self.assertEqual(int(first[1]["X-ASR-Request-Id"]), 1)
        stream = self.server.recognition.active.stream
        self.assertEqual(self.chunk(0)[0], 200)
        final = self.request(self.path + "/final", {"recording_generation": 1, "sequence": 1})
        self.assertEqual(final[1]["Connection"], "keep-alive")
        self.assertEqual(int(final[1]["X-ASR-Connection-Id"]), connection_id)
        self.assertEqual(int(final[1]["X-ASR-Request-Id"]), 3)
        self.assertIsNone(self.server.recognition.active)
        self.body = start_body()
        self.body["recording_generation"] = 2
        self.path = "/v1/asr/" + self.body["session_id"]
        second = self.start()
        self.assertEqual(int(second[1]["X-ASR-Connection-Id"]), connection_id)
        self.assertEqual(int(second[1]["X-ASR-Request-Id"]), 4)
        self.assertEqual(second[2]["revision"], 0)
        self.assertEqual(second[2]["text"], "")
        self.assertIsNot(self.server.recognition.active.stream, stream)
        self.assert_closed(self.request(self.path + "/cancel", {"recording_generation": 2}), 200)
        self.assertEqual(len(self.server.accepted_addresses), 1)

    def test_cancel_closes_persistent_connection_and_releases_utterance(self):
        self.start()
        self.chunk(0)
        self.assert_closed(self.request(self.path + "/cancel", {"recording_generation": 1}), 200)
        self.assertIsNone(self.server.recognition.active)
        self.assertEqual(len(self.server.accepted_addresses), 1)

    def test_sequence_error_closes_without_retry_or_consuming_audio(self):
        self.start()
        self.assert_closed(self.chunk(1), 409)
        self.assertEqual(self.server.recognition.active.sequence, 0)
        self.assertEqual([name for name, _ in self.engine.calls], ["start"])

    def test_authentication_error_closes_without_touching_decoder(self):
        self.start()
        self.assert_closed(self.chunk(0, {"Authorization": "Bearer wrong"}), 401)
        self.assertEqual(self.server.recognition.active.sequence, 0)

    def test_wrong_instance_closes_without_touching_decoder(self):
        self.start()
        self.assert_closed(self.chunk(0, {"X-ASR-Instance": uuid4().hex}), 409)
        self.assertEqual(self.server.recognition.active.sequence, 0)

    def test_health_closes_even_when_called_on_persistent_connection(self):
        self.start()
        self.assert_closed(self.request("/health"), 200)
        self.assertIsNotNone(self.server.recognition.active)

    def test_malformed_request_closes(self):
        self.assert_closed(self.request("/v1/asr/start", b"null"), 400)
        self.assertEqual(self.engine.calls, [])

    def test_inference_failure_closes_without_exposing_exception(self):
        self.start()
        self.engine.fail_chunk = True
        result = self.chunk(0)
        self.assert_closed(result, 502)
        self.assertNotIn("private", json.dumps(result[2]))
        self.assertIsNone(self.server.recognition.active)

    def test_client_connection_close_is_respected(self):
        self.assert_closed(self.start({"Connection": "keep-alive, Close"}), 200)

    def test_http10_does_not_become_persistent(self):
        self.client._http_vsn, self.client._http_vsn_str = 10, "HTTP/1.0"
        self.assert_closed(self.start({"Connection": "keep-alive"}), 200)

    def test_buffered_http_pipeline_retains_all_requests_and_terminal_close(self):
        def wire(path, body, headers=None):
            if isinstance(body, dict):
                body = json.dumps(body).encode()
            fields = {"Host": "127.0.0.1", "Authorization": "Bearer " + TOKEN,
                      "X-ASR-Instance": self.server.instance_id,
                      "Content-Length": str(len(body)), **(headers or {})}
            head = f"POST {path} HTTP/1.1\r\n" + "".join(f"{k}: {v}\r\n" for k, v in fields.items())
            return head.encode() + b"\r\n" + body

        with socket.create_connection(("127.0.0.1", self.server.server_port), timeout=2) as client:
            # One write deliberately puts following requests in the reader's
            # buffer; testing socket readiness alone would lose this case.
            client.sendall(wire("/v1/asr/start", self.body)
                           + wire(self.path + "/chunk?sequence=0", b"\0" * 640,
                                  {"X-Recording-Generation": "1"})
                           + wire(self.path + "/final", {"recording_generation": 1, "sequence": 1},
                                  {"Connection": "close"}))
            with client.makefile("rb") as stream:
                for expected in ("keep-alive", "keep-alive", "close"):
                    self.assertEqual(stream.readline().split()[:2], [b"HTTP/1.1", b"200"])
                    headers = {}
                    while True:
                        line = stream.readline()
                        if line == b"\r\n":
                            break
                        key, value = line.decode().split(":", 1)
                        headers[key.lower()] = value.strip()
                    self.assertEqual(headers["connection"], expected)
                    response = json.loads(stream.read(int(headers["content-length"])))
                    self.assertEqual(response["session_id"], self.body["session_id"])
                self.assertTrue(response["final"])
                self.assertEqual(stream.read(1), b"")
        self.assertEqual([name for name, _ in self.engine.calls], ["start", "chunk", "final"])
        self.assertEqual(len(self.server.accepted_addresses), 1)

    def test_stop_interrupts_partial_request_body_without_inference_or_reply(self):
        second_read = threading.Event()
        original_read = asr_service._StoppableSocketReader.readinto
        counts = []

        def observed_read(reader, buffer):
            if reader.server is self.server:
                counts.append(1)
                if len(counts) >= 2:
                    second_read.set()
            return original_read(reader, buffer)

        with patch.object(asr_service._StoppableSocketReader, "readinto", observed_read):
            with socket.create_connection(("127.0.0.1", self.server.server_port), timeout=2) as client:
                body = json.dumps(self.body).encode()
                header = ("POST /v1/asr/start HTTP/1.1\r\nHost: 127.0.0.1\r\n"
                          f"Authorization: Bearer {TOKEN}\r\nX-ASR-Instance: {self.server.instance_id}\r\n"
                          f"Content-Length: {len(body)}\r\n\r\n").encode()
                client.sendall(header + body[:1])
                self.assertTrue(second_read.wait(1))
                stopper = threading.Thread(target=self.server.shutdown, daemon=True)
                stopper.start()
                stopper.join(2)
                self.assertFalse(stopper.is_alive())
                self.assertEqual(client.recv(1), b"")
        self.assertEqual(self.engine.calls, [])

    def test_shutdown_wins_race_before_accepted_socket_registration(self):
        accepted, release, stopping = threading.Event(), threading.Event(), threading.Event()
        original_accept = HTTPServer.get_request
        original_stop = self.server._stop_connections

        def gated_accept(server):
            result = original_accept(server)
            accepted.set()
            if not release.wait(2):
                result[0].close()
                raise OSError("test acceptance barrier timed out")
            return result

        def observe_stop():
            original_stop()
            stopping.set()

        with patch.object(HTTPServer, "get_request", gated_accept), \
                patch.object(self.server, "_stop_connections", observe_stop):
            self.client.request("POST", "/v1/asr/start", json.dumps(self.body),
                                {"Authorization": "Bearer " + TOKEN,
                                 "X-ASR-Instance": self.server.instance_id})
            self.assertTrue(accepted.wait(1))
            stopper = threading.Thread(target=self.server.shutdown, daemon=True)
            stopper.start()
            try:
                self.assertTrue(stopping.wait(1))
            finally:
                release.set()
            stopper.join(2)
            self.assertFalse(stopper.is_alive())
        self.assertEqual(self.server.handler_calls, 0)
        self.assertEqual(self.engine.calls, [])
        self.server.server_close()
        self.assertEqual(self.server.active_connections, 0)
        with self.assertRaises((RemoteDisconnected, ConnectionResetError)):
            self.client.getresponse()

    def test_shutdown_wins_race_after_registration_before_handler(self):
        registered, release, stopping = threading.Event(), threading.Event(), threading.Event()
        original_accept, original_stop = self.server.get_request, self.server._stop_connections

        def gated_accept():
            result = original_accept()
            registered.set()
            if not release.wait(2):
                raise OSError("test registration barrier timed out")
            return result

        def observe_stop():
            original_stop()
            stopping.set()

        with patch.object(self.server, "get_request", gated_accept), \
                patch.object(self.server, "_stop_connections", observe_stop):
            self.client.request("POST", "/v1/asr/start", json.dumps(self.body),
                                {"Authorization": "Bearer " + TOKEN,
                                 "X-ASR-Instance": self.server.instance_id})
            self.assertTrue(registered.wait(1))
            stopper = threading.Thread(target=self.server.shutdown, daemon=True)
            stopper.start()
            try:
                self.assertTrue(stopping.wait(1))
            finally:
                release.set()
            stopper.join(2)
            self.assertFalse(stopper.is_alive())
        self.assertEqual(self.engine.calls, [])
        self.server.server_close()
        self.assertEqual(self.server.active_connections, 0)


class Source:
    def __init__(self):
        self.lines = queue.Queue()

    def readline(self, _limit):
        return self.lines.get(timeout=5)

    def send(self, body):
        self.lines.put(json.dumps(body) + "\n")


class ReaderDeadlineTests(unittest.TestCase):
    def test_short_polls_only_timeout_after_original_three_second_read_budget(self):
        now, polls = [0.0], []

        def poll(readers, writers, errors, duration):
            polls.append(duration)
            now[0] += duration
            return [], [], []

        server = SimpleNamespace(stopping=False)
        connection = SimpleNamespace(recv_into=lambda _: self.fail("empty socket must not be read"))
        with patch.object(asr_service, "time", SimpleNamespace(monotonic=lambda: now[0])), \
                patch.object(asr_service, "select", SimpleNamespace(select=poll)):
            with asr_service._StoppableSocketReader(connection, server, 3) as reader:
                with self.assertRaises(TimeoutError):
                    reader.readinto(bytearray(16))
        self.assertAlmostEqual(now[0], 3.0)
        self.assertTrue(all(0 < duration <= .05 for duration in polls))
        self.assertGreaterEqual(len(polls), 60)


class Events:
    def __init__(self):
        self.lines = queue.Queue()
        self.all = []

    def write(self, line):
        value = json.loads(line)
        self.all.append(value)
        self.lines.put(value)

    def flush(self):
        pass

    def next(self, state):
        value = self.lines.get(timeout=2)
        assert value["state"] == state, value
        return value


class OwnerStopTests(unittest.TestCase):
    def test_stop_and_owner_eof_wake_idle_unfinished_keepalive_before_read_timeout(self):
        for stop_command in (True, False):
            with self.subTest(stop_command=stop_command), tempfile.TemporaryDirectory() as directory:
                source, events, engine = Source(), Events(), Engine()
                instance, owned, codes = uuid4().hex, [], []

                def server_factory(address, handler):
                    server = ObservedServer(address, handler)
                    owned.append(server)
                    return server

                with patch.object(managed_asr, "ManagedServer", server_factory):
                    worker = threading.Thread(target=lambda: codes.append(managed_asr.run(
                        Path(directory), instance, source, events, engine_factory=lambda _: engine)), daemon=True)
                    worker.start()
                    events.next("awaiting_owner")
                    source.send({"protocol_version": 1, "instance_id": instance, "bearer_token": TOKEN})
                    events.next("loading")
                    events.next("warming")
                    events.next("warm_ready")
                    server = owned[0]
                    client = HTTPConnection("127.0.0.1", server.server_port, timeout=2)
                    try:
                        body = start_body()
                        self.assertEqual(request(client, instance, "/v1/asr/start", body)[0], 200)
                        self.assertTrue(server.next_read.wait(1))
                        self.assertIsNotNone(client.sock)
                        self.assertIsNotNone(server.recognition.active)
                        began = time.perf_counter()
                        if stop_command:
                            source.send({"op": "stop", "instance_id": instance})
                        else:
                            source.lines.put("")
                        terminal = events.next("stopped")  # 2s bound is below the 3s idle read limit.
                        elapsed = (time.perf_counter() - began) * 1000
                        worker.join(2)
                        self.assertFalse(worker.is_alive())
                        self.assertTrue(terminal["http_closed"])
                        self.assertEqual(codes, [0])
                        self.assertEqual(client.sock.recv(1), b"")
                        self.assertEqual(server.active_connections, 0)
                        self.assertIsNone(server.recognition)
                        self.assertEqual(server.errors, [])
                        self.assertEqual(len(server.accepted_addresses), 1)
                        OBSERVATIONS["owner_stop_command" if stop_command else "owner_eof"] = {
                            "stop_to_http_closed_ms": elapsed, "idle_read_limit_ms": 3000,
                            "unfinished_utterance": True, "client_socket_kept_open_until_terminal": True,
                            "scope": "Fake managed service HTTP/thread closure; not Rust Job/interpreter exit proof",
                        }
                    finally:
                        client.close()
                        source.lines.put("")
                        worker.join(4)
                        self.assertFalse(worker.is_alive())


if __name__ == "__main__":
    unittest.main()
