"""Linux owner for one private-pipe, fixed BGE-M3 CPU worker.

No model loads before the owner's start command. No HTTP, database, downloads,
arbitrary commands, or user-text logging. Importing this module uses only stdlib.
"""
import argparse
import ctypes
import math
import os
from pathlib import Path, PurePosixPath
import re
import select
import selectors
import signal
import subprocess
import sys
import time

import embedding_worker as wire

MAX_LINE = wire.MAX_LINE
START_GATE_SECONDS = 10
STARTUP_SECONDS = 90
ENCODE_SECONDS = 60
SIGKILL = getattr(signal, 'SIGKILL', 9)
SIGHUP = getattr(signal, 'SIGHUP', 1)


class SupervisorError(ValueError):
    pass


class CleanupFailed(RuntimeError):
    pass


def message(instance, state, **fields):
    return {'protocol_version': 1, 'instance_id': instance, 'state': state, **fields}


class Lines:
    """At most one incomplete 64 KiB frame; never decode an unterminated tail."""
    def __init__(self):
        self.pending = bytearray()

    def feed(self, data):
        self.pending.extend(data)
        frames = []
        while True:
            end = self.pending.find(b'\n')
            if end < 0:
                if len(self.pending) >= MAX_LINE:
                    raise SupervisorError('line_limit')
                return frames
            if end + 1 > MAX_LINE:
                raise SupervisorError('line_limit')
            frames.append(bytes(self.pending[:end + 1]))
            del self.pending[:end + 1]

    def eof(self):
        if self.pending:
            raise SupervisorError('unterminated_line')


class Output:
    """Bounded nonblocking writes keep owner EOF/stop observable under pressure."""
    def __init__(self, fd):
        self.fd = fd
        self.pending = bytearray()
        os.set_blocking(fd, False)

    def queue(self, value):
        raw = wire.wire_bytes(value)
        if len(self.pending) + len(raw) > MAX_LINE:
            raise SupervisorError('output_backpressure')
        self.pending.extend(raw)

    def flush(self):
        if not self.pending:
            return
        try:
            count = os.write(self.fd, self.pending)
        except BlockingIOError:
            return
        if count <= 0:
            raise BrokenPipeError()
        del self.pending[:count]


def finite_number(value):
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def check_timings(value, fields):
    if not isinstance(value, dict) or set(value) != set(fields) or not all(finite_number(value[k]) for k in fields):
        raise SupervisorError('worker_timings')


class WorkerEvents:
    """Only exact, validated receipts may leave the private worker pipe."""
    COMMON = {'protocol_version', 'instance_id', 'state'}

    def __init__(self, instance):
        self.instance = instance
        self.stage = 'starting'
        self.pending = None

    def reserve(self, command):
        if self.stage != 'ready' or self.pending is not None:
            raise SupervisorError('worker_not_idle')
        self.pending = {key: command[key] for key in ('request_id', 'role', 'input_sha256')}

    def receive(self, value):
        if (not isinstance(value, dict) or value.get('protocol_version') != 1
                or type(value.get('protocol_version')) is not int
                or value.get('instance_id') != self.instance):
            raise SupervisorError('worker_identity')
        state = value.get('state')
        if state == 'loading':
            allowed = self.COMMON | {'profile', 'ready', 'warmup_completed'}
            if self.stage != 'starting' or value.get('ready') is not False or value.get('warmup_completed') is not False:
                raise SupervisorError('worker_stage')
            self.stage = 'loading'
        elif state == 'ready':
            allowed = self.COMMON | {'profile', 'ready', 'warmup_completed', 'device', 'dtype',
                                     'intra_threads', 'inter_threads', 'cuda_visible_devices',
                                     'load_count', 'warmup_tokens', 'timings'}
            if (self.stage != 'loading' or value.get('ready') is not True or value.get('warmup_completed') is not True
                    or value.get('device') != 'cpu' or value.get('dtype') != 'float32'
                    or type(value.get('intra_threads')) is not int or value['intra_threads'] != 2
                    or type(value.get('inter_threads')) is not int or value['inter_threads'] != 1
                    or value.get('cuda_visible_devices') != '' or type(value.get('load_count')) is not int
                    or value['load_count'] != 1 or type(value.get('warmup_tokens')) is not int
                    or not 1 <= value['warmup_tokens'] <= 512):
                raise SupervisorError('worker_ready_contract')
            check_timings(value.get('timings'), ('hash_audit_ms', 'model_load_ms', 'warmup_ms', 'total_ms'))
            self.stage = 'ready'
        elif state in ('encoded', 'encode_error'):
            allowed = self.COMMON | {'request_id', 'role', 'input_sha256', 'text_sha256', 'profile', 'tokens'}
            if self.stage != 'ready' or self.pending is None:
                raise SupervisorError('worker_unowned_result')
            if any(value.get(key) != expected for key, expected in self.pending.items()) or value.get('text_sha256') != self.pending['input_sha256']:
                raise SupervisorError('worker_result_binding')
            tokens = value.get('tokens')
            if state == 'encoded':
                allowed |= {'vector', 'timings'}
                vector = value.get('vector')
                if (type(tokens) is not int or not 1 <= tokens <= 512 or not isinstance(vector, list)
                        or len(vector) != 1024 or not all(type(v) in (int, float) and math.isfinite(v) for v in vector)
                        or abs(sum(float(v) * float(v) for v in vector) - 1.0) > .001):
                    raise SupervisorError('worker_vector_contract')
                check_timings(value.get('timings'), ('tokenize_ms', 'inference_ms', 'total_ms'))
            else:
                allowed |= {'code'}
                if value.get('code') == 'input_too_long':
                    if type(tokens) is not int or tokens <= 512:
                        raise SupervisorError('worker_length_error')
                elif value.get('code') == 'encode_failed':
                    if tokens is not None and (type(tokens) is not int or not 1 <= tokens <= 512):
                        raise SupervisorError('worker_encode_error')
                else:
                    raise SupervisorError('worker_error_code')
            self.pending = None
        elif state == 'failed':
            allowed = self.COMMON | {'code'}
            if value.get('code') not in ('asset_mismatch', 'local_model_load_failed', 'warmup_failed', 'invalid_owner_protocol'):
                raise SupervisorError('worker_failure_code')
            self.stage = 'failed'
        elif state == 'worker_stopped':
            allowed = self.COMMON | {'ready', 'warmup_completed'}
            if value.get('ready') is not False or value.get('warmup_completed') is not False:
                raise SupervisorError('worker_stop_flags')
            self.stage = 'stopped'
        else:
            raise SupervisorError('worker_event')
        if set(value) != allowed:
            raise SupervisorError('worker_fields')
        if 'profile' in allowed and value.get('profile') != wire.PROFILE:
            raise SupervisorError('worker_profile')
        return value


def absolute_model_dir(value):
    path = PurePosixPath(value)
    if (not path.is_absolute() or '..' in path.parts
            or any(char in value for char in ('\0', '\n', '\r', '\\'))):
        raise SupervisorError('invalid_model_directory')
    result = Path(str(path)).resolve(strict=True)
    if not result.is_dir():
        raise SupervisorError('invalid_model_directory')
    return result


def parent_death_guard(parent_pid):
    """Called only in the newly forked Linux child, before exec and imports."""
    # Do not inherit the manager's graceful-stop handler in the pre-exec gap:
    # a parent-death signal there must terminate this child immediately.
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(1, signal.SIGTERM, 0, 0, 0) != 0 or os.getppid() != parent_pid:
        os._exit(125)


def live_group_members(group, proc_root=Path('/proc')):
    members = []
    for entry in proc_root.iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            fields = (entry / 'stat').read_text(encoding='utf-8', errors='replace').rsplit(') ', 1)[1].split()
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        if int(fields[2]) == group:
            if int(fields[3]) != group:
                raise CleanupFailed('owned_group_session_changed')
            if fields[0] not in ('Z', 'X', 'x'):
                members.append(int(entry.name))
    return members


def signal_group(group, kind):
    try:
        os.killpg(group, kind)
    except ProcessLookupError:
        pass


def close_worker_input(process, instance, pending=b''):
    try:
        fd = process.stdin.fileno()
        os.set_blocking(fd, False)
        outgoing = bytearray(pending)
        outgoing.extend(wire.wire_bytes({'protocol_version': 1, 'instance_id': instance, 'op': 'stop'}))
        deadline = time.monotonic() + 1
        while outgoing and time.monotonic() < deadline:
            try:
                written = os.write(fd, outgoing)
                if written <= 0:
                    break
                del outgoing[:written]
            except BlockingIOError:
                select.select([], [fd], [], .02)
    except (OSError, ValueError):
        pass
    finally:
        try:
            process.stdin.close()
        except (OSError, ValueError):
            pass


def stop_owned(process, instance, drain=lambda: None, pending=b'', *,
               request_stop=close_worker_input, members=live_group_members,
               send_signal=signal_group, clock=time.monotonic, sleep=time.sleep,
               graceful_seconds=10, term_seconds=5, kill_seconds=5):
    """Only this Popen's new session/group is eligible for signals."""
    request_stop(process, instance, pending)

    def wait_group(seconds):
        deadline = clock() + seconds
        while True:
            drain()
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
    process.wait(timeout=1)
    if members(process.pid):
        raise CleanupFailed('owned_group_still_live_after_wait')
    drain()
    return {'worker_started': True, 'worker_exit_code': process.returncode,
            'forced_group_stop': forced, 'linux_cleanup_confirmed': True, 'group_reaped': True}


def run(args):
    process = None
    selector = None
    output = None
    outgoing_worker = bytearray()
    owner_lines, worker_lines = Lines(), Lines()
    events = WorkerEvents(args.instance)
    signals = {}
    stop_signal = [False]
    reason = 'startup_failed'
    cleanup_ok = True

    def register(fd, event, name):
        try:
            selector.modify(fd, event, name)
        except KeyError:
            selector.register(fd, event, name)

    def unregister(fd):
        try:
            selector.unregister(fd)
        except (KeyError, ValueError):
            pass

    def emit(state, **fields):
        output.queue(message(args.instance, state, **fields))
        register(output.fd, selectors.EVENT_WRITE, 'output')

    def drain_cleanup():
        if process is not None:
            try:
                while os.read(process.stdout.fileno(), 8192):
                    pass
            except (BlockingIOError, OSError, ValueError):
                pass
        if output is not None:
            try:
                output.flush()
            except (OSError, ValueError):
                pass

    try:
        output = Output(sys.stdout.fileno())
        selector = selectors.DefaultSelector()
        owner_fd = sys.stdin.fileno()
        os.set_blocking(owner_fd, False)
        register(owner_fd, selectors.EVENT_READ, 'owner')
        if sys.platform != 'linux' or not re.fullmatch(r'[0-9a-f]{32}', args.instance):
            raise SupervisorError('linux_instance_required')
        model_dir = absolute_model_dir(args.model_dir)
        worker_path = Path(__file__).resolve().with_name('embedding_worker.py')
        if not worker_path.is_file():
            raise SupervisorError('worker_bundle_missing')
        for kind in (signal.SIGINT, signal.SIGTERM, SIGHUP):
            signals[kind] = signal.signal(kind, lambda *_: stop_signal.__setitem__(0, True))
        emit('awaiting_owner', manager_pid=os.getpid(), ready=False, warmup_completed=False)
        deadline = time.monotonic() + START_GATE_SECONDS
        encode_deadline = None
        done = False
        while not done:
            if stop_signal[0]:
                reason = 'owner_signal'
                break
            now = time.monotonic()
            if process is None and now >= deadline:
                reason = 'owner_timeout'
                emit('failed', code='owner_timeout')
                break
            if process is not None and events.stage != 'ready' and now >= deadline:
                reason = 'startup_timeout'
                emit('failed', code='startup_timeout')
                break
            if encode_deadline is not None and now >= encode_deadline:
                reason = 'encode_timeout'
                emit('failed', code='encode_timeout')
                break
            for key, _mask in selector.select(.1):
                if key.data == 'output':
                    output.flush()
                    if not output.pending:
                        unregister(output.fd)
                    continue
                if key.data == 'worker_input':
                    try:
                        written = os.write(key.fd, outgoing_worker)
                    except BlockingIOError:
                        continue
                    if written <= 0:
                        raise BrokenPipeError()
                    del outgoing_worker[:written]
                    if not outgoing_worker:
                        unregister(key.fd)
                    continue
                try:
                    raw = os.read(key.fd, 8192)
                except BlockingIOError:
                    continue
                if key.data == 'owner':
                    if not raw:
                        owner_lines.eof()
                        reason = 'owner_eof'
                        done = True
                        break
                    for frame in owner_lines.feed(raw):
                        command = wire.decode_line(frame)
                        op = wire.validate_command(command, args.instance)
                        if op == 'stop':
                            reason = 'owner_stop'
                            done = True
                            break
                        if op == 'start':
                            if process is not None:
                                raise SupervisorError('duplicate_start')
                            parent_pid = os.getpid()
                            process = subprocess.Popen(
                                [sys.executable, '-B', '-u', str(worker_path), '--instance', args.instance,
                                 '--model-dir', str(model_dir)],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                env=wire.cpu_environment(os.environ), bufsize=0, start_new_session=True,
                                preexec_fn=lambda: parent_death_guard(parent_pid))
                            if os.getpgid(process.pid) != process.pid or os.getsid(process.pid) != process.pid:
                                raise CleanupFailed('worker_session_not_owned')
                            os.set_blocking(process.stdout.fileno(), False)
                            os.set_blocking(process.stdin.fileno(), False)
                            register(process.stdout.fileno(), selectors.EVENT_READ, 'worker_output')
                            deadline = time.monotonic() + STARTUP_SECONDS
                            emit('starting', pid=process.pid, process_group=process.pid,
                                 ready=False, warmup_completed=False)
                        elif op == 'encode':
                            if process is None or outgoing_worker:
                                raise SupervisorError('worker_not_ready')
                            events.reserve(command)
                            outgoing_worker.extend(wire.wire_bytes(command))
                            register(process.stdin.fileno(), selectors.EVENT_WRITE, 'worker_input')
                            encode_deadline = time.monotonic() + ENCODE_SECONDS
                        else:
                            raise SupervisorError('invalid_owner_command')
                else:
                    if not raw:
                        worker_lines.eof()
                        reason = 'worker_exited'
                        done = True
                        break
                    for frame in worker_lines.feed(raw):
                        value = events.receive(wire.decode_line(frame))
                        if value['state'] == 'worker_stopped':
                            reason = 'worker_exited'
                            done = True
                            break
                        output.queue(value)
                        register(output.fd, selectors.EVENT_WRITE, 'output')
                        if value['state'] in ('encoded', 'encode_error'):
                            encode_deadline = None
                        if value['state'] == 'failed':
                            reason = 'worker_failed'
                            done = True
                            break
                if done:
                    break
    except (SupervisorError, wire.ProtocolError, ValueError, UnicodeError):
        reason = 'protocol_error'
        try:
            emit('failed', code='invalid_private_protocol')
        except (OSError, ValueError, AttributeError):
            pass
    except (OSError, CleanupFailed, subprocess.SubprocessError):
        reason = 'owner_or_worker_error'
        try:
            emit('failed', code='local_owner_failed')
        except (OSError, ValueError, AttributeError):
            pass
    finally:
        receipt = {'worker_started': False, 'worker_exit_code': None, 'forced_group_stop': False,
                   'linux_cleanup_confirmed': True, 'group_reaped': True}
        if process is not None:
            try:
                receipt = stop_owned(process, args.instance, drain_cleanup, bytes(outgoing_worker))
            except (CleanupFailed, OSError, ValueError, subprocess.SubprocessError):
                cleanup_ok = False
                receipt = {'worker_started': True, 'worker_exit_code': process.poll(),
                           'forced_group_stop': True, 'linux_cleanup_confirmed': False, 'group_reaped': False}
            try:
                process.stdout.close()
            except (OSError, ValueError):
                pass
        if output is not None:
            drain_cleanup()
            try:
                output.queue(message(args.instance, 'stopped' if cleanup_ok else 'cleanup_failed',
                                     reason=reason, ready=False, warmup_completed=False, **receipt))
                flush_deadline = time.monotonic() + 2
                while output.pending and time.monotonic() < flush_deadline:
                    output.flush()
                    if output.pending:
                        select.select([], [output.fd], [], .02)
            except (OSError, ValueError):
                pass
        if selector is not None:
            selector.close()
        for kind, previous in signals.items():
            signal.signal(kind, previous)
    return 0 if cleanup_ok and reason in ('owner_stop', 'owner_eof', 'owner_signal') else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--instance', required=True)
    parser.add_argument('--model-dir', required=True)
    return run(parser.parse_args())


if __name__ == '__main__':
    raise SystemExit(main())
