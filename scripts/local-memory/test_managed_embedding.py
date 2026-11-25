"""Stdlib-only protocol/lifecycle tests; no worker, WSL, model or GPU is started."""
import argparse
import copy
from pathlib import Path
import signal
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import managed_embedding as managed
import embedding_worker as wire

INSTANCE = 'a' * 32
HASH = 'b' * 64


def loading():
    return managed.message(INSTANCE, 'loading', profile=wire.PROFILE, ready=False, warmup_completed=False)


def ready():
    return managed.message(INSTANCE, 'ready', profile=wire.PROFILE, ready=True, warmup_completed=True,
        device='cpu', dtype='float32', intra_threads=2, inter_threads=1, cuda_visible_devices='',
        load_count=1, warmup_tokens=4,
        timings={'hash_audit_ms': 1, 'model_load_ms': 2, 'warmup_ms': 3, 'total_ms': 6})


def command():
    return {'request_id': 'request-1', 'role': 'query', 'input_sha256': HASH}


def encoded():
    return managed.message(INSTANCE, 'encoded', **command(), text_sha256=HASH, profile=wire.PROFILE,
        tokens=4, vector=[1.] + [0.] * 1023,
        timings={'tokenize_ms': 1, 'inference_ms': 2, 'total_ms': 3})


def active_events():
    events = managed.WorkerEvents(INSTANCE)
    events.receive(loading())
    events.receive(ready())
    events.reserve(command())
    return events


class FakeClock:
    def __init__(self):
        self.now = 0

    def __call__(self):
        return self.now

    def sleep(self, value):
        self.now += value


class FakeProcess:
    pid = 4142

    def __init__(self, exit_code=0):
        self.returncode = None
        self.exit_code = exit_code
        self.waited = 0
        self.stdin = Mock()
        self.stdout = Mock()
        self.stdin.fileno.return_value = 103
        self.stdout.fileno.return_value = 104

    def poll(self):
        return self.returncode

    def wait(self, timeout):
        self.waited += 1
        self.returncode = self.exit_code
        return self.returncode


class SupervisorTests(unittest.TestCase):
    def test_parent_death_guard_resets_inherited_handler_before_arming_and_checks_parent(self):
        calls = []
        libc = Mock()
        libc.prctl.side_effect = lambda *_: calls.append('arm') or 0
        with patch.object(managed.signal, 'signal', side_effect=lambda kind, handler: calls.append('reset')) as reset, \
                patch.object(managed.ctypes, 'CDLL', return_value=libc), \
                patch.object(managed.os, 'getppid', return_value=88), patch.object(managed.os, '_exit') as exit_process:
            managed.parent_death_guard(88)
            self.assertEqual(calls, ['reset', 'arm'])
            reset.assert_called_once_with(signal.SIGTERM, signal.SIG_DFL)
            exit_process.assert_not_called()
            managed.parent_death_guard(77)
            exit_process.assert_called_once_with(125)

    def test_frame_boundaries_reject_oversize_and_partial_eof(self):
        lines = managed.Lines()
        self.assertEqual(lines.feed(b'{"a":'), [])
        self.assertEqual(lines.feed(b'1}\n{}\n'), [b'{"a":1}\n', b'{}\n'])
        lines.eof()
        self.assertEqual(managed.Lines().feed(b'x' * (wire.MAX_LINE - 1) + b'\n'), [b'x' * (wire.MAX_LINE - 1) + b'\n'])
        with self.assertRaises(managed.SupervisorError):
            managed.Lines().feed(b'x' * wire.MAX_LINE)
        lines.feed(b'{')
        with self.assertRaises(managed.SupervisorError):
            lines.eof()

    def test_startup_receipts_require_order_fixed_profile_and_cpu_warmup(self):
        for mutation in ('early', 'profile', 'gpu', 'dtype', 'cold', 'negative_time', 'text'):
            with self.subTest(mutation=mutation):
                events = managed.WorkerEvents(INSTANCE)
                if mutation != 'early':
                    events.receive(loading())
                event = copy.deepcopy(ready())
                if mutation == 'profile': event['profile']['pooling'] = 'mean'
                elif mutation == 'gpu': event['device'] = 'cuda'
                elif mutation == 'dtype': event['dtype'] = 'float16'
                elif mutation == 'cold': event['warmup_completed'] = False
                elif mutation == 'negative_time': event['timings']['model_load_ms'] = -1
                elif mutation == 'text': event['text'] = 'never forward private text'
                with self.assertRaises(managed.SupervisorError):
                    events.receive(event)

    def test_encoded_identity_and_vector_must_match_the_one_pending_request(self):
        for mutation in ('request', 'role', 'hash', 'dimension', 'nan', 'norm', 'extra', 'tokens'):
            with self.subTest(mutation=mutation):
                events = active_events()
                event = copy.deepcopy(encoded())
                if mutation == 'request': event['request_id'] = 'other'
                elif mutation == 'role': event['role'] = 'document'
                elif mutation == 'hash': event['text_sha256'] = 'c' * 64
                elif mutation == 'dimension': event['vector'].pop()
                elif mutation == 'nan': event['vector'][0] = float('nan')
                elif mutation == 'norm': event['vector'][0] = 2.
                elif mutation == 'extra': event['source_text'] = 'private'
                elif mutation == 'tokens': event['tokens'] = 513
                with self.assertRaises(managed.SupervisorError):
                    events.receive(event)
        events = active_events()
        events.receive(encoded())
        self.assertIsNone(events.pending)
        with self.assertRaises(managed.SupervisorError):
            events.receive(encoded())

    def test_overlong_input_is_a_bound_error_without_a_vector_or_success(self):
        events = active_events()
        error = managed.message(INSTANCE, 'encode_error', **command(), text_sha256=HASH,
                                profile=wire.PROFILE, code='input_too_long', tokens=513)
        self.assertEqual(events.receive(error)['code'], 'input_too_long')
        self.assertIsNone(events.pending)
        for change in ('short', 'vector'):
            events = active_events()
            invalid = dict(error)
            if change == 'short': invalid['tokens'] = 512
            else: invalid['vector'] = [0]
            with self.assertRaises(managed.SupervisorError):
                events.receive(invalid)

    def test_normal_cleanup_waits_worker_without_sending_any_signal(self):
        clock = FakeClock()
        process = FakeProcess()
        request, send = Mock(), Mock()
        result = managed.stop_owned(process, INSTANCE, request_stop=request,
            members=lambda group: [] if clock.now >= .04 else [group], send_signal=send,
            clock=clock, sleep=clock.sleep, graceful_seconds=.1)
        request.assert_called_once_with(process, INSTANCE, b'')
        send.assert_not_called()
        self.assertEqual(process.waited, 1)
        self.assertEqual(result['worker_exit_code'], 0)
        self.assertTrue(result['linux_cleanup_confirmed'] and result['group_reaped'])
        self.assertFalse(result['forced_group_stop'])

    def test_lingering_owned_group_is_stopped_even_after_its_leader_exits(self):
        for release_on in (signal.SIGTERM, managed.SIGKILL):
            with self.subTest(release_on=release_on):
                clock = FakeClock()
                process = FakeProcess()
                process.returncode = 0
                live = [process.pid + 1]
                signals = []

                def send(group, kind):
                    self.assertEqual(group, process.pid)
                    signals.append(kind)
                    if kind == release_on:
                        live.clear()

                result = managed.stop_owned(process, INSTANCE, request_stop=Mock(), members=lambda _: list(live),
                    send_signal=send, clock=clock, sleep=clock.sleep,
                    graceful_seconds=.02, term_seconds=.02, kill_seconds=.02)
                self.assertEqual(signals, [signal.SIGTERM] if release_on == signal.SIGTERM else [signal.SIGTERM, managed.SIGKILL])
                self.assertTrue(result['forced_group_stop'] and result['linux_cleanup_confirmed'])
                self.assertEqual(process.waited, 1)

    def test_cleanup_failure_cannot_claim_reaped_or_complete(self):
        clock = FakeClock()
        process = FakeProcess()
        with self.assertRaises(managed.CleanupFailed):
            managed.stop_owned(process, INSTANCE, request_stop=Mock(), members=lambda group: [group],
                send_signal=Mock(), clock=clock, sleep=clock.sleep,
                graceful_seconds=.02, term_seconds=.02, kill_seconds=.02)
        self.assertEqual(process.waited, 0)

    def test_proc_group_check_excludes_other_groups_and_refuses_session_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for pid, state, group, session in [(12, 'S', 12, 12), (13, 'Z', 12, 12), (20, 'S', 20, 20)]:
                path = root / str(pid)
                path.mkdir()
                (path / 'stat').write_text(f'{pid} (编码 worker with spaces) {state} 1 {group} {session} 0\n', encoding='utf-8')
            self.assertEqual(managed.live_group_members(12, root), [12])
            (root / '12/stat').write_text('12 (worker) S 1 12 99 0\n', encoding='ascii')
            with self.assertRaises(managed.CleanupFailed):
                managed.live_group_members(12, root)

    def run_fake(self, frames, process=None):
        source = Mock()
        source.fileno.return_value = 100
        sink = Mock()
        sink.fileno.return_value = 101
        output = Mock()
        output.fd = 101
        output.pending = bytearray()
        selector = Mock()
        selector.select.side_effect = [[(SimpleNamespace(fd=100, data='owner'), 1)] for _ in frames]
        owner_frames = iter(frames)
        receipt = {'worker_started': process is not None, 'worker_exit_code': 0 if process else None,
            'forced_group_stop': False, 'linux_cleanup_confirmed': True, 'group_reaped': True}
        args = argparse.Namespace(instance=INSTANCE, model_dir='/fixture-model')
        with patch.object(managed.sys, 'platform', 'linux'), patch.object(managed.sys, 'stdin', source), \
                patch.object(managed.sys, 'stdout', sink), patch.object(managed, 'Output', return_value=output), \
                patch.object(managed.selectors, 'DefaultSelector', return_value=selector), \
                patch.object(managed, 'absolute_model_dir', return_value=Path('/fixture-model')), \
                patch.object(managed.Path, 'is_file', return_value=True), \
                patch.object(managed.os, 'set_blocking', create=True), patch.object(managed.os, 'read', side_effect=lambda fd, _n: next(owner_frames) if fd == 100 else b''), \
                patch.object(managed.os, 'getpgid', return_value=4142, create=True), patch.object(managed.os, 'getsid', return_value=4142, create=True), \
                patch.object(managed.signal, 'signal'), patch.object(managed.subprocess, 'Popen', return_value=process) as spawn, \
                patch.object(managed, 'stop_owned', return_value=receipt) as stop:
            result = managed.run(args)
        return result, [call.args[0] for call in output.queue.call_args_list], spawn, stop

    def test_eof_or_stop_before_start_never_spawns_or_loads_a_worker(self):
        for frame, reason in [(b'', 'owner_eof'), (wire.wire_bytes({'protocol_version':1,'instance_id':INSTANCE,'op':'stop'}), 'owner_stop')]:
            result, events, spawn, stop = self.run_fake([frame])
            self.assertEqual(result, 0)
            spawn.assert_not_called()
            stop.assert_not_called()
            self.assertEqual([event['state'] for event in events], ['awaiting_owner', 'stopped'])
            self.assertEqual(events[-1]['reason'], reason)
            self.assertIsNone(events[-1]['worker_exit_code'])
            self.assertTrue(events[-1]['group_reaped'] and events[-1]['linux_cleanup_confirmed'])

    def test_owner_eof_after_start_invokes_cleanup_for_only_the_created_group(self):
        process = FakeProcess()
        start = wire.wire_bytes({'protocol_version':1,'instance_id':INSTANCE,'op':'start','profile':wire.PROFILE})
        result, events, spawn, stop = self.run_fake([start, b''], process)
        self.assertEqual(result, 0)
        self.assertTrue(spawn.call_args.kwargs['start_new_session'])
        self.assertEqual(spawn.call_args.kwargs['stderr'], managed.subprocess.DEVNULL)
        self.assertEqual(spawn.call_args.kwargs['env']['CUDA_VISIBLE_DEVICES'], '')
        self.assertEqual(stop.call_args.args[0], process)
        self.assertEqual(events[-1]['state'], 'stopped')
        self.assertTrue(events[-1]['worker_started'])


if __name__ == '__main__':
    unittest.main()
