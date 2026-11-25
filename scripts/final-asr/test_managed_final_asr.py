"""Fake owner tests: no real process, socket, /proc, GPU, service or cache write."""
import io
import json
import signal
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from managed_final_asr import (
    BUNDLE_FILES, Commands, CleanupFailed, MAX_LINE, PROVIDER, ProtocolError,
    SIGKILL, ServiceEvents, bootstrap_token, cache_path, child_command,
    command_kind, run, service_environment, stop_owned,
    validated_service_event, wait_for_gpu_initialization, GPU_ADMISSION_HELPER,
)

INSTANCE, TOKEN = 'a' * 32, 'b' * 32


def service_event(state, **overrides):
    value = {'protocol_version': 1, 'instance_id': INSTANCE, 'state': state,
             'ready': state == 'warm_ready', 'warmup_completed': state == 'warm_ready'}
    if state in ('loading', 'warming', 'warm_ready'):
        value.update(base_url='http://127.0.0.1:49152', model_ready=state != 'loading',
                     busy=False, closing=False, provider=PROVIDER, inference_mode='offline_final')
    value.update(overrides)
    return (json.dumps(value) + '\n').encode()


class FakePipe:
    def __init__(self):
        self.written, self.closed = [], False

    def write(self, value):
        self.written.append(value)

    def flush(self):
        pass

    def close(self):
        self.closed = True


class FakeProcess:
    def __init__(self):
        self.pid, self.returncode = 4321, None
        self.stdin = FakePipe()
        self.live = True
        self.waited = False

    def poll(self):
        return self.returncode

    def wait(self, timeout):
        if self.live:
            raise AssertionError('wait cannot establish live-group cleanup')
        self.waited = True
        return self.returncode


class OwnerProtocolTests(unittest.TestCase):
    def test_shared_helper_is_loaded_from_sibling_or_fixed_source_fallback(self):
        self.assertEqual(GPU_ADMISSION_HELPER.name, 'gpu_admission.py')
        self.assertIn(GPU_ADMISSION_HELPER.parent.name, ('final-asr', 'speech-lab'))

    def test_only_valid_warm_ready_releases_initialization(self):
        validator, admission = ServiceEvents(INSTANCE), Mock()
        for phase in ('awaiting_owner', 'loading', 'warming'):
            validated_service_event(validator, service_event(phase), admission)
        admission.release_initialization.assert_not_called()
        validated_service_event(validator, service_event('warm_ready'), admission)
        admission.release_initialization.assert_called_once_with()
        invalid = ServiceEvents(INSTANCE)
        admission.reset_mock()
        with self.assertRaises(ProtocolError):
            validated_service_event(invalid, service_event('warm_ready'), admission)
        admission.release_initialization.assert_not_called()

    def test_waiting_gpu_remains_cancelable_and_ignores_stale_stop(self):
        admission, commands, waiting = Mock(), Mock(), Mock()
        admission.try_initialization.return_value = False
        commands.read.side_effect = [{'op': 'stop', 'instance_id': 'c'*32},
                                     {'op': 'stop', 'instance_id': INSTANCE}]
        reason = wait_for_gpu_initialization(admission, commands, INSTANCE, threading.Event(), waiting)
        self.assertEqual(reason, 'owner_stop')
        waiting.assert_called_once_with()
        for mode in ('eof', 'signal'):
            commands, stopped = Mock(), threading.Event()
            if mode == 'eof':
                commands.read.side_effect = EOFError()
            else:
                stopped.set()
            reason = wait_for_gpu_initialization(admission, commands, INSTANCE, stopped, lambda: None)
            self.assertEqual(reason, 'owner_' + mode)

    def test_private_bootstrap_is_exact_and_boolean_version_rejected(self):
        value = {'protocol_version': 1, 'instance_id': INSTANCE, 'bearer_token': TOKEN}
        self.assertEqual(bootstrap_token(value, INSTANCE), TOKEN)
        for override in ({'protocol_version': True}, {'instance_id': 'c'*32},
                         {'bearer_token': 'too-short'}, {'extra': TOKEN}):
            with self.subTest(override=override), self.assertRaises(ProtocolError):
                bootstrap_token({**value, **override}, INSTANCE)

    def test_only_exact_stop_is_effective_and_stale_stop_is_ignored(self):
        self.assertEqual(command_kind({'op': 'stop', 'instance_id': INSTANCE}, INSTANCE), 'stop')
        self.assertEqual(command_kind({'op': 'stop', 'instance_id': 'c'*32}, INSTANCE), 'stale_stop')
        with self.assertRaises(ProtocolError):
            command_kind({'op': 'stop', 'instance_id': INSTANCE, 'pid': 4321}, INSTANCE)

    def test_owner_reader_preserves_second_line_and_bounds_unterminated_line(self):
        commands = object.__new__(Commands)
        commands.fd, commands.pending = 7, bytearray()
        commands.selector = SimpleNamespace(select=lambda _: [True])
        first = {'protocol_version': 1, 'instance_id': INSTANCE, 'bearer_token': TOKEN}
        second = {'op': 'stop', 'instance_id': INSTANCE}
        raw = (json.dumps(first) + '\n' + json.dumps(second) + '\n').encode()
        with patch('managed_final_asr.os.read', return_value=raw) as read:
            self.assertEqual(commands.read(0), first)
            self.assertEqual(commands.read(0), second)
            self.assertEqual(read.call_count, 1)
        commands.pending.clear()
        with patch('managed_final_asr.os.read', return_value=b'x' * (MAX_LINE + 1)):
            with self.assertRaises(ProtocolError):
                commands.read(0)

    def test_warmup_order_and_endpoint_are_verified_before_forwarding(self):
        validator = ServiceEvents(INSTANCE)
        for phase in validator.ORDER:
            value = validator.validate(service_event(phase))
            self.assertEqual(value['state'], phase)
        self.assertEqual(validator.stage, 4)
        invalid = ServiceEvents(INSTANCE)
        invalid.validate(service_event('awaiting_owner'))
        with self.assertRaises(ProtocolError):
            invalid.validate(service_event('warm_ready'))
        invalid = ServiceEvents(INSTANCE)
        invalid.validate(service_event('awaiting_owner'))
        invalid.validate(service_event('loading'))
        with self.assertRaises(ProtocolError):
            invalid.validate(service_event('warming', base_url='http://127.0.0.1:49153'))

    def test_child_cannot_forge_owner_pid_or_cleanup_evidence(self):
        for field in ('pid', 'process_group', 'linux_cleanup_confirmed', 'bearer_token'):
            with self.subTest(field=field), self.assertRaises(ProtocolError):
                ServiceEvents(INSTANCE).validate(service_event('awaiting_owner', **{field: TOKEN}))
        with self.assertRaises(ProtocolError):
            ServiceEvents(INSTANCE).validate(service_event('awaiting_owner', instance_id='c'*32))

    def test_startup_failure_proves_no_child_without_exposing_credential(self):
        output = io.StringIO()
        args = SimpleNamespace(root='not-absolute', instance=INSTANCE,
                               python='/venv/bin/python', cache_dir='/mnt/c/cache/ai-tutor-final')
        with patch('managed_final_asr.subprocess.Popen') as popen:
            code = run(args, source=io.StringIO(TOKEN), output=output)
        popen.assert_not_called()
        value = json.loads(output.getvalue())
        self.assertEqual(code, 1)
        self.assertEqual((value['state'], value['reason']), ('owned_stopped', 'startup_failed'))
        self.assertFalse(value['process_started'])
        self.assertTrue(value['linux_cleanup_confirmed'])
        self.assertNotIn(TOKEN, output.getvalue())


class GroupRetirementTests(unittest.TestCase):
    def retire(self, process, on_signal, live_after_sleep=None):
        now, sent = [0.0], []
        def sleeper(seconds):
            now[0] += seconds
            if live_after_sleep is not None:
                process.live = live_after_sleep
                process.returncode = 0 if not process.live else None
        def send(group, kind):
            self.assertEqual(group, process.pid)
            sent.append(kind)
            on_signal(kind)
        result = stop_owned(process, INSTANCE, members=lambda group: [group] if process.live else [],
                            send_signal=send, clock=lambda: now[0], sleep=sleeper,
                            graceful_seconds=.02, term_seconds=.02, kill_seconds=.02)
        return result, sent

    def test_graceful_exact_stop_retires_without_signals(self):
        process = FakeProcess()
        receipt, sent = self.retire(process, lambda _: None, live_after_sleep=False)
        self.assertEqual(sent, [])
        self.assertFalse(receipt['forced_group_stop'])
        self.assertTrue(receipt['linux_cleanup_confirmed'])
        self.assertTrue(process.waited)
        self.assertEqual(json.loads(process.stdin.written[0]), {'op': 'stop', 'instance_id': INSTANCE})
        self.assertTrue(process.stdin.closed)

    def test_term_then_kill_stays_on_recorded_group(self):
        process = FakeProcess()
        def signal_effect(kind):
            if kind == SIGKILL:
                process.live, process.returncode = False, -9
        receipt, sent = self.retire(process, signal_effect)
        self.assertEqual(sent, [signal.SIGTERM, SIGKILL])
        self.assertTrue(receipt['forced_group_stop'])
        self.assertEqual(receipt['service_exit_code'], -9)

    def test_persistent_group_never_claims_cleanup(self):
        process = FakeProcess()
        with self.assertRaises(CleanupFailed):
            self.retire(process, lambda _: None)
        self.assertFalse(process.waited)

    def test_exited_leader_does_not_hide_live_group_members(self):
        process = FakeProcess()
        process.returncode = 0  # Leader exited, but its recorded group is still live.
        def signal_effect(kind):
            if kind == signal.SIGTERM:
                process.live = False
        receipt, sent = self.retire(process, signal_effect)
        self.assertEqual(sent, [signal.SIGTERM])
        self.assertTrue(receipt['forced_group_stop'])
        self.assertEqual(receipt['service_exit_code'], 0)


class EnvironmentTests(unittest.TestCase):
    def test_five_core_files_shared_helper_and_space_safe_direct_argv(self):
        self.assertEqual(len(BUNDLE_FILES), 5)
        command = child_command('/mnt/c/own env/bin/python', '/mnt/c/Project Space/bin/final-asr',
                                '/mnt/c/Project Space', INSTANCE)
        self.assertEqual(command[0], '/mnt/c/own env/bin/python')
        self.assertEqual(command[2], '/mnt/c/Project Space/bin/final-asr/qwen_final_service.py')
        self.assertEqual(command[4], '/mnt/c/Project Space')
        self.assertNotIn(TOKEN, ' '.join(command))
        self.assertNotIn('bash', command)

    def test_cache_and_cuda_paths_do_not_borrow_tts_environment(self):
        inherited = {'PYTHONPATH': '/old/tts', 'LD_LIBRARY_PATH': '/old/cuda',
                     'VLLM_API_KEY': TOKEN, 'AI_TUTOR_MANAGED_TTS_TOKEN': TOKEN,
                     'AI_TUTOR_TTS_LEASE_FD': '9', 'KEEP': 'value'}
        environment = service_environment('/mnt/c/Users/RBMS/.cache/ai-tutor-final-runtime',
            '/mnt/c/asr env/lib/python3.12/site-packages', inherited)
        self.assertEqual(environment['KEEP'], 'value')
        self.assertEqual(inherited['VLLM_API_KEY'], TOKEN)
        for key in ('PYTHONPATH', 'VLLM_API_KEY', 'AI_TUTOR_MANAGED_TTS_TOKEN', 'AI_TUTOR_TTS_LEASE_FD'):
            self.assertNotIn(key, environment)
        self.assertEqual(environment['LD_LIBRARY_PATH'], '/mnt/c/asr env/lib/python3.12/site-packages/nvidia/cu13/lib:/usr/lib/wsl/lib')
        self.assertTrue(environment['CUDA_CACHE_PATH'].startswith('/mnt/c/Users/RBMS/.cache/ai-tutor-final-runtime/'))
        self.assertEqual(environment['HF_HUB_OFFLINE'], '1')
        for invalid in ('relative', '/tmp/ai-tutor-final', '/mnt/c/cache/unrelated', '/mnt/c/cache/../ai-tutor-final'):
            with self.subTest(path=invalid), self.assertRaises(ValueError):
                cache_path(invalid)


if __name__ == '__main__':
    unittest.main()
