"""Linux owner for one project TTS service; stdin EOF/Stop retires its group.

The Windows owner supplies a fresh credential over stdin, never argv. This
wrapper does not install a voice, open devices, prewarm or claim HTTP readiness.
"""
import argparse
import json
import os
from pathlib import Path
import re
import selectors
import signal
import socket
import subprocess
import sys
import time

import gpu_admission

ENGINES = {
    "qwen": (18868, "tutor-qwen-tts", "start_qwen_omni.sh"),
    "cosy": (18869, "tutor-cosy-tts", "start_cosy_omni.sh"),
}


def emit(state, instance, **fields):
    print(json.dumps({"protocol_version": 1, "state": state,
                      "instance_id": instance, **fields}), flush=True)


class Commands:
    def __init__(self, fd):
        self.fd, self.pending = fd, bytearray()
        self.selector = selectors.DefaultSelector()
        self.selector.register(fd, selectors.EVENT_READ)

    def read(self, timeout):
        if b"\n" not in self.pending:
            if not self.selector.select(timeout):
                return None
            data = os.read(self.fd, 4097)
            if not data:
                raise EOFError()
            self.pending.extend(data)
        if len(self.pending) > 4096:
            raise ValueError("control message exceeds 4096 bytes")
        if b"\n" not in self.pending:
            return None
        line, _, tail = self.pending.partition(b"\n")
        self.pending = bytearray(tail)
        value = json.loads(line)
        if not isinstance(value, dict):
            raise ValueError("control message must be an object")
        return value

    def close(self):
        self.selector.close()


def gpu_lease(path):
    """Held by wrapper and its direct exec child, not all multiprocessing workers."""
    return gpu_admission.open_lock(path)


def check_ports_idle(ports):
    # Covers an earlier un-managed project candidate too. Never stop that owner.
    for port in ports:
        with socket.socket() as probe:
            # Match the Unix TCP server's restart semantics. TIME_WAIT after a
            # completed owner stop is not a live listener; REUSEPORT is not set.
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                probe.bind(("127.0.0.1", port))
            except OSError as error:
                raise RuntimeError("A project TTS port is already owned") from error


def live_group_members(group):
    """Only the locally created PGID; zombies have already released resources."""
    members = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            fields = (entry / "stat").read_text().rsplit(") ", 1)[1].split()
        except (FileNotFoundError, ProcessLookupError):
            continue  # A process can finish while /proc is being enumerated.
        except PermissionError:
            # A different uid may be hidden by procfs policy. Our group is owned
            # by this uid and remains readable; never contact a foreign process.
            continue
        if int(fields[2]) == group and fields[0] not in {"Z", "X", "x"}:
            members.append(int(entry.name))
    return members


class CleanupFailed(RuntimeError):
    pass


def stop_owned(process, grace_seconds=5, kill_seconds=5):
    """The group was created here with start_new_session, never found by name."""
    forced = False
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    deadline = time.monotonic() + grace_seconds
    while live_group_members(process.pid) and time.monotonic() < deadline:
        process.poll()
        time.sleep(.02)
    if live_group_members(process.pid):
        forced = True
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        deadline = time.monotonic() + kill_seconds
        while live_group_members(process.pid) and time.monotonic() < deadline:
            process.poll()
            time.sleep(.02)
        if live_group_members(process.pid):
            raise CleanupFailed("Owned process group still has live members")
    process.wait(timeout=5)
    return forced


def supervise(process, commands, instance, stop_signal, grace_seconds=5, on_prepared=None):
    reason = "service_exited"
    try:
        while process.poll() is None:
            if stop_signal[0]:
                reason = "owner_signal"
                break
            try:
                command = commands.read(.1)
            except EOFError:
                reason = "owner_eof"
                break
            if command is None:
                continue
            if command == {"op": "stop", "instance_id": instance}:
                reason = "owner_stop"
                break
            if command == {"op": "prepared", "instance_id": instance}:
                # Rust has completed HTTP readiness plus its real bilingual warmup.
                if on_prepared is not None:
                    on_prepared()
                continue
            raise ValueError("unexpected control message or instance")
    except (ValueError, UnicodeError):
        reason = "control_error"
    finally:
        forced = stop_owned(process, grace_seconds)
    return {"reason": reason, "forced_group_stop": forced,
            "service_exit_code": process.returncode}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", required=True, choices=ENGINES)
    parser.add_argument("--instance", required=True)
    parser.add_argument("--root", type=Path, help="Explicit project root; assets stay beside this wrapper")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{32}", args.instance):
        parser.error("instance must be a new 32-character lowercase hex nonce")
    bundle = Path(__file__).resolve().parent
    root = (args.root or bundle.parent.parent).resolve()
    port, model, script = ENGINES[args.engine]
    commands = Commands(sys.stdin.fileno())
    lease, process, admission = None, None, None
    stop_signal = [False]
    for kind in (signal.SIGTERM, signal.SIGINT):
        signal.signal(kind, lambda *_: stop_signal.__setitem__(0, True))
    try:
        deadline = time.monotonic() + 5
        bootstrap = None
        while bootstrap is None and time.monotonic() < deadline and not stop_signal[0]:
            bootstrap = commands.read(.1)
        if (not bootstrap or set(bootstrap) != {"protocol_version", "bearer_token"}
                or bootstrap["protocol_version"] != 1
                or not isinstance(bootstrap["bearer_token"], str)
                or not re.fullmatch(r"[A-Za-z0-9_-]{32,128}", bootstrap["bearer_token"])):
            raise ValueError("valid private bootstrap required within 5 seconds")
        mode = gpu_admission.load_policy(root)
        if mode == gpu_admission.PAIR:
            if args.engine != "qwen":
                raise ValueError("Qwen pair policy cannot admit CosyVoice")
            admission = gpu_admission.Admission.pair(
                root, gpu_admission.TTS_ROLE, profile=bundle / "qwen3_tts_12gb.yaml")
            def poll_wait(timeout):
                if stop_signal[0]:
                    return "owner_signal"
                try:
                    command = commands.read(timeout)
                except EOFError:
                    return "owner_eof"
                if command is None:
                    return None
                if command == {"op": "stop", "instance_id": args.instance}:
                    return "owner_stop"
                raise ValueError("unexpected control while waiting for GPU initialization")
            waiting_reason = gpu_admission.wait_initialization(
                admission, poll_wait,
                lambda: emit("waiting_gpu", args.instance, ready=False, warmup_completed=False))
            if waiting_reason is not None:
                emit("failed", args.instance, error_type="AdmissionCancelled",
                     linux_cleanup_confirmed=True, process_started=False, ready=False, warm=False)
                return 0
            gpu_admission.check_pair_ports(gpu_admission.TTS_ROLE, paths=admission.paths)
            lease = admission.global_fd
        else:
            lease = gpu_lease(Path("/tmp") / f"ai-tutor-tts-gpu-0-{os.getuid()}.lock")
            admission = gpu_admission.Admission.exclusive(lease, "tts_" + args.engine)
            check_ports_idle([value[0] for value in ENGINES.values()])
        environment = os.environ.copy()
        for name in gpu_admission.ENV_NAMES:
            environment.pop(name, None)
        environment.update(admission.environment())
        environment["AI_TUTOR_MANAGED_TTS_TOKEN"] = bootstrap.pop("bearer_token")
        environment["AI_TUTOR_TTS_LEASE_FD"] = str(lease)
        environment["AI_TUTOR_PROJECT_ROOT"] = str(root)
        process = subprocess.Popen(
            ["bash", str(bundle / script)], cwd=root,
            env=environment, stdin=subprocess.DEVNULL, stdout=sys.stderr,
            stderr=sys.stderr, start_new_session=True, pass_fds=admission.pass_fds)
        emit("starting", args.instance, engine=args.engine, model=model,
             endpoint=f"http://127.0.0.1:{port}/v1/audio/speech", pid=process.pid,
             process_group=process.pid, ready=False, warm=False)
        result = supervise(process, commands, args.instance, stop_signal,
                           on_prepared=admission.release_initialization)
        emit("stopped", args.instance, **result)
        return 0 if result["reason"] in {"owner_stop", "owner_eof", "owner_signal"} else 1
    except CleanupFailed:
        emit("cleanup_failed", args.instance, process_group=process.pid,
             linux_cleanup_confirmed=False, process_started=True,
             ready=False, warm=False)
        return 1
    except (OSError, EOFError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        # Only fixed diagnostic reasons: never print bootstrap/environment/argv.
        if process is not None:
            try:
                stop_owned(process)
            except (CleanupFailed, OSError, subprocess.SubprocessError):
                emit("cleanup_failed", args.instance, process_group=process.pid,
                     linux_cleanup_confirmed=False, process_started=True,
                     ready=False, warm=False)
                return 1
        # No child was created, or stop_owned above verified its entire group.
        # This proof is independent of the failure itself; clients must not
        # infer successful cleanup from an arbitrary failed/bridge-exit event.
        emit("failed", args.instance, error_type=type(error).__name__,
             linux_cleanup_confirmed=True, process_started=process is not None,
             ready=False, warm=False)
        return 1
    finally:
        commands.close()
        if admission is not None:
            admission.close()
        elif lease is not None:
            os.close(lease)


if __name__ == "__main__":
    raise SystemExit(main())
