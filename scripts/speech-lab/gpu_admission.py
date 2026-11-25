"""Opt-in admission for exactly one Qwen TTS + one Qwen ASR on GPU 0.

Pure stdlib, with fcntl imported only when a Linux lock is used. Default owners
and raw launchers retain their historical exclusive GPU lease behavior.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import sys
import time

EXCLUSIVE = 'exclusive'
PAIR = 'qwen_pair_12gb_v1'
TTS_ROLE = 'qwen_tts'
ASR_ROLE = 'qwen_asr'
PROFILE_SHA256 = '3efa38e607d4d53d7eb7e44556c03373b6f069f1407c0957f24f6fc668820f3e'
TTS_CONFIG_SHA256 = '81aca2b6fac304944d8acf345272d8a9a727d5fc2e2e66b222ab4729340c7455'
MODE_ENV = 'AI_TUTOR_GPU_ADMISSION_MODE'
ROLE_ENV = 'AI_TUTOR_GPU_ROLE'
GLOBAL_ENV = 'AI_TUTOR_GPU_GLOBAL_FD'
ROLE_FD_ENV = 'AI_TUTOR_GPU_ROLE_FD'
INIT_ENV = 'AI_TUTOR_GPU_INIT_FD'
ENV_NAMES = (MODE_ENV, ROLE_ENV, GLOBAL_ENV, ROLE_FD_ENV, INIT_ENV)


def _fcntl():
    import fcntl
    return fcntl


def _strict_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError('duplicate_gpu_policy_key')
        value[key] = item
    return value


def _read_peer_policy(path):
    if not path.exists():
        return None
    with path.open('rb') as source:
        raw = source.read(4097)
    if len(raw) > 4096:
        raise ValueError('speech_policy_limit')
    return json.loads(raw)


def _validate_pair_distribution(root):
    runtime = Path(root) / 'runtime'
    voice = _read_peer_policy(runtime / 'local-speech-policy.json')
    asr = _read_peer_policy(runtime / 'local-final-asr-policy.json')
    if (not isinstance(voice, dict) or not isinstance(asr, dict)
            or voice.get('mode') != 'managed' or asr.get('enabled') is not True):
        return
    left_config, right_config = voice.get('managed'), asr.get('managed')
    left = left_config.get('distribution') if isinstance(left_config, dict) else None
    right = right_config.get('distribution') if isinstance(right_config, dict) else None
    # Match Rust eq_ignore_ascii_case, not Unicode lower()/casefold().
    ascii_lower = str.maketrans('ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz')
    if (not isinstance(left, str) or not isinstance(right, str) or not left
            or left.translate(ascii_lower) != right.translate(ascii_lower)):
        raise ValueError('pair_requires_same_wsl_distribution')


def load_policy(root):
    path = Path(root) / 'runtime/gpu-resource-policy.json'
    if not path.exists():
        return EXCLUSIVE
    raw = path.read_bytes()
    if len(raw) > 4096:
        raise ValueError('gpu_policy_limit')
    value = json.loads(raw, object_pairs_hook=_strict_object)
    if (not isinstance(value, dict) or set(value) != {'protocol_version', 'mode'}
            or type(value['protocol_version']) is not int or value['protocol_version'] != 1
            or value['mode'] not in (EXCLUSIVE, PAIR)):
        raise ValueError('invalid_gpu_resource_policy')
    if value['mode'] == PAIR:
        _validate_pair_distribution(root)
    return value['mode']


def lock_paths(uid=None):
    uid = os.getuid() if uid is None else uid
    stem = f'ai-tutor-gpu-0-{uid}'
    return {'global': Path('/tmp') / f'ai-tutor-tts-gpu-0-{uid}.lock',
            TTS_ROLE: Path('/tmp') / f'{stem}-qwen-tts.lock',
            ASR_ROLE: Path('/tmp') / f'{stem}-qwen-asr.lock',
            'init': Path('/tmp') / f'{stem}-init.lock'}


def open_lock(path, *, shared=False):
    locks = _fcntl()
    fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o600)
    try:
        locks.flock(fd, (locks.LOCK_SH if shared else locks.LOCK_EX) | locks.LOCK_NB)
    except BaseException:
        os.close(fd)
        raise
    return fd


def validate_qwen_profile(root, profile):
    profile = Path(profile)
    if profile.name != 'qwen3_tts_12gb.yaml' or hashlib.sha256(profile.read_bytes()).hexdigest() != PROFILE_SHA256:
        raise ValueError('pair_requires_fixed_qwen256_graph_profile')
    config = Path(root) / 'models/speech/Qwen3-TTS-12Hz-0.6B-CustomVoice/config.json'
    if hashlib.sha256(config.read_bytes()).hexdigest() != TTS_CONFIG_SHA256:
        raise ValueError('pair_requires_fixed_qwen_tts_06b_config')


def role_is_held(role, *, paths=None):
    paths = lock_paths() if paths is None else paths
    locks = _fcntl()
    try:
        fd = os.open(paths[role], os.O_RDWR)
    except FileNotFoundError:
        return False
    try:
        try:
            locks.flock(fd, locks.LOCK_EX | locks.LOCK_NB)
        except BlockingIOError:
            return True
        locks.flock(fd, locks.LOCK_UN)
        return False
    finally:
        os.close(fd)


def port_idle(port):
    with socket.socket() as probe:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            probe.bind(('127.0.0.1', port))
        except OSError:
            return False
    return True


def check_pair_ports(role, *, paths=None, qwen_port=18868, cosy_port=18869):
    if not port_idle(cosy_port):
        raise RuntimeError('pair_refuses_cosy_or_legacy_18869_listener')
    if not port_idle(qwen_port):
        if role != ASR_ROLE or not role_is_held(TTS_ROLE, paths=paths):
            raise RuntimeError('pair_refuses_unleased_qwen_listener')


class Admission:
    def __init__(self, mode, role, global_fd, *, paths=None, role_fd=None):
        self.mode, self.role, self.global_fd = mode, role, global_fd
        self.paths = paths
        self.role_fd, self.init_fd = role_fd, None

    @classmethod
    def exclusive(cls, fd, role):
        return cls(EXCLUSIVE, role, fd)

    @classmethod
    def pair(cls, root, role, *, profile=None):
        if load_policy(root) != PAIR or role not in (TTS_ROLE, ASR_ROLE):
            raise ValueError('pair_not_authorized_for_role')
        if role == TTS_ROLE:
            validate_qwen_profile(root, profile)
        paths = lock_paths()
        global_fd = open_lock(paths['global'], shared=True)
        try:
            role_fd = open_lock(paths[role])
        except BaseException:
            os.close(global_fd)
            raise
        return cls(PAIR, role, global_fd, paths=paths, role_fd=role_fd)

    def try_initialization(self):
        if self.mode == EXCLUSIVE or self.init_fd is not None:
            return True
        try:
            self.init_fd = open_lock(self.paths['init'])
        except BlockingIOError:
            return False
        return True

    def release_initialization(self):
        if self.init_fd is None:
            return False
        # Child inherited this same open-file description. Closing only the
        # parent's fd would leave the EX lock held by the running child.
        fd = self.init_fd
        _fcntl().flock(fd, _fcntl().LOCK_UN)
        self.init_fd = None
        os.close(fd)
        return True

    @property
    def pass_fds(self):
        if self.mode == PAIR and (self.role_fd is None or self.init_fd is None):
            raise RuntimeError('pair_initialization_lease_not_acquired')
        return tuple(fd for fd in (self.global_fd, self.role_fd, self.init_fd) if fd is not None)

    def environment(self):
        value = {MODE_ENV: self.mode, ROLE_ENV: self.role, GLOBAL_ENV: str(self.global_fd)}
        if self.mode == PAIR:
            self.pass_fds  # Require all three before launching.
            value.update({ROLE_FD_ENV: str(self.role_fd), INIT_ENV: str(self.init_fd)})
        return value

    def close(self):
        # Do not unlock global/role here: live children must retain their locks
        # if cleanup was unconfirmed. Normal callers close after group retirement.
        for name in ('init_fd', 'role_fd', 'global_fd'):
            fd = getattr(self, name)
            if fd is not None:
                os.close(fd)
                setattr(self, name, None)


def wait_initialization(admission, poll_control, waiting, *, timeout=600, clock=time.monotonic):
    """Return a stop reason or None; owners retain their distinct wire protocols."""
    if admission.try_initialization():
        return None
    waiting()
    deadline = clock() + timeout
    while True:
        remaining = deadline - clock()
        if remaining <= 0:
            raise TimeoutError('gpu_initialization_wait_timeout')
        reason = poll_control(min(.1, remaining))
        if reason is not None:
            return reason
        if admission.try_initialization():
            return None


def _inherited_fd(environment, name, path):
    value = environment.get(name, '')
    if not value.isascii() or not value.isdecimal() or int(value) < 3:
        raise ValueError('invalid_inherited_gpu_fd')
    fd = int(value)
    actual, expected = os.fstat(fd), os.stat(path)
    if (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino):
        raise ValueError('inherited_gpu_fd_inode_mismatch')
    return fd


def validate_inherited_pair(root, profile, caller, environment=None):
    environment = os.environ if environment is None else environment
    if load_policy(root) != PAIR or environment.get(MODE_ENV) != PAIR or environment.get(ROLE_ENV) != TTS_ROLE:
        raise ValueError('inherited_pair_not_authorized')
    if Path(caller).name != 'start_qwen_omni.sh':
        raise ValueError('pair_shell_requires_qwen_launcher')
    validate_qwen_profile(root, profile)
    paths, locks = lock_paths(), _fcntl()
    global_fd = _inherited_fd(environment, GLOBAL_ENV, paths['global'])
    role_fd = _inherited_fd(environment, ROLE_FD_ENV, paths[TTS_ROLE])
    init_fd = _inherited_fd(environment, INIT_ENV, paths['init'])
    if len({global_fd, role_fd, init_fd}) != 3 or environment.get('AI_TUTOR_TTS_LEASE_FD') != str(global_fd):
        raise ValueError('pair_fd_roles_must_be_distinct')
    locks.flock(global_fd, locks.LOCK_SH | locks.LOCK_NB)  # Never upgrade pair global to EX.
    locks.flock(role_fd, locks.LOCK_EX | locks.LOCK_NB)
    locks.flock(init_fd, locks.LOCK_EX | locks.LOCK_NB)
    check_pair_ports(TTS_ROLE, paths=paths)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--validate-inherited', action='store_true', required=True)
    parser.add_argument('--root', required=True)
    parser.add_argument('--profile', required=True)
    parser.add_argument('--caller', required=True)
    args = parser.parse_args()
    try:
        validate_inherited_pair(args.root, args.profile, args.caller)
    except (OSError, ValueError, RuntimeError):
        print('Qwen pair GPU admission validation failed', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
