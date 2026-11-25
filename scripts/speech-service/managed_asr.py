"""Owned CPU ASR entry point. Rust owns the child, devices and retry policy.

No descriptor files, arbitrary commands, downloads, microphone or saved audio.
The parent must attach its process ownership before releasing the stdin gate.
"""
import argparse
import json
import os
from pathlib import Path
import re
import sys
import threading
import time

from asr_service import Handler, Recognition, RecognitionServer as ManagedServer

MODEL = "sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20"
MODEL_BY_BACKEND = {
    "zipformer": MODEL,
    "paraformer": "sherpa-onnx-streaming-paraformer-bilingual-zh-en-8e40c43",
}
MAX_LINE = 4096


def load_engine(model, backend="zipformer"):
    from recognizer import Paraformer, Zipformer
    if backend == "zipformer":
        return Zipformer(model, threads=2, warmup=False)
    if backend == "paraformer":
        return Paraformer(model, threads=2, warmup=False)
    raise ValueError("Unsupported ASR backend")


class OwnerLines:
    """One reader for the private owner pipe, without a blocking Windows CRT read."""
    def __init__(self, source):
        self.source = source
        self.pending = bytearray()
        self.fd = None
        if os.name != "nt":
            return
        try:
            descriptor = source.fileno()
        except (AttributeError, OSError, ValueError):
            return  # StringIO and the fake protocol sources retain readline.
        import ctypes
        from ctypes import wintypes
        import msvcrt
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.GetFileType.argtypes = [wintypes.HANDLE]
        kernel.GetFileType.restype = wintypes.DWORD
        handle = msvcrt.get_osfhandle(descriptor)
        if kernel.GetFileType(handle) != 3:  # FILE_TYPE_PIPE
            return
        kernel.PeekNamedPipe.argtypes = [wintypes.HANDLE, wintypes.LPVOID, wintypes.DWORD,
                                        wintypes.LPDWORD, wintypes.LPDWORD, wintypes.LPDWORD]
        kernel.PeekNamedPipe.restype = wintypes.BOOL
        # No TextIO read has happened and there is only one consumer. Binary mode
        # also prevents CRT CRLF translation from waiting for a byte after '\r'.
        msvcrt.setmode(descriptor, os.O_BINARY)
        self.fd, self.handle, self.kernel = descriptor, handle, kernel

    def _available(self):
        import ctypes
        from ctypes import wintypes
        available = wintypes.DWORD()
        if not self.kernel.PeekNamedPipe(self.handle, None, 0, None,
                                        ctypes.byref(available), None):
            error = ctypes.get_last_error()
            if error in (109, 232, 233):  # broken/closed/disconnected pipe
                return None
            raise ctypes.WinError(error)
        return available.value

    def readline(self):
        if self.fd is None:
            return self.source.readline(MAX_LINE + 1)
        while True:
            end = self.pending.find(b"\n")
            if end >= 0:
                if end + 1 > MAX_LINE:
                    raise ValueError("Owner line exceeds limit")
                line = bytes(self.pending[:end + 1])
                del self.pending[:end + 1]
                return line.decode("utf-8")
            if len(self.pending) > MAX_LINE:
                raise ValueError("Owner line exceeds limit")
            available = self._available()
            if available is None:
                line = bytes(self.pending)
                self.pending.clear()
                return line.decode("utf-8")
            if available:
                # The sole consumer reads only bytes already present; no CRT
                # file-descriptor lock is held while waiting for the owner.
                data = os.read(self.fd, min(available, MAX_LINE + 1 - len(self.pending)))
                if not data:
                    line = bytes(self.pending)
                    self.pending.clear()
                    return line.decode("utf-8")
                self.pending.extend(data)
            else:
                time.sleep(0.01)


class ParentControl:
    def __init__(self, source, instance):
        self.source, self.instance = source, instance
        self.lines = OwnerLines(source)
        self.bootstrap = threading.Event()
        self.stop = threading.Event()
        self.token = None
        self.error = None

    def read(self):
        try:
            line = self.lines.readline()
            if not line:
                self.error = "owner_closed_before_bootstrap"
                return
            if len(line) > MAX_LINE or not line.endswith("\n"):
                raise ValueError()
            data = json.loads(line)
            token = data.get("bearer_token") if isinstance(data, dict) else None
            if (not isinstance(data, dict)
                    or set(data) != {"protocol_version", "instance_id", "bearer_token"}
                    or type(data["protocol_version"]) is not int or data["protocol_version"] != 1
                    or data["instance_id"] != self.instance
                    or not isinstance(token, str)
                    or not re.fullmatch(r"[A-Za-z0-9_.-]{24,256}", token)):
                raise ValueError()
            self.token = token
            self.bootstrap.set()
            while True:
                line = self.lines.readline()
                if not line:
                    self.stop.set()
                    return
                if len(line) > MAX_LINE or not line.endswith("\n"):
                    raise ValueError()
                command = json.loads(line)
                if command == {"op": "stop", "instance_id": self.instance}:
                    self.stop.set()
                    return
                # A delayed command for another lifecycle never stops this one.
        except (ValueError, UnicodeError, OSError):
            self.error = "invalid_owner_protocol"
            self.stop.set()
        finally:
            self.bootstrap.set()


def run(root, instance, source=sys.stdin, output=sys.stdout, engine_factory=None,
        bootstrap_timeout=5, *, backend="zipformer"):
    if backend not in MODEL_BY_BACKEND:
        raise ValueError("Unsupported ASR backend")
    if not re.fullmatch(r"[0-9a-f]{32}", instance):
        raise ValueError("Invalid instance identifier")
    root = Path(root).resolve()

    def emit(state, **fields):
        output.write(json.dumps({"protocol_version": 1, "state": state,
                                 "instance_id": instance, "backend": backend, **fields}) + "\n")
        output.flush()

    # This reports the actual interpreter, which can differ from the venv
    # redirector PID. The parent verifies owned Job membership on its handle;
    # it never terminates a process by looking up a PID again.
    emit("awaiting_owner", interpreter_pid=os.getpid(), ready=False, warmup_completed=False)
    control = ParentControl(source, instance)
    threading.Thread(target=control.read, name="asr-owner-input", daemon=True).start()
    if not control.bootstrap.wait(bootstrap_timeout) or not control.token or control.error:
        emit("failed", ready=False, warmup_completed=False, http_closed=True,
             reason=control.error or "owner_bootstrap_timeout")
        return 1
    if control.stop.is_set():
        emit("stopped", ready=False, warmup_completed=False, http_closed=True)
        return 0

    server, serving = None, None
    error = None
    try:
        server = ManagedServer(("127.0.0.1", 0), Handler)
        server.token, server.instance_id = control.token, instance
        server.recognition = None
        server.ready, server.warmup_completed = False, False
        serving = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.05),
                                   name="asr-inference", daemon=True)
        serving.start()
        endpoint = f"http://127.0.0.1:{server.server_port}"
        emit("loading", base_url=endpoint, ready=False, warmup_completed=False)
        model = root / "models/speech" / MODEL_BY_BACKEND[backend]
        engine = (engine_factory(model) if engine_factory is not None
                  else load_engine(model, backend=backend))
        if not control.stop.is_set():
            server.recognition = Recognition(engine)
            server.ready = True
            emit("warming", base_url=endpoint, ready=True, warmup_completed=False,
                 provider=engine.provider)
            engine.warm()
        if not control.stop.is_set():
            server.warmup_completed = True
            emit("warm_ready", base_url=endpoint, ready=True, warmup_completed=True,
                 provider=engine.provider)
            control.stop.wait()
        if control.error:
            error = control.error
    except Exception:
        # Native errors and transcripts are deliberately not copied to stdout.
        error = "local_asr_start_or_inference_failed"
    finally:
        if server is not None:
            server.ready, server.warmup_completed = False, False
            if serving is not None:
                server.shutdown()
                serving.join()
            server.server_close()
            server.recognition = None
        # This is an HTTP cleanup receipt, not proof that this process exited.
        # Rust must still observe the owned interpreter/launcher termination.
        emit("failed" if error else "stopped", ready=False, warmup_completed=False,
             http_closed=True, **({"reason": error} if error else {}))
    return 1 if error else 0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--instance", required=True)
    parser.add_argument("--backend", choices=tuple(MODEL_BY_BACKEND), default="zipformer")
    args = parser.parse_args()
    return run(args.root, args.instance, backend=args.backend)


if __name__ == "__main__":
    raise SystemExit(main())
