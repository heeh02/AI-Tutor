"""Exercise ownership and the real HTTP contract without audio devices."""
from http.server import HTTPServer
import json
import threading
import unittest
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from uuid import uuid4

from asr_service import Handler, Recognition, ProtocolError


class Engine:
    provider = "test acoustic engine"

    def create(self):
        return []

    def chunk(self, stream, pcm):
        stream.append(pcm)
        return "电路 circuit" if len(stream) >= 2 else "电路"

    def finish(self, stream):
        return "电路 circuit complete"


def start(sid=None, generation=1, duration=20000):
    return dict(protocol_version=1, session_id=sid or str(uuid4()), recording_generation=generation,
                sample_rate=16000, channels=1, encoding="pcm_s16le", language="auto", max_duration_ms=duration)


class OwnershipTests(unittest.TestCase):
    def setUp(self):
        self.now = 0
        self.service = Recognition(Engine(), clock=lambda: self.now)
        self.first = start()
        self.service.start(self.first)
        self.sid = self.first["session_id"]

    def test_old_cancel_and_final_cannot_touch_a_new_utterance(self):
        self.service.cancel(self.sid, 1)
        new = start(generation=2)
        self.service.start(new)
        self.service.cancel(self.sid, 1)
        with self.assertRaises(ProtocolError) as error:
            self.service.finish(self.sid, 1, 0)
        self.assertEqual(error.exception.status, 409)
        self.assertEqual(self.service.active.session_id, new["session_id"])

    def test_sequence_and_final_text_revision(self):
        self.assertEqual(self.service.chunk(self.sid, 1, 0, b"\0" * 3200)["revision"], 1)
        with self.assertRaises(ProtocolError):
            self.service.chunk(self.sid, 1, 0, b"\0" * 3200)
        self.assertEqual(self.service.active.samples, 1600)
        self.assertEqual(self.service.chunk(self.sid, 1, 1, b"\0" * 3200)["revision"], 2)
        self.assertEqual(self.service.chunk(self.sid, 1, 2, b"\0" * 2)["revision"], 2)
        with self.assertRaises(ProtocolError):
            self.service.finish(self.sid, 1, 2)
        result = self.service.finish(self.sid, 1, 3)
        self.assertEqual((result["sequence"], result["revision"], result["final"]), (3, 3, True))
        self.assertIsNone(self.service.active)

    def test_expiry_and_sample_budget_release_decoder(self):
        self.now = 31
        self.service.expire()
        self.assertIsNone(self.service.active)
        limited = start(duration=100)
        self.service.start(limited)
        sid = limited["session_id"]
        self.service.chunk(sid, 1, 0, b"\0" * 3200)
        with self.assertRaises(ProtocolError) as error:
            self.service.chunk(sid, 1, 1, b"\0" * 2)
        self.assertEqual(error.exception.status, 413)
        self.assertIsNone(self.service.active)


class HttpTests(unittest.TestCase):
    def setUp(self):
        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.server.recognition, self.server.token = Recognition(Engine()), "local-test-token"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.thread.join()
        self.server.server_close()

    def request(self, path, body=None, **headers):
        headers.setdefault("Authorization", "Bearer local-test-token")
        if isinstance(body, dict):
            body = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        req = Request(f"http://127.0.0.1:{self.server.server_port}" + path, body, headers)
        try:
            response = urlopen(req, timeout=3)
        except HTTPError as error:
            response = error
        with response:
            return response.status, json.loads(response.read())

    def test_authenticated_loopback_stream_and_cancel(self):
        self.assertEqual(self.request("/health", Authorization="")[0], 401)
        self.assertEqual(self.request("/health", Origin="https://example.com")[0], 401)
        self.assertEqual(self.request("/health")[0], 200)
        body = start()
        sid = body["session_id"]
        self.assertEqual(self.request("/v1/asr/start", body)[0], 200)
        self.assertEqual(self.request("/v1/asr/start", start())[0], 409)
        path = f"/v1/asr/{sid}"
        code, partial = self.request(path + "/chunk?sequence=0", b"\0" * 3200, **{"X-Recording-Generation": "1"})
        self.assertEqual((code, partial["text"], partial["sequence"]), (200, "电路", 0))
        self.assertEqual(self.request(path + "/chunk?sequence=1", b"\0" * 5000, **{"X-Recording-Generation": "1"})[0], 413)
        code, final = self.request(path + "/final", dict(recording_generation=1, sequence=1))
        self.assertEqual((code, final["final"], final["sequence"]), (200, True, 1))
        self.assertEqual(self.request(path + "/cancel", dict(recording_generation=1))[0], 200)
        self.assertEqual(self.request("/v1/asr/start", b"null")[0], 400)


if __name__ == "__main__":
    unittest.main()
