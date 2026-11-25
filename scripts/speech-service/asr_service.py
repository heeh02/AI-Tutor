"""One foreground inference stream over bounded authenticated loopback HTTP.

Rust owns capture, resampling, fallback and consent to send. This process only
loads the acoustic model and retains the current utterance's decoder state.
"""
import argparse
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, HTTPServer
from socketserver import ThreadingMixIn
import io
import json
from pathlib import Path
import secrets
import select
import threading
import time
from urllib.parse import parse_qs, urlsplit
from uuid import UUID


class ProtocolError(Exception):
    def __init__(self, status, message):
        self.status, self.message = status, message


@dataclass
class Utterance:
    session_id: str
    generation: int
    stream: object
    deadline: float
    max_samples: int
    sequence: int = 0
    samples: int = 0
    revision: int = 0
    text: str = ""


class Recognition:
    def __init__(self, engine, clock=time.monotonic):
        self.engine, self.clock = engine, clock
        self.active = None
        self.lock = threading.RLock()

    def expire(self):
        with self.lock:
            if self.active and self.clock() >= self.active.deadline:
                self.active = None

    @staticmethod
    def identity(session_id, generation):
        try:
            if str(UUID(session_id)) != session_id or type(generation) is not int or generation < 0:
                raise ValueError()
        except (ValueError, TypeError, AttributeError):
            raise ProtocolError(400, "Invalid stream identity")

    def start(self, body):
        with self.lock:
            sid, generation = body.get("session_id"), body.get("recording_generation")
            self.identity(sid, generation)
            duration = body.get("max_duration_ms")
            if (body.get("protocol_version") != 1 or body.get("sample_rate") != 16000
                    or body.get("channels") != 1 or body.get("encoding") != "pcm_s16le"
                    or body.get("language") != "auto" or type(duration) is not int
                    or not 100 <= duration <= 20000):
                raise ProtocolError(400, "Unsupported audio configuration")
            self.expire()
            if self.active:
                raise ProtocolError(409, "A foreground utterance is already active")
            self.active = Utterance(sid, generation, self.engine.create(), self.clock() + 30, duration * 16)
            return self.response(self.active)

    def current(self, sid, generation):
        with self.lock:
            self.identity(sid, generation)
            self.expire()
            item = self.active
            if item is None or (item.session_id, item.generation) != (sid, generation):
                raise ProtocolError(409, "Utterance expired or no longer current")
            return item

    def response(self, item, text=None, final=False):
        with self.lock:
            if text is not None and text != item.text:
                if len(text) > 8000:
                    self.active = None
                    raise ProtocolError(502, "Recognition result exceeds utterance limit")
                item.text, item.revision = text, item.revision + 1
            return {"protocol_version": 1, "session_id": item.session_id,
                    "recording_generation": item.generation, "revision": item.revision,
                    "text": item.text, "final": final, "provider": self.engine.provider}

    def chunk(self, sid, generation, sequence, pcm):
        with self.lock:
            item = self.current(sid, generation)
            if type(sequence) is not int or sequence != item.sequence:
                raise ProtocolError(409, "Audio sequence does not match")
            if not 0 < len(pcm) <= 3200 or len(pcm) % 2:
                raise ProtocolError(400, "Expected at most 100ms of mono PCM16")
            if item.samples + len(pcm) // 2 > item.max_samples:
                self.active = None
                raise ProtocolError(413, "Utterance duration exceeded")
            result = self.engine.chunk(item.stream, pcm)
            item.samples += len(pcm) // 2
            item.sequence += 1
            item.deadline = self.clock() + 30
            return {**self.response(item, result), "sequence": sequence}

    def finish(self, sid, generation, sequence):
        with self.lock:
            item = self.current(sid, generation)
            if type(sequence) is not int or sequence != item.sequence:
                raise ProtocolError(409, "Final sequence does not match")
            try:
                return {**self.response(item, self.engine.finish(item.stream), final=True), "sequence": sequence}
            finally:
                self.active = None

    def cancel(self, sid, generation):
        with self.lock:
            self.identity(sid, generation)
            if self.active and (self.active.session_id, self.active.generation) == (sid, generation):
                self.active = None
            return {"protocol_version": 1, "session_id": sid,
                    "recording_generation": generation, "canceled": True}


class _StoppableSocketReader(io.RawIOBase):
    """Keep BufferedReader semantics, but check owner stop during idle reads."""
    def __init__(self, connection, server, timeout):
        self.connection, self.server, self.timeout = connection, server, timeout

    def readable(self):
        return True

    def readinto(self, buffer):
        if self.closed:
            raise ValueError("read of closed socket stream")
        if not len(buffer):
            return 0
        deadline = time.monotonic() + self.timeout
        while not getattr(self.server, "stopping", False):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("ASR socket read timed out")
            ready, _, _ = select.select([self.connection], [], [], min(.05, remaining))
            if getattr(self.server, "stopping", False):
                return 0
            if ready:
                return self.connection.recv_into(buffer)
        return 0


class RecognitionServer(ThreadingMixIn, HTTPServer):
    """At most eight HTTP connections; one serialized recognition state."""
    MAX_CONNECTIONS = 8
    daemon_threads = False
    block_on_close = True

    def __init__(self, *args, **kwargs):
        self._connection_lock = threading.Lock()
        self._connection_slots = threading.BoundedSemaphore(self.MAX_CONNECTIONS)
        self._connections = {}
        self._connection_sequence = 0
        self.capacity_rejections = 0
        self._stopping = False
        super().__init__(*args, **kwargs)

    @property
    def stopping(self):
        with self._connection_lock:
            return self._stopping

    def get_request(self):
        connection, address = super().get_request()
        with self._connection_lock:
            self._connection_sequence += 1
            if self._stopping:
                connection.close()
                raise OSError("ASR server is stopping")
            if not self._connection_slots.acquire(blocking=False):
                self.capacity_rejections += 1
                connection.close()  # Not handed to a handler; no concurrent I/O.
                raise OSError("ASR connection capacity reached")
            self._connections[connection] = self._connection_sequence
        return connection, address

    def connection_id(self, request):
        with self._connection_lock:
            return self._connections.get(request)

    @property
    def active_connections(self):
        with self._connection_lock:
            return len(self._connections)

    def close_request(self, request):
        try:
            super().close_request(request)
        finally:
            with self._connection_lock:
                if self._connections.pop(request, None) is not None:
                    self._connection_slots.release()

    def _stop_connections(self):
        with self._connection_lock:
            self._stopping = True

    def shutdown(self):
        # The read adapter observes this flag within its bounded select loop.
        # Never close a socket concurrently with another Windows Winsock call.
        # A simultaneous accept observes the same flag before admitting work.
        self._stop_connections()
        super().shutdown()

    def server_close(self):
        self._stop_connections()
        # ThreadingMixIn closes the listener then joins all non-daemon handlers.
        # managed_asr releases Recognition only after this method returns.
        super().server_close()

    def service_actions(self):
        recognition = getattr(self, "recognition", None)
        if recognition is not None and recognition.lock.acquire(blocking=False):
            try:
                recognition.expire()
            finally:
                recognition.lock.release()


class Handler(BaseHTTPRequestHandler):
    # Healthy utterances can share the transport while each has its own decoder.
    # HTTP connections are bounded; one Recognition lock serializes inference.
    protocol_version = "HTTP/1.1"
    timeout = 3
    # Headers/body are separate writes; avoid Nagle + delayed-ACK chunk stalls.
    disable_nagle_algorithm = True

    def setup(self):
        super().setup()
        identify = getattr(self.server, "connection_id", None)
        self.connection_id = identify(self.connection) if identify is not None else None
        self.request_id = 0
        # No request bytes have been read yet. Replace only the raw reader,
        # retaining normal buffering (including pipelined requests) and the
        # original 3s socket timeout. Short polls never poison BufferedReader.
        self.rfile.close()
        self.rfile = io.BufferedReader(_StoppableSocketReader(
            self.connection, self.server, self.timeout))

    def handle_one_request(self):
        if getattr(self.server, "stopping", False):
            self.close_connection = True
            return
        try:
            super().handle_one_request()
        except ConnectionError:
            # A pooled client may reset an idle socket or abandon a response.
            # Close only this transport; decoder ownership is unchanged.
            self.close_connection = True
        except OSError:
            if not getattr(self.server, "stopping", False):
                raise
            self.close_connection = True

    def log_message(self, *_):
        pass

    def reply(self, status, body, keep_alive=False):
        if getattr(self.server, "stopping", False):
            self.close_connection = True
            return
        requested_close = any(token.strip().lower() == "close"
                              for value in self.headers.get_all("Connection", [])
                              for token in value.split(","))
        keep_alive = (keep_alive and self.request_version == "HTTP/1.1"
                      and not self.close_connection and not requested_close
                      and not getattr(self.server, "stopping", False))
        self.close_connection = not keep_alive
        instance = getattr(self.server, "instance_id", None)
        if instance is not None:
            body = {**body, "instance_id": instance}
        payload = json.dumps(body, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "keep-alive" if keep_alive else "close")
        # Assigned by this server's accept path, never a socket handle or port.
        # Bare HTTPServer unit fixtures do not invent transport identities.
        if self.connection_id is not None:
            self.send_header("X-ASR-Connection-Id", str(self.connection_id))
            self.send_header("X-ASR-Request-Id", str(self.request_id))
        self.end_headers()
        self.wfile.write(payload)

    def authorized(self):
        return (not self.headers.get("Origin") and secrets.compare_digest(
            self.headers.get("Authorization", ""), "Bearer " + self.server.token))

    def instance_matches(self):
        instance = getattr(self.server, "instance_id", None)
        return instance is None or self.headers.get("X-ASR-Instance") == instance

    def do_GET(self):
        self.request_id += 1  # Successfully parsed GET, including HTTP errors.
        if not self.authorized():
            self.reply(401, {"error": "Local service authentication required"})
        elif not self.instance_matches():
            self.reply(409, {"error": "Local service instance changed"})
        elif self.path == "/health":
            recognition = self.server.recognition
            self.reply(200, {"protocol_version": 1,
                             "ready": getattr(self.server, "ready", True),
                             "warmup_completed": getattr(self.server, "warmup_completed", True),
                             "provider": recognition.engine.provider if recognition else None})
        else:
            self.reply(404, {"error": "Unknown endpoint"})

    def do_POST(self):
        self.request_id += 1  # Successfully parsed POST, including HTTP errors.
        if getattr(self.server, "stopping", False):
            self.close_connection = True
            return
        if not self.authorized():
            self.reply(401, {"error": "Local service authentication required"})
            return
        if not self.instance_matches():
            self.reply(409, {"error": "Local service instance changed"})
            return
        if not (getattr(self.server, "ready", True)
                and getattr(self.server, "warmup_completed", True)):
            self.reply(503, {"error": "Local acoustic model is not warm"})
            return
        try:
            keep_alive = False
            path = urlsplit(self.path)
            length = int(self.headers.get("Content-Length", "-1"))
            if not 0 <= length <= 4096 or self.headers.get("Transfer-Encoding"):
                raise ProtocolError(413, "Request body limit exceeded")
            payload = self.rfile.read(length)
            if len(payload) != length:
                raise ProtocolError(400, "Incomplete request")
            if getattr(self.server, "stopping", False):
                self.close_connection = True
                return
            state = self.server.recognition
            with state.lock:
                if getattr(self.server, "stopping", False):
                    self.close_connection = True
                    return
                try:
                    if path.path == "/v1/asr/start":
                        body = json.loads(payload)
                        if not isinstance(body, dict):
                            raise ProtocolError(400, "Expected JSON object")
                        result = state.start(body)
                        keep_alive = True
                    else:
                        parts = path.path.strip("/").split("/")
                        if len(parts) != 4 or parts[:2] != ["v1", "asr"]:
                            raise ProtocolError(404, "Unknown endpoint")
                        sid, action = parts[2:]
                        if action == "chunk":
                            generation = int(self.headers.get("X-Recording-Generation", "-1"))
                            query = parse_qs(path.query)
                            result = state.chunk(sid, generation, int(query["sequence"][0]), payload)
                            keep_alive = True
                        else:
                            body = json.loads(payload)
                            if not isinstance(body, dict):
                                raise ProtocolError(400, "Expected JSON object")
                            generation = body.get("recording_generation")
                            if action == "final":
                                result = state.finish(sid, generation, body.get("sequence"))
                                keep_alive = True
                            elif action == "cancel":
                                result = state.cancel(sid, generation)
                            else:
                                raise ProtocolError(404, "Unknown endpoint")
                except (ProtocolError, ValueError, KeyError, UnicodeDecodeError, ConnectionError, TimeoutError):
                    raise
                except Exception:
                    # Clear a failed decoder before releasing the same state lock;
                    # a late error must not erase a newer recording.
                    state.active = None
                    raise
            self.reply(200, result, keep_alive=keep_alive)
        except ProtocolError as error:
            self.reply(error.status, {"error": error.message})
        except (ValueError, KeyError, UnicodeDecodeError):
            self.reply(400, {"error": "Malformed request"})
        except (ConnectionError, TimeoutError):
            self.close_connection = True
        except Exception:
            self.reply(502, {"error": "Local acoustic inference failed"})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--port", type=int, default=18867)
    parser.add_argument("--threads", type=int, choices=range(1, 5), default=2)
    args = parser.parse_args()
    root = args.root.resolve()
    descriptor = root / "runtime/speech-service.json"
    descriptor.parent.mkdir(parents=True, exist_ok=True)
    server = RecognitionServer(("127.0.0.1", args.port), Handler)
    server.timeout = 1
    from recognizer import Zipformer
    started = time.monotonic()
    engine = Zipformer(root / "models/speech/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20", args.threads)
    server.recognition = Recognition(engine)
    server.token = secrets.token_urlsafe(32)
    config = {"protocol_version": 1, "base_url": f"http://127.0.0.1:{server.server_port}", "token": server.token}
    temporary = descriptor.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(config), encoding="utf-8")
    temporary.replace(descriptor)
    print(json.dumps({"ready": True, "provider": engine.provider, "runtime": engine.runtime_version,
                      "load_and_warm_ms": round((time.monotonic() - started) * 1000, 1)}), flush=True)
    try:
        while True:
            server.handle_request()
            server.service_actions()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        if descriptor.exists() and json.loads(descriptor.read_text(encoding="utf-8")).get("token") == server.token:
            descriptor.unlink()


if __name__ == "__main__":
    main()
