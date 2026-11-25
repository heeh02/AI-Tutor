"""Fake-only tests. No sockets, Torch, model, audio device, or subprocess.

Bundle copy of the root-tested frozen proposal; root runs bundle verification.
"""
from email.message import Message
import io
from types import SimpleNamespace
import threading
import unittest
from unittest.mock import Mock, patch

from qwen_final_engine import FinalResult, InferenceCancelled, MAX_PCM_BYTES
from qwen_final_service import FinalHandler, FinalHTTPServer, FinalService, ServiceError

INSTANCE, TOKEN = 'a' * 32, 'b' * 32
ONE = '00000000-0000-4000-8000-000000000001'
TWO = '00000000-0000-4000-8000-000000000002'


def result(text='recognized', limit=False, eos=True):
    return FinalResult(text, 'English', ({'new_tokens': 512 if limit else 2,
        'hit_token_limit': limit, 'ended_with_eos': eos,
        'raw_decoder_text': 'raw decoder output'},))


class FakeEngine:
    provider = 'fake offline engine'

    def __init__(self):
        self.loaded = False
        self.warm_entered = threading.Event()
        self.warm_release = threading.Event()
        self.warm_release.set()
        self.started = threading.Event()
        self.release = threading.Event()
        self.calls = 0
        self.fail = False
        self.output = result()
        self.warm_output = result()

    def load(self):
        self.loaded = True

    def warm(self, *, cancelled=None):
        self.warm_entered.set()
        if not self.warm_release.wait(3):
            raise RuntimeError('fake warm barrier timed out')
        return self.warm_output

    def transcribe(self, pcm, *, cancelled=None):
        self.calls += 1
        self.started.set()
        if not self.release.wait(3):
            raise RuntimeError('fake inference barrier timed out')
        if self.fail:
            raise RuntimeError('secret native error must not enter response')
        return self.output


class CoreTests(unittest.TestCase):
    def setUp(self):
        self.engine = FakeEngine()
        self.core = FinalService(self.engine, INSTANCE, TOKEN)

    def tearDown(self):
        self.engine.warm_release.set()
        self.engine.release.set()
        self.assertTrue(self.core.close(timeout=3))

    def assert_code(self, status, code, action):
        with self.assertRaises(ServiceError) as caught:
            action()
        self.assertEqual((caught.exception.status, caught.exception.code), (status, code))

    def running(self):
        self.core.prepare()
        job = self.core.submit(ONE, 1, b'\x01\x00\x02\x00')
        self.assertTrue(self.engine.started.wait(1))
        return job

    def test_no_admission_until_real_warm_completed(self):
        self.assert_code(503, 'model_not_warm_or_closing', lambda: self.core.submit(ONE, 1, b'\0\0'))
        self.engine.warm_release.clear()
        preparing = threading.Thread(target=self.core.prepare)
        preparing.start()
        self.assertTrue(self.engine.warm_entered.wait(1))
        self.assertTrue(self.core.health()['model_ready'])
        self.assertFalse(self.core.health()['warmup_completed'])
        self.assert_code(503, 'model_not_warm_or_closing', lambda: self.core.submit(ONE, 1, b'\0\0'))
        self.engine.warm_release.set()
        preparing.join(1)
        self.assertFalse(preparing.is_alive())
        self.assertTrue(self.core.health()['warmup_completed'])

    def test_cancel_invalidates_reply_but_reserves_gpu_until_retirement(self):
        job = self.running()
        receipt = self.core.cancel(ONE, 1)
        self.assertTrue(receipt['canceled'])
        self.assertFalse(receipt['worker_retired'])
        self.assertTrue(self.core.health()['busy'])
        self.assert_code(409, 'request_canceled', lambda: self.core.wait(job, 0))
        self.assert_code(409, 'gpu_worker_busy', lambda: self.core.submit(TWO, 2, b'\0\0'))
        self.assertEqual(self.engine.calls, 1)
        self.engine.release.set()
        self.assertTrue(job.retired.wait(1))
        self.assertEqual(job.pcm, bytearray())
        self.assertIsNone(job.result)
        self.assert_code(409, 'request_canceled', lambda: self.core.wait(job, 0))
        self.engine.started.clear()
        self.engine.release.clear()
        second = self.core.submit(TWO, 2, b'\0\0')
        self.assertTrue(self.engine.started.wait(1))
        stale = self.core.cancel(ONE, 1)
        self.assertFalse(stale['canceled'])
        self.assertIsNone(stale['worker_retired'])
        self.assertFalse(second.canceled)
        self.engine.release.set()
        self.assertEqual(self.core.wait(second, 1)['text'], 'recognized')

    def test_cancel_callable_is_latched_to_old_job_and_new_job_recovers(self):
        callbacks = []
        enter_decode = threading.Event()
        def cancellable(pcm, *, cancelled=None):
            callbacks.append(cancelled)
            self.engine.started.set()
            if not enter_decode.wait(1):
                raise RuntimeError('fake decode gate timed out')
            if cancelled():
                raise InferenceCancelled(({'new_tokens': 2},))
            return result()
        self.engine.transcribe = cancellable
        job = self.running()
        self.assertFalse(callbacks[0]())
        self.core.cancel(ONE, 1)
        self.assertTrue(callbacks[0]())
        self.assertTrue(self.core.health()['busy'])
        enter_decode.set()
        self.assertTrue(job.retired.wait(1))
        self.assertEqual(self.core.health()['phase'], 'warm_ready')
        self.assertIsNone(job.result)
        second = self.core.submit(TWO, 2, b'\0\0')
        self.assertEqual(self.core.wait(second, 1)['text'], 'recognized')
        self.assertTrue(callbacks[0]())
        self.assertFalse(callbacks[1]())

    def test_closing_warm_cancellation_is_not_a_model_failure(self):
        def canceled_warm(*, cancelled=None):
            self.assertFalse(cancelled())
            self.core.begin_close()
            self.assertTrue(cancelled())
            raise InferenceCancelled()
        self.engine.warm = canceled_warm
        self.core.prepare()
        self.assertEqual(self.core.health()['phase'], 'stopping')
        self.assertFalse(self.core.health()['warmup_completed'])
        self.assertIsNone(self.core.worker)

    def test_timeout_keeps_running_work_busy_and_discards_late_result(self):
        job = self.running()
        self.assert_code(504, 'final_deadline_exceeded', lambda: self.core.wait(job, 0))
        self.assertTrue(self.core.health()['busy'])
        self.engine.release.set()
        self.assertTrue(job.retired.wait(1))
        self.assertIsNone(job.result)

    def test_stop_does_not_claim_worker_exited_until_barrier_released(self):
        job = self.running()
        self.assertFalse(self.core.close(timeout=0))
        self.assert_code(503, 'model_not_warm_or_closing', lambda: self.core.submit(TWO, 2, b'\0\0'))
        self.engine.release.set()
        self.assertTrue(job.retired.wait(1))
        self.assertTrue(self.core.close(timeout=1))
        self.assertIsNone(job.result)

    def test_stop_during_warm_cannot_publish_late_warm_ready(self):
        self.engine.warm_release.clear()
        preparing = threading.Thread(target=self.core.prepare)
        preparing.start()
        self.assertTrue(self.engine.warm_entered.wait(1))
        self.assertFalse(self.core.close(timeout=0))
        self.engine.warm_release.set()
        preparing.join(1)
        self.assertFalse(preparing.is_alive())
        self.assertFalse(self.core.health()['warmup_completed'])
        self.assertIsNone(self.core.worker)

    def test_empty_final_preserved_and_generation_replay_rejected(self):
        self.engine.output = result(text='')
        job = self.running()
        self.engine.release.set()
        answer = self.core.wait(job, 1)
        self.assertTrue(answer['final'])
        self.assertEqual(answer['text'], '')
        self.assert_code(409, 'stale_or_replayed_generation', lambda: self.core.submit(TWO, 1, b'\0\0'))

    def test_failure_is_finite_and_prevents_new_gpu_admission(self):
        self.engine.fail = True
        job = self.running()
        self.engine.release.set()
        self.assert_code(502, 'local_gpu_inference_failed', lambda: self.core.wait(job, 1))
        self.assertEqual(self.core.health()['phase'], 'failed')
        self.assert_code(503, 'model_not_warm_or_closing', lambda: self.core.submit(TWO, 2, b'\0\0'))

    def test_raw_limit_and_eos_information_remain_distinct(self):
        capped = result(limit=True, eos=False).payload()
        self.assertTrue(capped['possible_truncation'])
        self.assertEqual(capped['generation_calls'][0]['raw_decoder_text'], 'raw decoder output')
        ended = result(limit=True, eos=True).payload()
        self.assertTrue(ended['generation_calls'][0]['hit_token_limit'])
        self.assertFalse(ended['possible_truncation'])

    def test_decoder_truncation_is_rejected_with_raw_metadata(self):
        self.engine.output = result(limit=True, eos=False)
        job = self.running()
        self.engine.release.set()
        with self.assertRaises(ServiceError) as caught:
            self.core.wait(job, 1)
        self.assertEqual((caught.exception.status, caught.exception.code), (502, 'decoder_truncated'))
        self.assertTrue(caught.exception.details['possible_truncation'])
        self.assertEqual(caught.exception.details['generation_calls'][0]['new_tokens'], 512)
        self.assertIsNone(job.result)

    def test_truncated_warmup_never_becomes_ready(self):
        self.engine.warm_output = result(limit=True, eos=False)
        self.assert_code(502, 'warm_decoder_truncated', self.core.prepare)
        self.assertFalse(self.core.health()['warmup_completed'])
        self.assertIsNone(self.core.worker)


class HTTPBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.engine = FakeEngine()
        self.core = FinalService(self.engine, INSTANCE, TOKEN)
        self.core.prepare()

    def tearDown(self):
        self.engine.release.set()
        self.assertTrue(self.core.close(timeout=3))

    def handler(self, body=b'\0\0', changes=None, extra=()):
        headers = {
            'Authorization': 'Bearer ' + TOKEN, 'X-ASR-Instance': INSTANCE,
            'X-ASR-Request-Id': ONE, 'X-Recording-Generation': '1',
            'Content-Length': str(len(body)), 'Content-Type': 'application/octet-stream',
            'X-Audio-Sample-Rate': '16000', 'X-Audio-Channels': '1',
            'X-Audio-Encoding': 'pcm_s16le', 'X-Audio-Sample-Count': str(len(body)//2),
        }
        headers.update(changes or {})
        handler = object.__new__(FinalHandler)
        handler.server = SimpleNamespace(core=self.core)
        handler.path = '/v1/asr/transcribe'
        handler.headers = Message()
        for name, value in list(headers.items()) + list(extra):
            handler.headers[name] = value
        handler.rfile = io.BytesIO(body)
        handler.connection = SimpleNamespace(settimeout=lambda _: None)
        handler.reply = lambda status, value: setattr(handler, 'answer', (status, value))
        return handler

    def test_http_rejections_happen_before_body_read_or_gpu_work(self):
        cases = [({'Authorization': 'Bearer wrong'}, (), 401),
                 ({'X-ASR-Instance': 'c'*32}, (), 409),
                 ({'Content-Length': str(MAX_PCM_BYTES+2)}, (), 413),
                 ({'X-Audio-Sample-Count': '2'}, (), 400),
                 ({}, (('Content-Length', '2'),), 400),
                 ({}, (('Transfer-Encoding', 'chunked'),), 400),
                 ({}, (('Origin', ''),), 401)]
        for changes, extra, status in cases:
            with self.subTest(changes=changes, extra=extra):
                handler = self.handler(changes=changes, extra=extra)
                handler.do_POST()
                self.assertEqual(handler.answer[0], status)
                self.assertEqual(handler.rfile.tell(), 0)
                self.assertEqual(self.engine.calls, 0)

    def test_absolute_body_deadline_and_incomplete_body(self):
        handler = self.handler()
        with patch('qwen_final_service.time.monotonic', side_effect=[0, 4]):
            handler.do_POST()
        self.assertEqual(handler.answer[0], 408)
        handler = self.handler(changes={'Content-Length': '4', 'X-Audio-Sample-Count': '2'})
        handler.do_POST()
        self.assertEqual(handler.answer, (400, {'error': 'incomplete_pcm_body'}))
        self.assertEqual(self.engine.calls, 0)

    def test_one_http_request_returns_one_tagged_offline_final(self):
        self.engine.release.set()
        handler = self.handler()
        handler.do_POST()
        self.assertEqual(handler.answer[0], 200)
        self.assertEqual(handler.answer[1]['request_id'], ONE)
        self.assertEqual(handler.answer[1]['recording_generation'], 1)
        self.assertTrue(handler.answer[1]['final'])
        self.assertEqual(self.engine.calls, 1)

    def test_http_never_publishes_truncated_text_as_final(self):
        self.engine.output = result(limit=True, eos=False)
        self.engine.release.set()
        handler = self.handler()
        handler.do_POST()
        self.assertEqual(handler.answer[0], 502)
        self.assertEqual(handler.answer[1]['error'], 'decoder_truncated')
        self.assertNotIn('final', handler.answer[1])
        self.assertNotIn('text', handler.answer[1])

    def test_stop_wakes_exact_sockets_without_cross_thread_close(self):
        server = object.__new__(FinalHTTPServer)
        connection = Mock()
        server.core = self.core
        server.connection_lock = threading.Lock()
        server.connections = {connection}
        with patch('qwen_final_service.HTTPServer.shutdown') as shutdown:
            server.shutdown()
        connection.shutdown.assert_called_once()
        connection.close.assert_not_called()
        shutdown.assert_called_once()
        self.assertTrue(self.core.health()['closing'])


if __name__ == '__main__':
    unittest.main()
