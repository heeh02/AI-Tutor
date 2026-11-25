"""Linux owner of one independent Qwen offline-final service process group.

Credentials travel only over private stdin. This owner installs nothing and
opens no audio devices. It holds the existing TTS GPU lease exclusively.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
from queue import Empty, Full, Queue
import re
import selectors
import signal
import socket
import subprocess
import sys
import threading
import time
from urllib.parse import urlsplit


def _load_admission_helper():
    bundle = Path(__file__).resolve().parent
    sibling = bundle / 'gpu_admission.py'
    # Installed bundle first; source checkout has one shared speech-lab source.
    helper = sibling if sibling.is_file() else bundle.parent / 'speech-lab/gpu_admission.py'
    if not sibling.is_file() and bundle.parent.name != 'scripts':
        raise ImportError('installed_gpu_admission_helper_missing')
    if not helper.is_file():
        raise ImportError('gpu_admission_helper_missing')
    spec = importlib.util.spec_from_file_location('gpu_admission', helper)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module, helper


gpu_admission, GPU_ADMISSION_HELPER = _load_admission_helper()

MAX_LINE = 4096
SIGKILL = getattr(signal, 'SIGKILL', 9)  # Linux value; allows Windows fake tests.
BUNDLE_FILES = ('managed_final_asr.py', 'qwen_final_service.py',
                'qwen_final_engine.py', 'qwen-model.json', 'qwen-sdk.json')
PROVIDER = 'Qwen3-ASR-1.7B / PyTorch BF16 / local offline final'


class ProtocolError(ValueError):
    pass


class CleanupFailed(RuntimeError):
    pass


def absolute_linux_path(value):
    path = PurePosixPath(value)
    if not path.is_absolute() or '..' in path.parts or any(char in value for char in ('\0', '\n', '\r', '\\')):
        raise ValueError('absolute_linux_path_required')
    return path


def cache_path(value):
    path = absolute_linux_path(value)
    if path.parts[:3] != ('/', 'mnt', 'c') or not path.name.startswith('ai-tutor-'):
        raise ValueError('task_owned_c_drive_cache_required')
    return path


def service_environment(cache, site_packages, inherited):
    cache = cache_path(str(cache))
    packages = absolute_linux_path(str(site_packages))
    environment = dict(inherited)
    for name in ('PYTHONPATH', 'PYTHONHOME', 'CUDA_HOME', 'CUDA_PATH', 'VLLM_API_KEY',
                 'VLLM_CONFIG_ROOT', 'AI_TUTOR_MANAGED_TTS_TOKEN', 'AI_TUTOR_TTS_LEASE_FD'):
        environment.pop(name, None)
    for name in gpu_admission.ENV_NAMES:
        environment.pop(name, None)
    environment.update({
        'PYTHONNOUSERSITE': '1', 'PYTHONUNBUFFERED': '1', 'PYTHONIOENCODING': 'utf-8',
        'HF_HUB_OFFLINE': '1', 'TRANSFORMERS_OFFLINE': '1',
        'HF_HUB_DISABLE_TELEMETRY': '1', 'TOKENIZERS_PARALLELISM': 'false',
        'CUDA_VISIBLE_DEVICES': '0',
        'LD_LIBRARY_PATH': str(packages / 'nvidia/cu13/lib') + ':/usr/lib/wsl/lib',
        'HF_HOME': str(cache / 'hf'), 'XDG_CACHE_HOME': str(cache / 'xdg'),
        'CUDA_CACHE_PATH': str(cache / 'cuda'), 'TRITON_CACHE_DIR': str(cache / 'triton'),
        'TORCHINDUCTOR_CACHE_DIR': str(cache / 'inductor'),
        'TORCH_EXTENSIONS_DIR': str(cache / 'extensions'), 'NUMBA_CACHE_DIR': str(cache / 'numba'),
    })
    return environment


class Commands:
    def __init__(self, fd):
        self.fd, self.pending = fd, bytearray()
        self.selector = selectors.DefaultSelector()
        self.selector.register(fd, selectors.EVENT_READ)

    def read(self, timeout):
        if b'\n' not in self.pending:
            if not self.selector.select(timeout):
                return None
            data = os.read(self.fd, MAX_LINE + 1)
            if not data:
                raise EOFError()
            self.pending.extend(data)
        end = self.pending.find(b'\n')
        if end > MAX_LINE - 1 or (end < 0 and len(self.pending) > MAX_LINE):
            raise ProtocolError('owner_line_limit')
        if end < 0:
            return None
        raw = bytes(self.pending[:end])
        del self.pending[:end + 1]
        value = json.loads(raw)
        if not isinstance(value, dict):
            raise ProtocolError('owner_object_required')
        return value

    def close(self):
        self.selector.close()


def bootstrap_token(value, instance):
    if (not isinstance(value, dict)
            or set(value) != {'protocol_version', 'instance_id', 'bearer_token'}
            or type(value['protocol_version']) is not int or value['protocol_version'] != 1
            or value['instance_id'] != instance or not isinstance(value['bearer_token'], str)
            or not re.fullmatch(r'[A-Za-z0-9_.-]{24,256}', value['bearer_token'])):
        raise ProtocolError('invalid_private_bootstrap')
    return value['bearer_token']


def command_kind(value, instance):
    if value == {'op': 'stop', 'instance_id': instance}:
        return 'stop'
    if isinstance(value, dict) and set(value) == {'op', 'instance_id'} and value['op'] == 'stop' and isinstance(value['instance_id'], str):
        return 'stale_stop'
    raise ProtocolError('invalid_owner_control')


class ServiceEvents:
    """Validate this one service's finite startup/stop receipts before forwarding."""
    ORDER = ('awaiting_owner', 'loading', 'warming', 'warm_ready')

    def __init__(self, instance):
        self.instance, self.stage, self.endpoint = instance, 0, None

    def validate(self, raw):
        if not raw.endswith(b'\n') or len(raw) > MAX_LINE:
            raise ProtocolError('service_line_limit')
        value = json.loads(raw)
        if not isinstance(value, dict) or type(value.get('protocol_version')) is not int or value.get('protocol_version') != 1 or value.get('instance_id') != self.instance:
            raise ProtocolError('service_identity_mismatch')
        state = value.get('state')
        common = {'protocol_version', 'instance_id', 'state', 'ready', 'warmup_completed'}
        if state in self.ORDER:
            if self.stage >= len(self.ORDER) or state != self.ORDER[self.stage]:
                raise ProtocolError('service_stage_order')
            expected_warm = state == 'warm_ready'
            if value.get('ready') is not expected_warm or value.get('warmup_completed') is not expected_warm:
                raise ProtocolError('service_warm_flags')
            allowed = common
            if state != 'awaiting_owner':
                allowed |= {'base_url', 'model_ready', 'busy', 'closing', 'provider', 'inference_mode'}
                if value.get('model_ready') is not (state != 'loading') or value.get('busy') is not False or value.get('closing') is not False or value.get('provider') != PROVIDER or value.get('inference_mode') != 'offline_final':
                    raise ProtocolError('service_model_flags')
                endpoint = value.get('base_url')
                if not isinstance(endpoint, str) or len(endpoint) > 128:
                    raise ProtocolError('service_endpoint')
                url = urlsplit(endpoint)
                try:
                    valid = (url.scheme == 'http' and url.hostname == '127.0.0.1' and
                             url.port is not None and url.port > 0 and not url.username and
                             not url.password and url.path in ('', '/') and not url.query and not url.fragment)
                except ValueError:
                    valid = False
                if not valid or (self.endpoint is not None and endpoint != self.endpoint):
                    raise ProtocolError('service_endpoint')
                self.endpoint = endpoint
            self.stage += 1
        elif state == 'failed':
            allowed = common | {'reason', 'model_started'}
            if value.get('reason') not in ('bootstrap_or_local_configuration_failed', 'local_final_preparation_failed'):
                raise ProtocolError('service_failure_reason')
        elif state == 'cleanup_pending':
            allowed = common | {'worker_retired'}
            if value.get('worker_retired') is not False:
                raise ProtocolError('service_cleanup_pending')
        elif state == 'stopped':
            allowed = common | {'http_closed', 'worker_retired', 'process_exit_still_required'}
            if value.get('http_closed') is not True or value.get('worker_retired') is not True or value.get('process_exit_still_required') is not True:
                raise ProtocolError('service_stopped_receipt')
        else:
            raise ProtocolError('unexpected_service_state')
        if state not in self.ORDER and (value.get('ready') is not False or value.get('warmup_completed') is not False):
            raise ProtocolError('service_terminal_flags')
        if set(value) - allowed:
            raise ProtocolError('unexpected_service_fields')
        return value


def validated_service_event(validator, raw, admission):
    value = validator.validate(raw)
    if value['state'] == 'warm_ready' and admission is not None:
        admission.release_initialization()
    return value


def wait_for_gpu_initialization(admission, commands, instance, signal_stop, waiting):
    def poll_wait(timeout):
        if signal_stop.is_set():
            return 'owner_signal'
        try:
            command = commands.read(timeout)
        except EOFError:
            return 'owner_eof'
        if command is not None and command_kind(command, instance) == 'stop':
            return 'owner_stop'
        return None
    return gpu_admission.wait_initialization(admission, poll_wait, waiting)


def gpu_lease():
    import fcntl
    path = Path('/tmp') / f'ai-tutor-tts-gpu-0-{os.getuid()}.lock'
    fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        # Also refuse historical unmanaged project TTS listeners; never stop them.
        for port in (18868, 18869):
            with socket.socket() as probe:
                probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                probe.bind(('127.0.0.1', port))
        return fd
    except BaseException:
        os.close(fd)
        raise


def live_group_members(group):
    members = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            fields = (entry / 'stat').read_text().rsplit(') ', 1)[1].split()
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        if int(fields[2]) == group and fields[0] not in ('Z', 'X', 'x'):
            if int(fields[3]) != group:
                raise CleanupFailed('owned_group_session_changed')
            members.append(int(entry.name))
    return members


def signal_group(group, kind):
    try:
        os.killpg(group, kind)
    except ProcessLookupError:
        pass


def stop_owned(process, instance, poll_events=lambda: None, *, members=live_group_members,
               send_signal=signal_group, clock=time.monotonic, sleep=time.sleep,
               graceful_seconds=10, term_seconds=5, kill_seconds=5):
    """Only the PGID created here by start_new_session; never a name/PID search."""
    try:
        process.stdin.write((json.dumps({'op': 'stop', 'instance_id': instance}) + '\n').encode())
        process.stdin.flush()
    except (OSError, ValueError):
        pass
    finally:
        try:
            process.stdin.close()
        except (OSError, ValueError):
            pass

    def wait_group(seconds):
        deadline = clock() + seconds
        while True:
            poll_events()
            process.poll()
            if not members(process.pid):
                return True
            if clock() >= deadline:
                return False
            sleep(.02)

    forced = False
    if not wait_group(graceful_seconds):
        forced = True
        send_signal(process.pid, signal.SIGTERM)
        if not wait_group(term_seconds):
            send_signal(process.pid, SIGKILL)
            if not wait_group(kill_seconds):
                raise CleanupFailed('owned_group_still_live')
    process.wait(timeout=5)
    if members(process.pid):
        raise CleanupFailed('owned_group_returned_after_wait')
    poll_events()
    return {'forced_group_stop': forced, 'service_exit_code': process.returncode,
            'linux_cleanup_confirmed': True, 'process_started': True}


def child_command(python, bundle, root, instance):
    # Do not canonicalize python's symlink: its venv path selects the environment.
    return [str(python), '-B', str(PurePosixPath(str(bundle)) / 'qwen_final_service.py'),
            '--root', str(root), '--instance', instance, '--port', '0']


def run(args, source=sys.stdin, output=sys.stdout):
    def emit(state, **fields):
        output.write(json.dumps({'protocol_version': 1, 'instance_id': args.instance,
                                 'state': state, **fields}) + '\n')
        output.flush()
    process, lease, commands, reader, admission = None, None, None, None, None
    reader_stop, signal_stop = threading.Event(), threading.Event()
    events = Queue(maxsize=16)
    validator = ServiceEvents(args.instance)
    reason, invalid_service_event = 'startup_failed', False

    def forward_events(suppress=False):
        nonlocal invalid_service_event
        while True:
            try:
                kind, raw = events.get_nowait()
            except Empty:
                return
            if kind != 'line':
                if kind == 'error':
                    invalid_service_event = True
                continue
            try:
                value = validated_service_event(validator, raw, admission)
                output.write(json.dumps(value) + '\n')
                output.flush()
            except (ValueError, UnicodeError, OSError):
                invalid_service_event = True
                if not suppress:
                    raise ProtocolError('service_event_rejected')

    def read_service():
        try:
            while not reader_stop.is_set():
                raw = process.stdout.readline(MAX_LINE + 1)
                item = ('line', raw) if raw else ('eof', b'')
                while not reader_stop.is_set():
                    try:
                        events.put(item, timeout=.1)
                        break
                    except Full:
                        continue
                if not raw:
                    return
        except (OSError, ValueError):
            try:
                events.put(('error', b''), timeout=.1)
            except Full:
                pass

    previous_signals = {}
    try:
        if sys.platform != 'linux' or not re.fullmatch(r'[0-9a-f]{32}', args.instance):
            raise ValueError('linux_and_fresh_instance_required')
        root = Path(str(absolute_linux_path(args.root))).resolve(strict=True)
        python = Path(str(absolute_linux_path(args.python)))
        cache = Path(str(cache_path(args.cache_dir))).resolve()
        cache_path(str(cache))
        bundle = Path(__file__).resolve().parent
        if not python.is_file() or not all((bundle / name).is_file() for name in BUNDLE_FILES):
            raise ValueError('installed_bundle_or_interpreter_missing')
        packages = python.parent.parent / 'lib/python3.12/site-packages'
        if python.parent.name != 'bin' or not (packages / 'nvidia/cu13/lib').is_dir():
            raise ValueError('independent_python312_cu130_environment_required')
        for kind in (signal.SIGTERM, signal.SIGINT):
            previous_signals[kind] = signal.signal(kind, lambda *_: signal_stop.set())
        commands = Commands(source.fileno())
        bootstrap, deadline = None, time.monotonic() + 5
        while bootstrap is None and time.monotonic() < deadline and not signal_stop.is_set():
            bootstrap = commands.read(.1)
        token = bootstrap_token(bootstrap, args.instance)
        if gpu_admission.load_policy(root) == gpu_admission.PAIR:
            admission = gpu_admission.Admission.pair(root, gpu_admission.ASR_ROLE)
            waiting_reason = wait_for_gpu_initialization(
                admission, commands, args.instance, signal_stop,
                lambda: emit('waiting_gpu', ready=False, warmup_completed=False))
            if waiting_reason is not None:
                reason = waiting_reason
                return 0
            gpu_admission.check_pair_ports(gpu_admission.ASR_ROLE, paths=admission.paths)
            lease = admission.global_fd
        else:
            lease = gpu_lease()
            admission = gpu_admission.Admission.exclusive(lease, gpu_admission.ASR_ROLE)
        environment = service_environment(str(cache), str(packages), os.environ)
        environment.update(admission.environment())
        for name in ('HF_HOME', 'XDG_CACHE_HOME', 'CUDA_CACHE_PATH', 'TRITON_CACHE_DIR',
                     'TORCHINDUCTOR_CACHE_DIR', 'TORCH_EXTENSIONS_DIR', 'NUMBA_CACHE_DIR'):
            Path(environment[name]).mkdir(parents=True, exist_ok=True)
        environment['AI_TUTOR_FINAL_ASR_LEASE_FD'] = str(lease)
        environment['VIRTUAL_ENV'] = str(python.parent.parent)
        process = subprocess.Popen(child_command(python, bundle, root, args.instance),
            cwd=root, env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=sys.stderr, start_new_session=True, pass_fds=admission.pass_fds)
        if os.getpgid(process.pid) != process.pid or os.getsid(process.pid) != process.pid:
            raise CleanupFailed('new_session_identity_not_verified')
        emit('starting', pid=process.pid, process_group=process.pid,
             ready=False, warmup_completed=False)
        process.stdin.write((json.dumps({'protocol_version': 1, 'instance_id': args.instance,
                                         'bearer_token': token}) + '\n').encode())
        process.stdin.flush()
        token = None
        bootstrap.clear()
        reader = threading.Thread(target=read_service, name='final-asr-service-events', daemon=True)
        reader.start()
        while True:
            if signal_stop.is_set():
                reason = 'owner_signal'
                break
            forward_events()
            if invalid_service_event:
                reason = 'protocol_error'
                break
            if process.poll() is not None:
                reason = 'service_exited' if validator.stage == len(validator.ORDER) else 'startup_failed'
                break
            try:
                command = commands.read(.1)
            except EOFError:
                reason = 'owner_eof'
                break
            except (ValueError, UnicodeError):
                reason = 'control_error'
                break
            if command is not None:
                try:
                    action = command_kind(command, args.instance)
                except ProtocolError:
                    reason = 'control_error'
                    break
                if action == 'stop':
                    reason = 'owner_stop'
                    break
    except ProtocolError:
        reason = 'protocol_error' if process is not None else 'startup_failed'
    except (ValueError, UnicodeError):
        reason = 'control_error' if process is not None else 'startup_failed'
    except (CleanupFailed, OSError, EOFError, subprocess.SubprocessError):
        reason = 'owner_error' if process is not None else 'startup_failed'
    finally:
        receipt = {'forced_group_stop': False, 'service_exit_code': 1,
                   'linux_cleanup_confirmed': True, 'process_started': False}
        cleanup_ok = process is None
        if process is not None:
            try:
                receipt = stop_owned(process, args.instance, lambda: forward_events(suppress=True))
                cleanup_ok = True
            except (CleanupFailed, OSError, ValueError, subprocess.SubprocessError):
                cleanup_ok = False
        reader_stop.set()
        if reader is not None:
            reader.join(timeout=.5)
        if cleanup_ok and process is not None and (reader is None or not reader.is_alive()):
            process.stdout.close()
        if commands is not None:
            commands.close()
        if admission is not None:
            admission.close()
        elif lease is not None:
            os.close(lease)
        for kind, previous in previous_signals.items():
            signal.signal(kind, previous)
        if cleanup_ok:
            emit('owned_stopped', reason=reason, **receipt)
        else:
            emit('cleanup_failed', reason=reason, process_started=True,
                 linux_cleanup_confirmed=False, ready=False, warmup_completed=False)
    return 0 if cleanup_ok and reason in ('owner_stop', 'owner_eof', 'owner_signal') else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', required=True)
    parser.add_argument('--instance', required=True)
    parser.add_argument('--python', required=True)
    parser.add_argument('--cache-dir', required=True)
    return run(parser.parse_args())


if __name__ == '__main__':
    raise SystemExit(main())
