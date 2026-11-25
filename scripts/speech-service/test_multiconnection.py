"""Bounded real-loopback concurrency tests; stdlib/fake engine only."""
from http.client import HTTPConnection, RemoteDisconnected
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import socket
import struct
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
from uuid import uuid4

import asr_service
import managed_asr
import test_connection_reuse as support

OBSERVATIONS = {}
REQUEST = threading.local()
SERIAL_SHA256 = "c2c8eb8e10849c63751d4fabb82821e292a34aae17831f736e3a238c7a476af0"


class AuditHandler(support.ObservedHandler):
    def setup(self):
        super().setup()
        with self.server.changed:
            self.server.handler_threads[self.connection_id] = threading.get_ident()
            self.server.changed.notify_all()

    def do_POST(self):
        REQUEST.path = self.path
        super().do_POST()

    def reply(self, status, body, keep_alive=False):
        gate = self.server.reply_gate
        if gate is not None and gate[0](self.path, status):
            self.server.reply_lock_owned = self.server.recognition.lock._is_owned()
            gate[1].set()
            if not gate[2].wait(4):
                raise RuntimeError("bounded fake reply barrier expired")
        super().reply(status, body, keep_alive)


class AuditServer(support.ObservedServer):
    def __init__(self, address, _handler=AuditHandler):
        self.changed = threading.Condition()
        self.handler_threads = {}
        self.close_threads = {}
        self.maximum_connections = 0
        self.reply_gate = None
        self.reply_lock_owned = None
        self.joined = False
        self.recognition_present_after_join = False
        super().__init__(address)
        self.RequestHandlerClass = AuditHandler

    def get_request(self):
        result = super().get_request()
        with self.changed:
            self.maximum_connections = max(self.maximum_connections, self.active_connections)
            self.changed.notify_all()
        return result

    def close_request(self, request):
        connection_id = self.connection_id(request)
        super().close_request(request)
        with self.changed:
            self.close_threads[connection_id] = threading.get_ident()
            self.changed.notify_all()

    def server_close(self):
        super().server_close()
        self.joined = True
        self.recognition_present_after_join = getattr(self, "recognition", None) is not None

    def wait_for(self, predicate, timeout=2):
        with self.changed:
            return self.changed.wait_for(predicate, timeout)


def live(server, engine):
    server.token, server.instance_id = support.TOKEN, uuid4().hex
    server.recognition = asr_service.Recognition(engine)
    server.ready = server.warmup_completed = True
    thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=.01), daemon=True)
    thread.start()
    return thread


def close_server(server, thread):
    if thread.is_alive():
        server.shutdown()
    thread.join(2)
    server.server_close()
    if thread.is_alive():
        raise AssertionError("accept loop did not stop")


class MultiTests(unittest.TestCase):
    def setUp(self):
        self.engine = support.Engine()
        self.server = AuditServer(("127.0.0.1", 0))
        self.thread = live(self.server, self.engine)
        self.clients = []

    def tearDown(self):
        for client in self.clients:
            client.close()
        close_server(self.server, self.thread)
        self.assertEqual(self.server.active_connections, 0)
        self.assertEqual(self.server.errors, [])

    def client(self):
        client = HTTPConnection("127.0.0.1", self.server.server_port, timeout=2)
        self.clients.append(client)
        return client

    def request(self, client, path, body=None, headers=None):
        return support.request(client, self.server.instance_id, path, body, headers)

    def test_unread_first_response_does_not_block_second_connection(self):
        first, second = self.client(), self.client()
        body = support.start_body()
        first.request("POST", "/v1/asr/start", json.dumps(body), {
            "Authorization": "Bearer " + support.TOKEN, "X-ASR-Instance": self.server.instance_id})
        held = first.getresponse()
        self.addCleanup(held.close)
        self.assertEqual(held.status, 200)
        first_id = int(held.getheader("X-ASR-Connection-Id"))
        self.assertTrue(self.server.next_read.wait(1))
        result = self.request(second, f"/v1/asr/{body['session_id']}/chunk?sequence=0", b"\0" * 640,
                              {"X-Recording-Generation": "1"})
        self.assertEqual(result[0], 200)
        second_id = int(result[1]["X-ASR-Connection-Id"])
        self.assertNotEqual(first_id, second_id)
        self.assertEqual(int(result[1]["X-ASR-Request-Id"]), 1)
        self.assertFalse(held.isclosed())  # Still unconsumed, not returned to a client pool.
        self.assertEqual(self.server.active_connections, 2)
        OBSERVATIONS["new_two_connections"] = {
            "first_response_held_unread": True, "first_connection_id": first_id,
            "second_connection_id": second_id, "second_status": result[0],
            "second_roundtrip_ms": result[3], "client_timeout_seconds": 2,
            "scope": "Controlled Python clients with actual handler/fake engine; not a claim about the failed real row",
        }
        held.close()

    def test_socket_to_id_mapping_remains_stable_across_concurrent_idle_connections(self):
        first, second = self.client(), self.client()
        body = support.start_body()
        a = self.request(first, "/v1/asr/start", body)
        path = f"/v1/asr/{body['session_id']}"
        b = self.request(second, path + "/chunk?sequence=0", b"\0" * 640,
                         {"X-Recording-Generation": "1"})
        again = self.request(first, path + "/chunk?sequence=1", b"\0" * 640,
                             {"X-Recording-Generation": "1"})
        self.assertEqual([a[0], b[0], again[0]], [200, 200, 200])
        self.assertNotEqual(a[1]["X-ASR-Connection-Id"], b[1]["X-ASR-Connection-Id"])
        self.assertEqual(a[1]["X-ASR-Connection-Id"], again[1]["X-ASR-Connection-Id"])
        self.assertEqual(again[1]["X-ASR-Request-Id"], "2")
        self.assertEqual(b[1]["X-ASR-Request-Id"], "1")

    def test_client_reset_of_unread_response_closes_only_that_transport(self):
        client = self.client()
        body = support.start_body()
        client.request("POST", "/v1/asr/start", json.dumps(body), {
            "Authorization": "Bearer " + support.TOKEN, "X-ASR-Instance": self.server.instance_id})
        held = client.getresponse()
        self.assertEqual(held.status, 200)
        self.assertTrue(self.server.next_read.wait(1))
        # Force a reset on this test client's own socket, not a global setting.
        linger = struct.pack("HH" if os.name == "nt" else "ii", 1, 0)
        client.sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, linger)
        held.close()
        client.close()
        self.assertTrue(self.server.wait_for(lambda: self.server.active_connections == 0))
        self.assertEqual(self.server.errors, [])
        self.assertEqual(self.server.recognition.active.session_id, body["session_id"])

    def test_partial_body_read_does_not_hold_recognition_lock(self):
        slow = socket.create_connection(("127.0.0.1", self.server.server_port), timeout=2)
        self.addCleanup(slow.close)
        incomplete = json.dumps(support.start_body()).encode()
        slow.sendall(("POST /v1/asr/start HTTP/1.1\r\nHost: localhost\r\n"
                      f"Authorization: Bearer {support.TOKEN}\r\nX-ASR-Instance: {self.server.instance_id}\r\n"
                      f"Content-Length: {len(incomplete)}\r\n\r\n").encode() + incomplete[:1])
        self.assertTrue(self.server.wait_for(lambda: len(self.server.handler_threads) == 1))
        complete = support.start_body()
        result = self.request(self.client(), "/v1/asr/start", complete)
        self.assertEqual(result[0], 200)
        self.assertEqual(self.server.recognition.active.session_id, complete["session_id"])
        self.assertEqual([name for name, _ in self.engine.calls], ["start"])

    def test_error_response_write_is_outside_lock_and_cannot_clear_new_stream(self):
        first, second = self.client(), self.client()
        original = support.start_body()
        self.request(first, "/v1/asr/start", original)
        waiting, release = threading.Event(), threading.Event()
        self.addCleanup(release.set)
        self.server.reply_gate = (lambda _path, status: status == 502, waiting, release)
        self.engine.fail_chunk = True
        result, errors = [], []

        def failing_chunk():
            try:
                result.append(self.request(first, f"/v1/asr/{original['session_id']}/chunk?sequence=0",
                                           b"\0" * 640, {"X-Recording-Generation": "1"}))
            except Exception as error:
                errors.append(type(error).__name__)

        worker = threading.Thread(target=failing_chunk, daemon=True)
        worker.start()
        try:
            self.assertTrue(waiting.wait(1))
            self.assertFalse(self.server.reply_lock_owned)
            self.engine.fail_chunk = False
            current = support.start_body()
            current["recording_generation"] = 2
            self.assertEqual(self.request(second, "/v1/asr/start", current)[0], 200)
        finally:
            release.set()
            worker.join(2)
        self.assertFalse(worker.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(result[0][0], 502)
        self.assertEqual(self.server.recognition.active.session_id, current["session_id"])
        self.assertEqual(self.request(second, f"/v1/asr/{current['session_id']}/chunk?sequence=0",
                                     b"\0" * 640, {"X-Recording-Generation": "2"})[0], 200)

    def test_one_inference_lock_and_expiry_does_not_block_accept_loop(self):
        entered, release, second_lock_attempted = threading.Event(), threading.Event(), threading.Event()
        self.addCleanup(release.set)
        tracking = threading.Lock()
        active, maximum = [0], [0]
        original_chunk = self.engine.chunk

        def gated_chunk(stream, pcm):
            with tracking:
                active[0] += 1
                maximum[0] = max(maximum[0], active[0])
            try:
                if not stream:
                    entered.set()
                    if not release.wait(4):
                        raise RuntimeError("bounded fake inference barrier expired")
                return original_chunk(stream, pcm)
            finally:
                with tracking:
                    active[0] -= 1

        real_lock = self.server.recognition.lock

        class WatchedLock:
            def acquire(self, *args, **kwargs):
                if getattr(REQUEST, "path", "").endswith("?sequence=1"):
                    second_lock_attempted.set()
                return real_lock.acquire(*args, **kwargs)

            def release(self):
                real_lock.release()

            def __enter__(self):
                self.acquire()
                return self

            def __exit__(self, *_):
                self.release()

        self.server.recognition.lock = WatchedLock()
        self.engine.chunk = gated_chunk
        first, second, health = self.client(), self.client(), self.client()
        body = support.start_body()
        self.request(first, "/v1/asr/start", body)
        outputs, errors = {}, []

        def chunk(client, number):
            try:
                outputs[number] = self.request(client, f"/v1/asr/{body['session_id']}/chunk?sequence={number}",
                                               b"\0" * 640, {"X-Recording-Generation": "1"})
            except Exception as error:
                errors.append(type(error).__name__)

        a = threading.Thread(target=chunk, args=(first, 0), daemon=True)
        b = threading.Thread(target=chunk, args=(second, 1), daemon=True)
        a.start()
        try:
            self.assertTrue(entered.wait(1))
            b.start()
            self.assertTrue(second_lock_attempted.wait(1))
            self.assertNotIn(1, outputs)
            self.assertEqual(self.request(health, "/health")[0], 200)
        finally:
            release.set()
            a.join(2)
            if b.ident is not None:
                b.join(2)
        self.assertEqual(errors, [])
        self.assertFalse(a.is_alive() or b.is_alive())
        self.assertEqual([outputs[index][0] for index in (0, 1)], [200, 200])
        self.assertEqual(maximum[0], 1)
        self.assertEqual(self.server.recognition.active.sequence, 2)
        OBSERVATIONS["serialized_inference"] = {"maximum_simultaneous_fake_chunk_calls": maximum[0],
            "health_served_while_inference_held": True, "second_chunk_waited_on_shared_lock": True}

    def test_eight_connection_cap_rejects_ninth_and_recovers_released_slot(self):
        sockets = []
        self.addCleanup(lambda: [client.close() for client in sockets])
        for _ in range(8):
            sockets.append(socket.create_connection(("127.0.0.1", self.server.server_port), timeout=2))
        self.assertTrue(self.server.wait_for(lambda: len(self.server.handler_threads) == 8))
        self.assertEqual(self.server.active_connections, 8)
        ninth = socket.create_connection(("127.0.0.1", self.server.server_port), timeout=2)
        sockets.append(ninth)
        try:
            self.assertEqual(ninth.recv(1), b"")
        except ConnectionResetError:
            pass
        self.assertEqual(self.server.capacity_rejections, 1)
        self.assertEqual(self.server.active_connections, 8)
        sockets[0].close()
        self.assertTrue(self.server.wait_for(lambda: self.server.active_connections == 7))
        sockets.append(socket.create_connection(("127.0.0.1", self.server.server_port), timeout=2))
        self.assertTrue(self.server.wait_for(lambda: len(self.server.handler_threads) == 9))
        self.assertEqual(self.server.active_connections, 8)
        self.assertEqual(self.server.maximum_connections, 8)
        OBSERVATIONS["capacity"] = {"cap": 8, "maximum_registered_connections": 8,
            "capacity_rejections": 1, "released_slot_reused": True}


class SerialControlTests(unittest.TestCase):
    def test_frozen_serial_server_stalls_second_socket_while_first_response_is_held(self):
        supplied = os.environ.get("AI_TUTOR_ASR_SERIAL_BASELINE")
        if not supplied:
            self.skipTest("explicit frozen serial baseline path required for control")
        path = Path(supplied).resolve(strict=True)
        self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), SERIAL_SHA256)
        spec = importlib.util.spec_from_file_location("asr_serial_baseline_control", path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        idle = threading.Event()

        class SerialHandler(module.Handler):
            def handle_one_request(self):
                if getattr(self, "seen", False):
                    idle.set()
                self.seen = True
                super().handle_one_request()

        server = module.RecognitionServer(("127.0.0.1", 0), SerialHandler)
        engine = support.Engine()
        server.token, server.instance_id = support.TOKEN, uuid4().hex
        server.recognition = module.Recognition(engine)
        server.ready = server.warmup_completed = True
        thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=.01), daemon=True)
        thread.start()
        first = HTTPConnection("127.0.0.1", server.server_port, timeout=2)
        second = HTTPConnection("127.0.0.1", server.server_port, timeout=2)
        held = None
        try:
            body = support.start_body()
            headers = {"Authorization": "Bearer " + support.TOKEN, "X-ASR-Instance": server.instance_id}
            first.request("POST", "/v1/asr/start", json.dumps(body), headers)
            held = first.getresponse()
            self.assertEqual(held.status, 200)
            self.assertTrue(idle.wait(1))
            began = time.perf_counter()
            second.request("POST", f"/v1/asr/{body['session_id']}/chunk?sequence=0", b"\0" * 640,
                           {**headers, "X-Recording-Generation": "1"})
            sent_ms = (time.perf_counter() - began) * 1000
            with self.assertRaises(TimeoutError):
                second.getresponse()
            elapsed_ms = (time.perf_counter() - began) * 1000
            self.assertEqual([name for name, _ in engine.calls], ["start"])
            self.assertFalse(held.isclosed())
            OBSERVATIONS["serial_control"] = {"baseline_sha256": SERIAL_SHA256,
                "first_response_held_unread": True, "first_handler_waiting_for_next_request": True,
                "second_request_sent_ms": sent_ms, "second_response_timeout_ms": elapsed_ms,
                "client_timeout_seconds": 2, "fake_chunk_calls_before_stop": 0,
                "scope": "Controlled HOL mechanism only; not attribution of ordinal69 or any older model failure"}
        finally:
            # Stop while the first client remains open. The frozen cooperative
            # reader handles that lifecycle; do not service/retry the queued PCM.
            close_server(server, thread)
            if held is not None:
                held.close()
            first.close()
            second.close()


class ManagedMultiStopTests(unittest.TestCase):
    def test_owner_stop_joins_idle_and_partial_handlers_before_releasing_recognition(self):
        source, events = support.Source(), support.Events()
        instance, servers, codes = uuid4().hex, [], []

        def server_factory(address, handler):
            server = AuditServer(address, handler)
            servers.append(server)
            return server

        with tempfile.TemporaryDirectory() as directory, patch.object(managed_asr, "ManagedServer", server_factory):
            worker = threading.Thread(target=lambda: codes.append(managed_asr.run(
                Path(directory), instance, source, events, engine_factory=lambda _: support.Engine())), daemon=True)
            worker.start()
            events.next("awaiting_owner")
            source.send({"protocol_version": 1, "instance_id": instance, "bearer_token": support.TOKEN})
            events.next("loading"); events.next("warming"); events.next("warm_ready")
            server = servers[0]
            first = HTTPConnection("127.0.0.1", server.server_port, timeout=2)
            sockets = []
            try:
                self.assertEqual(support.request(first, instance, "/v1/asr/start", support.start_body())[0], 200)
                for partial in (True, False):
                    client = socket.create_connection(("127.0.0.1", server.server_port), timeout=2)
                    sockets.append(client)
                    if partial:
                        client.sendall(("POST /v1/asr/start HTTP/1.1\r\nHost: localhost\r\n"
                                        f"Authorization: Bearer {support.TOKEN}\r\nX-ASR-Instance: {instance}\r\n"
                                        "Content-Length: 100\r\n\r\n{").encode())
                self.assertTrue(server.wait_for(lambda: len(server.handler_threads) == 3))
                began = time.perf_counter()
                source.send({"op": "stop", "instance_id": instance})
                terminal = events.next("stopped")
                elapsed = (time.perf_counter() - began) * 1000
                worker.join(2)
                self.assertFalse(worker.is_alive())
                self.assertEqual(codes, [0])
                self.assertTrue(terminal["http_closed"])
                self.assertTrue(server.joined and server.recognition_present_after_join)
                self.assertIsNone(server.recognition)
                self.assertEqual(server.active_connections, 0)
                self.assertEqual(server.handler_threads, server.close_threads)
                self.assertEqual(server.errors, [])
                OBSERVATIONS["managed_multi_stop"] = {"registered_handlers": 3,
                    "stop_to_http_closed_ms": elapsed, "clients_kept_open_until_terminal": True,
                    "all_handlers_joined_before_recognition_release": True,
                    "each_socket_closed_by_its_handler_thread": True,
                    "scope": "Fake managed Python lifecycle; no Rust Job/interpreter exit claim"}
            finally:
                first.close()
                for client in sockets:
                    client.close()
                source.lines.put("")
                worker.join(4)
                self.assertFalse(worker.is_alive())


if __name__ == "__main__":
    unittest.main()
