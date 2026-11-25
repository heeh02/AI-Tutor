"""One offline GPU job, bounded authenticated loopback HTTP.

This is not a WSL/process owner or a streaming ASR implementation. Stdin EOF or
an exact Stop latches shutdown; process exit waits for real worker retirement.
"""
import argparse
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
from queue import Empty, Queue
import re
import secrets
import signal
import socket
from socketserver import ThreadingMixIn
import sys
import threading
import time
from uuid import UUID

from qwen_final_engine import FinalResult, InferenceCancelled, MAX_PCM_BYTES, QwenFinalEngine


class ServiceError(Exception):
    def __init__(self, status, code, details=None):
        self.status, self.code, self.details = status, code, details


def identity(request_id, generation):
    try:
        valid = str(UUID(request_id)) == request_id
    except (ValueError, TypeError, AttributeError):
        valid = False
    if not valid or type(generation) is not int or not 0 <= generation < 2**64:
        raise ServiceError(400, 'invalid_request_identity')


@dataclass
class Job:
    request_id: str
    generation: int
    pcm: bytearray
    canceled: bool = False
    result: object = None
    error: object = None
    error_metadata: object = None
    reply_ready: threading.Event = field(default_factory=threading.Event)
    retired: threading.Event = field(default_factory=threading.Event)


class FinalService:
    def __init__(self, engine, instance, token):
        if not re.fullmatch(r'[0-9a-f]{32}', instance) or not re.fullmatch(r'[A-Za-z0-9_.-]{24,256}', token):
            raise ValueError('invalid_private_service_identity')
        self.engine, self.instance, self.token = engine, instance, token
        self.condition = threading.Condition()
        self.phase, self.model_ready = 'not_ready', False
        self.active, self.worker = None, None
        self.last_generation = -1
        self.closing, self.preparing = False, False

    def health(self):
        with self.condition:
            return {'phase': self.phase, 'model_ready': self.model_ready,
                    'ready': self.phase == 'warm_ready' and not self.closing,
                    'warmup_completed': self.phase == 'warm_ready' and not self.closing,
                    'busy': self.active is not None, 'closing': self.closing,
                    'provider': self.engine.provider, 'inference_mode': 'offline_final'}

    def prepare(self, progress=lambda state: None):
        with self.condition:
            if self.phase != 'not_ready' or self.closing:
                raise ServiceError(409, 'preparation_already_started_or_closing')
            self.phase, self.preparing = 'loading', True
        try:
            progress(self.health())
            self.engine.load()
            with self.condition:
                self.model_ready = True
                if self.closing:
                    return
                self.phase = 'warming'
            progress(self.health())
            warm_result = self.engine.warm(cancelled=lambda: self.closing)
            if not isinstance(warm_result, FinalResult) or warm_result.payload()['possible_truncation']:
                raise ServiceError(502, 'warm_decoder_truncated')
            with self.condition:
                if self.closing:
                    return
                self.worker = threading.Thread(target=self._work, name='qwen-final-worker', daemon=False)
                self.worker.start()
                self.phase = 'warm_ready'
            progress(self.health())
        except InferenceCancelled:
            with self.condition:
                if not self.closing:
                    self.phase = 'failed'
                    raise
                self.phase = 'stopping'
            return
        except Exception:
            with self.condition:
                self.phase = 'stopping' if self.closing else 'failed'
            raise
        finally:
            with self.condition:
                self.preparing = False
                self.condition.notify_all()

    def check_admission(self, request_id, generation):
        identity(request_id, generation)
        with self.condition:
            if self.closing or self.phase != 'warm_ready':
                raise ServiceError(503, 'model_not_warm_or_closing')
            if self.active is not None:
                raise ServiceError(409, 'gpu_worker_busy')
            if generation <= self.last_generation:
                raise ServiceError(409, 'stale_or_replayed_generation')

    def submit(self, request_id, generation, pcm):
        if not isinstance(pcm, (bytes, bytearray)) or not 0 < len(pcm) <= MAX_PCM_BYTES or len(pcm) % 2:
            raise ServiceError(400, 'invalid_pcm_size')
        with self.condition:
            self.check_admission(request_id, generation)
            job = Job(request_id, generation, bytearray(pcm))
            self.active, self.last_generation = job, generation
            self.condition.notify_all()
            return job

    def cancel(self, request_id, generation):
        identity(request_id, generation)
        with self.condition:
            job = self.active
            matched = job is not None and (job.request_id, job.generation) == (request_id, generation)
            if matched:
                job.canceled = True
                job.result = None
                job.reply_ready.set()
            return {'request_id': request_id, 'recording_generation': generation,
                    'canceled': matched, 'worker_retired': job.retired.is_set() if matched else None,
                    'gpu_busy': self.active is not None}

    def wait(self, job, timeout=60):
        if not job.reply_ready.wait(timeout):
            self.cancel(job.request_id, job.generation)
            raise ServiceError(504, 'final_deadline_exceeded')
        with self.condition:
            if job.canceled:
                raise ServiceError(409, 'request_canceled')
            if job.error:
                raise ServiceError(502, job.error, job.error_metadata)
            if job.result is None or not job.retired.is_set():
                raise ServiceError(502, 'missing_retired_final')
            return {'request_id': job.request_id, 'recording_generation': job.generation,
                    'final': True, 'provider': self.engine.provider, **job.result}

    def _work(self):
        while True:
            with self.condition:
                self.condition.wait_for(lambda: self.active is not None or self.closing)
                if self.active is None and self.closing:
                    return
                job = self.active
            result, error, error_metadata = None, None, None
            try:
                if not job.canceled:
                    # No service/state lock is held during Torch inference.
                    output = self.engine.transcribe(job.pcm, cancelled=lambda job=job: job.canceled or self.closing)
                    if not isinstance(output, FinalResult) or not isinstance(output.text, str) or len(output.text) > 8000:
                        raise ValueError('invalid_engine_final')
                    result = output.payload()
                    if len(json.dumps(result, ensure_ascii=False).encode('utf-8')) > 60000:
                        raise ValueError('engine_final_too_large')
                    if result['possible_truncation']:
                        error = 'decoder_truncated'
                        error_metadata = {key: result[key] for key in
                                          ('generation_calls', 'max_new_tokens', 'possible_truncation')}
                        result = None
            except InferenceCancelled:
                with self.condition:
                    job.canceled = True
                    job.result = None
                    job.reply_ready.set()
                # A requested stop is not a poisoned model. Busy remains held
                # until the unchanged retirement finally below actually runs.
            except Exception:
                error = 'local_gpu_inference_failed'
            finally:
                with self.condition:
                    # Retire only this exact job; cancellation never creates a replacement slot.
                    job.pcm[:] = b'\0' * len(job.pcm)
                    job.pcm.clear()
                    if not job.canceled and not self.closing:
                        job.result, job.error, job.error_metadata = result, error, error_metadata
                    if error == 'local_gpu_inference_failed':
                        self.phase = 'stopping' if self.closing else 'failed'
                    if self.active is job:
                        self.active = None
                    job.retired.set()
                    job.reply_ready.set()
                    self.condition.notify_all()

    def begin_close(self):
        with self.condition:
            self.closing, self.phase = True, 'stopping'
            if self.active is not None:
                self.active.canceled = True
                self.active.result = None
                self.active.reply_ready.set()
            self.condition.notify_all()

    def close(self, timeout=None):
        self.begin_close()
        worker = self.worker
        if worker is not None:
            worker.join(timeout)
        with self.condition:
            retired = not self.preparing and (worker is None or not worker.is_alive())
            if retired:
                self.phase = 'stopped'
            return retired  # False means GPU/preparation work may still be running.


class FinalHTTPServer(ThreadingMixIn, HTTPServer):
    daemon_threads = False
    block_on_close = True

    def __init__(self, port, service):
        self.core = service
        self.slots = threading.BoundedSemaphore(4)
        self.connection_lock = threading.Lock()
        self.connections = set()
        super().__init__(('127.0.0.1', port), FinalHandler)

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            self.close_request(request)
            return
        with self.connection_lock:
            self.connections.add(request)
        try:
            super().process_request(request, address)
        except BaseException:
            with self.connection_lock:
                self.connections.discard(request)
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            with self.connection_lock:
                self.connections.discard(request)
            self.slots.release()

    def shutdown(self):
        self.core.begin_close()
        # Wake header/body reads before ThreadingMixIn joins handlers. This is a
        # Linux service; no Windows Winsock handle reuse is involved.
        with self.connection_lock:
            connections = list(self.connections)
        for connection in connections:
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        super().shutdown()


class FinalHandler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    timeout = 3
    disable_nagle_algorithm = True

    def log_message(self, *_):
        pass

    def single(self, name):
        values = self.headers.get_all(name, [])
        if len(values) != 1:
            raise ServiceError(400, 'missing_or_duplicate_header')
        return values[0]

    def authorize(self):
        authorization = self.single('Authorization')
        if self.headers.get_all('Origin') or not authorization.isascii() or not secrets.compare_digest(authorization, 'Bearer ' + self.server.core.token):
            raise ServiceError(401, 'local_authentication_required')
        if self.single('X-ASR-Instance') != self.server.core.instance:
            raise ServiceError(409, 'service_instance_changed')

    def reply(self, status, value):
        payload = json.dumps({'protocol_version': 1, 'instance_id': self.server.core.instance,
                              **value}, ensure_ascii=False, allow_nan=False).encode('utf-8')
        if len(payload) > 65536:
            status, payload = 502, b'{"error":"response_limit"}'
        self.close_connection = True
        self.send_response(status)
        self.send_header('Content-Type', 'application/json; charset=utf-8')
        self.send_header('Content-Length', str(len(payload)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Connection', 'close')
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        try:
            self.authorize()
            if self.path != '/health':
                raise ServiceError(404, 'unknown_endpoint')
            self.reply(200, self.server.core.health())
        except ServiceError as error:
            self.reply(error.status, {'error': error.code, **({'diagnostic': error.details} if error.details else {})})

    def do_POST(self):
        pcm = None
        try:
            self.authorize()
            if self.path not in ('/v1/asr/transcribe', '/v1/asr/cancel'):
                raise ServiceError(404, 'unknown_endpoint')
            if self.headers.get_all('Transfer-Encoding') or self.headers.get_all('Expect'):
                raise ServiceError(400, 'unsupported_transfer_mode')
            raw_length = self.single('Content-Length')
            raw_generation = self.single('X-Recording-Generation')
            if not raw_length.isascii() or not raw_length.isdecimal() or len(raw_length) > 7 or not raw_generation.isascii() or not raw_generation.isdecimal() or len(raw_generation) > 20:
                raise ServiceError(400, 'invalid_integer_header')
            length, generation = int(raw_length), int(raw_generation)
            request_id = self.single('X-ASR-Request-Id')
            identity(request_id, generation)
            if self.path == '/v1/asr/cancel':
                if length != 0:
                    raise ServiceError(400, 'cancel_body_must_be_empty')
                self.reply(200, self.server.core.cancel(request_id, generation))
                return
            if not 0 < length <= MAX_PCM_BYTES or length % 2:
                raise ServiceError(413, 'pcm_body_limit')
            if (self.single('Content-Type') != 'application/octet-stream'
                    or self.single('X-Audio-Sample-Rate') != '16000'
                    or self.single('X-Audio-Channels') != '1'
                    or self.single('X-Audio-Encoding') != 'pcm_s16le'
                    or self.single('X-Audio-Sample-Count') != str(length // 2)):
                raise ServiceError(400, 'unsupported_audio_format')
            self.server.core.check_admission(request_id, generation)
            pcm, deadline = bytearray(), time.monotonic() + 3
            while len(pcm) < length:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ServiceError(408, 'body_deadline_exceeded')
                self.connection.settimeout(remaining)
                block = self.rfile.read1(min(65536, length - len(pcm)))
                if not block:
                    raise ServiceError(400, 'incomplete_pcm_body')
                pcm.extend(block)
            job = self.server.core.submit(request_id, generation, pcm)
            pcm[:] = b'\0' * len(pcm)
            pcm.clear()
            self.reply(200, self.server.core.wait(job))
        except ServiceError as error:
            self.reply(error.status, {'error': error.code, **({'diagnostic': error.details} if error.details else {})})
        except TimeoutError:
            self.reply(408, {'error': 'body_deadline_exceeded'})
        except (ConnectionError, OSError):
            self.close_connection = True
        finally:
            if pcm is not None:
                pcm[:] = b'\0' * len(pcm)
                pcm.clear()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', required=True)
    parser.add_argument('--instance', required=True)
    parser.add_argument('--port', type=int, default=0)
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9a-f]{32}', args.instance) or not 0 <= args.port <= 65535:
        parser.error('instance must be lowercase UUID hex; port must be 0..65535')
    protocol_output = sys.stdout
    def emit(state, **fields):
        protocol_output.write(json.dumps({'protocol_version': 1, 'instance_id': args.instance,
                                          'state': state, **fields}) + '\n')
        protocol_output.flush()
    emit('awaiting_owner', ready=False, warmup_completed=False)
    first_line = Queue(maxsize=1)
    def bootstrap_reader():
        try:
            first_line.put(sys.stdin.readline(4097))
        except (OSError, UnicodeError):
            first_line.put(None)
    threading.Thread(target=bootstrap_reader, name='qwen-bootstrap', daemon=True).start()
    try:
        line = first_line.get(timeout=5)
        bootstrap = json.loads(line) if isinstance(line, str) else None
        if not isinstance(bootstrap, dict) or len(line) > 4096 or not line.endswith('\n') or set(bootstrap) != {'protocol_version', 'instance_id', 'bearer_token'} or type(bootstrap['protocol_version']) is not int or bootstrap['protocol_version'] != 1 or bootstrap['instance_id'] != args.instance or not isinstance(bootstrap['bearer_token'], str):
            raise ValueError('invalid_private_bootstrap')
        core = FinalService(QwenFinalEngine(args.root), args.instance, bootstrap['bearer_token'])
        server = FinalHTTPServer(args.port, core)
    except (Empty, ValueError, TypeError, OSError):
        emit('failed', reason='bootstrap_or_local_configuration_failed', ready=False,
             warmup_completed=False, model_started=False)
        return 1
    endpoint = f'http://127.0.0.1:{server.server_port}'
    stopped = threading.Event()
    def request_stop(*_):
        core.begin_close()
        stopped.set()
    for kind in (signal.SIGINT, signal.SIGTERM):
        signal.signal(kind, request_stop)
    def controls():
        try:
            while True:
                line = sys.stdin.readline(4097)
                if not line:
                    break
                if len(line) > 4096 or not line.endswith('\n'):
                    break
                command = json.loads(line)
                if command == {'op': 'stop', 'instance_id': args.instance}:
                    break
                # A stale lifecycle stop does not stop this service.
        except (ValueError, OSError):
            pass
        request_stop()
    controller = threading.Thread(target=controls, name='qwen-final-control', daemon=True)
    serving = threading.Thread(target=lambda: server.serve_forever(poll_interval=.05), name='qwen-final-http')
    controller.start()
    serving.start()
    failed = False
    try:
        core.prepare(lambda state: emit(state['phase'], base_url=endpoint, **{k: v for k, v in state.items() if k != 'phase'}))
        stopped.wait()
    except Exception:
        failed = True
        emit('failed', reason='local_final_preparation_failed', ready=False, warmup_completed=False)
    finally:
        core.begin_close()
        server.shutdown()
        serving.join()
        server.server_close()
        if not core.close(timeout=5):
            emit('cleanup_pending', ready=False, warmup_completed=False, worker_retired=False)
            core.close(timeout=None)  # Never claim cancellation stopped live CUDA work.
        emit('stopped', ready=False, warmup_completed=False, http_closed=True, worker_retired=True,
             process_exit_still_required=True)
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
