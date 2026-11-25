"""Fake lock/port admission tests; no model, service, real flock or socket bind."""
import hashlib
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import gpu_admission as gpu


class FakeLocks:
    LOCK_SH, LOCK_EX, LOCK_NB, LOCK_UN = 1, 2, 4, 8

    def __init__(self):
        self.next_fd, self.fds, self.held, self.inodes, self.operations = 10, {}, {}, {}, []

    def open(self, path, flags, mode=0):
        path = str(path)
        if path not in self.inodes:
            if not flags & os.O_CREAT:
                raise FileNotFoundError(path)
            self.inodes[path] = len(self.inodes) + 1
        fd = self.next_fd
        self.next_fd += 1
        self.fds[fd] = path
        return fd

    def close(self, fd):
        self.operations.append(('close', fd))
        self.held.pop(fd, None)
        self.fds.pop(fd)

    def flock(self, fd, operation):
        self.operations.append(('flock', fd, operation))
        if operation == self.LOCK_UN:
            self.held.pop(fd, None)
            return
        shared = bool(operation & self.LOCK_SH)
        for other, other_shared in self.held.items():
            if other != fd and self.fds[other] == self.fds[fd] and not (shared and other_shared):
                raise BlockingIOError('fake conflicting open-file description')
        self.held[fd] = shared

    def stat(self, path):
        return SimpleNamespace(st_dev=1, st_ino=self.inodes[str(path)])

    def fstat(self, fd):
        return self.stat(self.fds[fd])


class PolicyAndProfileTests(unittest.TestCase):
    def test_pair_distribution_matches_only_ascii_case_when_both_sides_enabled(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime = root / 'runtime'
            runtime.mkdir()
            (runtime / 'gpu-resource-policy.json').write_text(json.dumps({'protocol_version': 1, 'mode': gpu.PAIR}))
            voice, asr = runtime / 'local-speech-policy.json', runtime / 'local-final-asr-policy.json'
            for left, right, valid in (('Ubuntu', 'UBUNTU', True), ('Ubuntu', 'devbox', False),
                                       ('', '', False), ('Ubuntu', None, False),
                                       ('\u00c4buntu', '\u00e4buntu', False)):
                voice.write_text(json.dumps({'mode': 'managed', 'managed': {'distribution': left}}))
                asr.write_text(json.dumps({'enabled': True, 'managed': {'distribution': right}}))
                with self.subTest(left=left, right=right):
                    if valid:
                        self.assertEqual(gpu.load_policy(root), gpu.PAIR)
                    else:
                        with self.assertRaisesRegex(ValueError, 'pair_requires_same_wsl_distribution'):
                            gpu.load_policy(root)

    def test_missing_or_disabled_peer_preserves_standalone_pair_and_exclusive(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime = root / 'runtime'
            runtime.mkdir()
            policy = runtime / 'gpu-resource-policy.json'
            policy.write_text(json.dumps({'protocol_version': 1, 'mode': gpu.PAIR}))
            voice, asr = runtime / 'local-speech-policy.json', runtime / 'local-final-asr-policy.json'
            voice.write_text(json.dumps({'mode': 'managed'}))
            self.assertEqual(gpu.load_policy(root), gpu.PAIR)
            asr.write_text(json.dumps({'enabled': False, 'managed': {'distribution': 'devbox'}}))
            self.assertEqual(gpu.load_policy(root), gpu.PAIR)
            voice.write_text(json.dumps({'mode': 'disabled'}))
            asr.write_text(json.dumps({'enabled': True}))
            self.assertEqual(gpu.load_policy(root), gpu.PAIR)
            policy.write_text(json.dumps({'protocol_version': 1, 'mode': gpu.EXCLUSIVE}))
            voice.write_text(json.dumps({'mode': 'managed', 'managed': {'distribution': 'Ubuntu'}}))
            asr.write_text(json.dumps({'enabled': True, 'managed': {'distribution': 'devbox'}}))
            self.assertEqual(gpu.load_policy(root), gpu.EXCLUSIVE)

    def test_each_present_peer_policy_is_bounded_at_4096_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime = root / 'runtime'
            runtime.mkdir()
            (runtime / 'gpu-resource-policy.json').write_text(json.dumps({'protocol_version': 1, 'mode': gpu.PAIR}))
            paths = [runtime / 'local-speech-policy.json', runtime / 'local-final-asr-policy.json']
            for oversized in paths:
                for path in paths:
                    path.write_bytes(b'{}')
                oversized.write_bytes(b'{}' + b' ' * 4094)
                self.assertEqual(gpu.load_policy(root), gpu.PAIR)
                oversized.write_bytes(b'{}' + b' ' * 4095)
                with self.subTest(path=oversized.name), self.assertRaisesRegex(ValueError, 'speech_policy_limit'):
                    gpu.load_policy(root)

    def test_absent_policy_is_exclusive_and_invalid_opt_in_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.assertEqual(gpu.load_policy(root), gpu.EXCLUSIVE)
            path = root / 'runtime/gpu-resource-policy.json'
            path.parent.mkdir()
            for mode in (gpu.EXCLUSIVE, gpu.PAIR):
                path.write_text(json.dumps({'protocol_version': 1, 'mode': mode}))
                self.assertEqual(gpu.load_policy(root), mode)
            for text in ('{"protocol_version":true,"mode":"exclusive"}',
                         '{"protocol_version":1,"mode":"anything"}',
                         '{"protocol_version":1,"mode":"exclusive","extra":1}',
                         '{"protocol_version":1,"mode":"exclusive","mode":"qwen_pair_12gb_v1"}'):
                path.write_text(text)
                with self.subTest(text=text), self.assertRaises(ValueError):
                    gpu.load_policy(root)

    def test_profile_and_model_identity_are_both_required(self):
        profile, config = b'fixed-256MiB-graph', b'fixed-06b-customvoice'
        with mock.patch.object(gpu, 'PROFILE_SHA256', hashlib.sha256(profile).hexdigest()), \
                mock.patch.object(gpu, 'TTS_CONFIG_SHA256', hashlib.sha256(config).hexdigest()):
            with mock.patch.object(Path, 'read_bytes', side_effect=[profile, config]):
                gpu.validate_qwen_profile('/project', '/bundle/qwen3_tts_12gb.yaml')
            with mock.patch.object(Path, 'read_bytes', return_value=b'different-kv-budget'):
                with self.assertRaises(ValueError):
                    gpu.validate_qwen_profile('/project', '/bundle/qwen3_tts_12gb.yaml')
            with mock.patch.object(Path, 'read_bytes', side_effect=[profile, b'other-model']):
                with self.assertRaises(ValueError):
                    gpu.validate_qwen_profile('/project', '/bundle/qwen3_tts_12gb.yaml')


class LeaseTests(unittest.TestCase):
    def setUp(self):
        self.locks = FakeLocks()
        self.paths = gpu.lock_paths(uid=1000)
        patches = [mock.patch.object(gpu, '_fcntl', return_value=self.locks),
                   mock.patch.object(gpu, 'lock_paths', return_value=self.paths),
                   mock.patch.object(gpu.os, 'open', side_effect=self.locks.open),
                   mock.patch.object(gpu.os, 'close', side_effect=self.locks.close),
                   mock.patch.object(gpu.os, 'stat', side_effect=self.locks.stat),
                   mock.patch.object(gpu.os, 'fstat', side_effect=self.locks.fstat),
                   mock.patch.object(gpu, 'load_policy', return_value=gpu.PAIR),
                   mock.patch.object(gpu, 'validate_qwen_profile'),
                   mock.patch.object(gpu, 'port_idle', return_value=True)]
        for item in patches:
            item.start()
            self.addCleanup(item.stop)

    def pair(self, role):
        admission = gpu.Admission.pair('/project', role, profile='/bundle/qwen3_tts_12gb.yaml')
        self.addCleanup(admission.close)
        return admission

    def test_pair_shared_global_admits_other_role_but_blocks_duplicate_and_exclusive(self):
        tts = self.pair(gpu.TTS_ROLE)
        asr = self.pair(gpu.ASR_ROLE)
        self.assertNotEqual(tts.global_fd, asr.global_fd)
        with self.assertRaises(BlockingIOError):
            self.pair(gpu.TTS_ROLE)
        with self.assertRaises(BlockingIOError):
            gpu.open_lock(self.paths['global'])

    def test_init_lock_serializes_and_explicit_unlock_precedes_parent_close(self):
        tts, asr = self.pair(gpu.TTS_ROLE), self.pair(gpu.ASR_ROLE)
        self.assertTrue(tts.try_initialization())
        with self.assertRaises(RuntimeError):
            _ = asr.pass_fds
        self.assertFalse(asr.try_initialization())
        init_fd = tts.init_fd
        self.assertEqual(len(tts.pass_fds), 3)
        self.assertTrue(tts.release_initialization())
        self.assertEqual(self.locks.operations[-2:], [('flock', init_fd, self.locks.LOCK_UN), ('close', init_fd)])
        self.assertTrue(asr.try_initialization())
        self.assertFalse(tts.release_initialization())
        self.assertIn(tts.global_fd, self.locks.held)
        self.assertIn(tts.role_fd, self.locks.held)

    def test_exclusive_prepared_is_noop_and_passes_one_fd(self):
        fd = gpu.open_lock(self.paths['global'])
        exclusive = gpu.Admission.exclusive(fd, gpu.TTS_ROLE)
        self.addCleanup(exclusive.close)
        self.assertTrue(exclusive.try_initialization())
        self.assertFalse(exclusive.release_initialization())
        self.assertEqual(exclusive.pass_fds, (fd,))
        self.assertEqual(exclusive.environment()[gpu.MODE_ENV], gpu.EXCLUSIVE)

    def test_inherited_shell_relocks_global_shared_never_exclusive(self):
        admission = self.pair(gpu.TTS_ROLE)
        admission.try_initialization()
        environment = admission.environment()
        environment['AI_TUTOR_TTS_LEASE_FD'] = str(admission.global_fd)
        self.locks.operations.clear()
        gpu.validate_inherited_pair('/project', '/bundle/qwen3_tts_12gb.yaml', 'start_qwen_omni.sh', environment)
        global_ops = [item for item in self.locks.operations if item[:2] == ('flock', admission.global_fd)]
        self.assertEqual(global_ops, [('flock', admission.global_fd, self.locks.LOCK_SH | self.locks.LOCK_NB)])
        wrong = dict(environment)
        wrong[gpu.INIT_ENV] = str(admission.role_fd)
        with self.assertRaises(ValueError):
            gpu.validate_inherited_pair('/project', '/bundle/qwen3_tts_12gb.yaml', 'start_qwen_omni.sh', wrong)
        with self.assertRaises(ValueError):
            gpu.validate_inherited_pair('/project', '/bundle/qwen3_tts_12gb.yaml', 'start_cosy_omni.sh', environment)

    def test_asr_accepts_18868_only_with_held_tts_role_and_always_rejects_18869(self):
        with mock.patch.object(gpu, 'port_idle', side_effect=lambda port: port != 18868):
            with self.assertRaises(RuntimeError):
                gpu.check_pair_ports(gpu.ASR_ROLE, paths=self.paths)
            tts = self.pair(gpu.TTS_ROLE)
            gpu.check_pair_ports(gpu.ASR_ROLE, paths=self.paths)
            tts.close()
            with self.assertRaises(RuntimeError):
                gpu.check_pair_ports(gpu.ASR_ROLE, paths=self.paths)
        with mock.patch.object(gpu, 'port_idle', side_effect=lambda port: port != 18869):
            with self.assertRaises(RuntimeError):
                gpu.check_pair_ports(gpu.ASR_ROLE, paths=self.paths)


class InitializationWaitTests(unittest.TestCase):
    def test_wait_emits_once_and_keeps_control_responsive(self):
        admission = mock.Mock()
        admission.try_initialization.side_effect = [False, False, True]
        waiting, poll = mock.Mock(), mock.Mock(return_value=None)
        self.assertIsNone(gpu.wait_initialization(admission, poll, waiting, clock=lambda: 0))
        waiting.assert_called_once_with()
        self.assertEqual(poll.call_count, 2)

    def test_stop_eof_signal_return_without_child_admission(self):
        for reason in ('owner_stop', 'owner_eof', 'owner_signal'):
            with self.subTest(reason=reason):
                admission = mock.Mock()
                admission.try_initialization.return_value = False
                self.assertEqual(gpu.wait_initialization(admission, lambda _: reason, lambda: None), reason)
                self.assertEqual(admission.try_initialization.call_count, 1)

    def test_wait_deadline_is_finite(self):
        admission = mock.Mock()
        admission.try_initialization.return_value = False
        with self.assertRaises(TimeoutError):
            gpu.wait_initialization(admission, lambda _: None, lambda: None, timeout=0, clock=lambda: 0)


if __name__ == '__main__':
    unittest.main()
