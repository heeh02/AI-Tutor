"""Explicit Windows diagnostic for one owned WSL TTS service, without playback.

Starting, authenticated readiness, bilingual synthetic preparation and confirmed
shutdown are separate milestones. This script never installs a production voice
or stops an existing lab service. The caller must arrange exclusive GPU use.
"""
import argparse
import json
import os
from pathlib import Path, PureWindowsPath
import queue
import secrets
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import uuid


ENGINES = {
    "qwen": (18868, "tutor-qwen-tts", "runtime/speech-gpu/omni-api-key"),
    "cosy": (18869, "tutor-cosy-tts", "runtime/speech-cosy/api-key"),
}
REFERENCE = "runtime/goal-baselines/20261104/cosyvoice3-preparation/reference-audio.json"
READY_SECONDS = 240
STOP_SECONDS = 20
PREPARE_SECONDS = 150  # Rust owns the inner 120-second preparation deadline.
STDERR_LIMIT = 32 * 1024 * 1024


class ProbeFailure(RuntimeError):
    """Messages here are fixed diagnostics, never raw child/HTTP output."""


def wrapper_event(line, instance, engine):
    """Only validated, whitelisted protocol fields can enter the report."""
    try:
        value = json.loads(line)
    except (ValueError, UnicodeError) as error:
        raise ProbeFailure("wrapper_invalid_json") from error
    if (not isinstance(value, dict) or type(value.get("protocol_version")) is not int or value.get("protocol_version") != 1
            or value.get("instance_id") != instance):
        raise ProbeFailure("wrapper_wrong_owner_or_protocol")
    state = value.get("state")
    if state not in {"starting", "stopped", "failed", "cleanup_failed"}:
        raise ProbeFailure("wrapper_unknown_state")
    result = {"protocol_version": 1, "instance_id": instance, "state": state}
    if state == "starting":
        port, model, _ = ENGINES[engine]
        endpoint = f"http://127.0.0.1:{port}/v1/audio/speech"
        if (value.get("engine") != engine or value.get("model") != model
                or value.get("endpoint") != endpoint
                or value.get("ready") is not False or value.get("warm") is not False):
            raise ProbeFailure("wrapper_starting_contract_mismatch")
        result.update(engine=engine, model=model, endpoint=endpoint, ready=False, warm=False)
    elif state == "stopped":
        if (value.get("reason") not in {"owner_stop", "owner_eof", "owner_signal",
                                       "service_exited", "control_error"}
                or type(value.get("service_exit_code")) is not int
                or type(value.get("forced_group_stop")) is not bool):
            raise ProbeFailure("wrapper_stopped_contract_mismatch")
        result.update(reason=value["reason"], service_exit_code=value["service_exit_code"],
                      forced_group_stop=value["forced_group_stop"])
    else:
        for field in ("ready", "warm", "linux_cleanup_confirmed", "process_started"):
            if type(value.get(field)) is bool:
                result[field] = value[field]
        if value.get("error_type") in {"OSError", "EOFError", "ValueError", "RuntimeError",
                                       "BlockingIOError", "CalledProcessError", "TimeoutExpired"}:
            result["error_type"] = value["error_type"]
    return result


def preparation_receipt(text, engine):
    try:
        value = json.loads(text)
    except (ValueError, UnicodeError) as error:
        raise ProbeFailure("prepare_invalid_json") from error
    if (not isinstance(value, dict)
            or value.get("test_type") != "rust_local_tts_prepare_bilingual_synthetic"
            or value.get("engine") != engine):
        raise ProbeFailure("prepare_wrong_contract")
    receipt = value.get("receipt")
    languages = receipt.get("languages") if isinstance(receipt, dict) else None
    status = value.get("status")
    if (value.get("success") is not True or value.get("outcome") != "completed"
            or not isinstance(status, dict)
            or status.get("phase") != "warm_ready_pending_install"
            or not isinstance(languages, list) or len(languages) != 2
            or any(not isinstance(row, dict) for row in languages)
            or [row.get("language") for row in languages] != ["zh", "en"]
            or any(type(row.get("samples")) is not int or row["samples"] <= 0
                   for row in languages)
            or any(value.get(key) is not False for key in
                   ("client_installed", "speaker_output", "microphone_opened", "played"))
            or value.get("private_fixture_cleaned") is not True):
        raise ProbeFailure("prepare_did_not_confirm_synthetic_only_success")
    return value


def stdout_reader(stream, events, instance, engine):
    try:
        while True:
            line = stream.readline(8193)
            if not line:
                break
            if len(line) > 8192:
                raise ProbeFailure("wrapper_line_too_large")
            events.put(("event", wrapper_event(line, instance, engine)))
    except (OSError, ProbeFailure) as error:
        events.put(("error", str(error) if isinstance(error, ProbeFailure)
                    else "wrapper_stdout_read_failed"))
    finally:
        events.put(("eof", None))


def stderr_reader(stream, path, token, events):
    log, written = None, 0
    try:
        log = path.open("xb")
    except OSError:
        events.put(("error", "wrapper_stderr_log_failed"))
    pending, secret = b"", token.encode("ascii")
    try:
        while True:
            chunk = stream.read1(65536)
            pending = (pending + chunk).replace(secret, b"[REDACTED]")
            # Retain a possible split token suffix across pipe reads.
            if chunk:
                output, pending = pending[:-len(secret)], pending[-len(secret):]
            else:
                output, pending = pending, b""
            if log is not None:
                try:
                    if written + len(output) > STDERR_LIMIT:
                        events.put(("error", "wrapper_stderr_log_limit_reached"))
                        log.close()
                        log = None
                    else:
                        log.write(output)
                        log.flush()
                        written += len(output)
                except OSError:
                    events.put(("error", "wrapper_stderr_log_failed"))
                    try:
                        log.close()
                    except OSError:
                        pass
                    log = None
            # Keep draining after a disk failure so cleanup cannot block on logs.
            if not chunk:
                break
    except OSError:
        events.put(("error", "wrapper_stderr_read_failed"))
    finally:
        if log is not None:
            try:
                log.close()
            except OSError:
                events.put(("error", "wrapper_stderr_log_failed"))


def collect_events(events, report, started):
    while True:
        try:
            kind, item = events.get_nowait()
        except queue.Empty:
            return
        if kind == "event":
            item["observed_ms"] = round((time.monotonic() - started) * 1000, 2)
            if item["state"] == "starting":
                report["starting"] = True
            report["wrapper_events"].append(item)
        elif kind == "error":
            report["errors"].append(item)


def require_running(process, events, report, started):
    collect_events(events, report, started)
    if report["errors"]:
        raise ProbeFailure("wrapper_protocol_or_log_failure")
    if any(item["state"] != "starting" for item in report["wrapper_events"]):
        raise ProbeFailure("wrapper_terminated_before_preparation_completed")
    if process.poll() is not None:
        raise ProbeFailure("wrapper_exited_before_preparation_completed")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None  # Never forward this instance's credential to another URL.


def models_get(opener, url, token, timeout):
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    request = urllib.request.Request(url, headers=headers)
    try:
        with opener.open(request, timeout=timeout) as response:
            body = response.read(65537)
            if len(body) > 65536:
                raise ProbeFailure("models_response_too_large")
            return response.status, body
    except urllib.error.HTTPError as error:
        code = error.code
        error.close()
        return code, b""
    except (urllib.error.URLError, TimeoutError, OSError):
        return None, b""


def wait_ready(process, events, report, started, engine, token):
    deadline = started + READY_SECONDS
    while time.monotonic() < deadline:
        require_running(process, events, report, started)
        if report["wrapper_events"]:
            report["starting"] = True
            break
        time.sleep(.05)
    else:
        raise ProbeFailure("starting_or_readiness_deadline_exceeded")
    port, model, _ = ENGINES[engine]
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    url = f"http://127.0.0.1:{port}/v1/models"
    while time.monotonic() < deadline:
        require_running(process, events, report, started)
        status, body = models_get(opener, url, token, min(2, deadline - time.monotonic()))
        report["authentication"]["authorized_http_status"] = status
        if status == 200:
            try:
                data = json.loads(body)
                models = [row.get("id") for row in data.get("data", [])]
            except (ValueError, AttributeError, TypeError):
                raise ProbeFailure("models_invalid_json") from None
            if model not in models:
                raise ProbeFailure("models_expected_id_missing")
            report["authentication"]["expected_model_present"] = True
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            absent, _ = models_get(opener, url, None, min(2, remaining))
            report["authentication"]["unauthenticated_http_status"] = absent
            if absent != 401:
                raise ProbeFailure("models_without_credential_was_not_401")
            require_running(process, events, report, started)
            report["ready"] = True
            report["ready_ms"] = round((time.monotonic() - started) * 1000, 2)
            return
        if status in {401, 403}:
            raise ProbeFailure("fresh_instance_credential_rejected")
        time.sleep(min(.4, max(0, deadline - time.monotonic())))
    raise ProbeFailure("starting_or_readiness_deadline_exceeded")


def write_private_fixture(output, root, engine, token):
    fixture = output / "fixture"
    fixture.mkdir()
    credential = fixture / ENGINES[engine][2]
    credential.parent.mkdir(parents=True)
    with credential.open("x", encoding="utf-8") as file:
        file.write(token)
    if engine == "cosy":
        original = (root / REFERENCE).read_bytes()
        manifest = json.loads(original)
        rows = {row.get("id"): row for row in manifest["samples"]}
        for name in ("zh_concept", "en_concept"):
            if not PureWindowsPath(rows[name]["path"]).is_absolute():
                raise ProbeFailure("reference_audio_requires_original_absolute_windows_paths")
        destination = fixture / REFERENCE
        destination.parent.mkdir(parents=True)
        with destination.open("xb") as file:
            file.write(original)  # Preserve the manifest and original source paths verbatim.
    return fixture


def cleanup_credentials(fixture, engine, token):
    """Delete exact owned credential files, including an interrupted Rust fixture.

    The outer fixture directory was exclusively created by this invocation. No
    recursive delete, production key, policy, reference audio or report is used.
    """
    removed = 0
    if not fixture.exists():
        return removed
    candidates = [fixture / ENGINES[engine][2]]
    candidates.extend((fixture / "runtime").glob("tts-probe-*/runtime/tts-service.json"))
    for path in candidates:
        if not path.exists():
            continue
        if path.is_symlink() or not path.resolve().is_relative_to(fixture.resolve()):
            raise ProbeFailure("credential_cleanup_path_not_owned")
        text = path.read_text(encoding="utf-8")
        matches = (text == token if path == candidates[0]
                   else json.loads(text).get("bearer_token") == token)
        if not matches:
            raise ProbeFailure("credential_cleanup_content_changed")
        path.unlink()
        removed += 1
    return removed


def run_preparation(root, fixture, output, engine, token, report):
    command = [str(root / "target/release/tts-stream-probe.exe"),
               "--real-local-synthetic", "--root", str(fixture),
               "--prepare-bilingual", "--engine", engine]
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
    try:
        stdout, stderr = process.communicate(timeout=PREPARE_SECONDS)
    except (subprocess.TimeoutExpired, KeyboardInterrupt):
        report["prepare_interrupted"] = True
        process.terminate()  # This exact diagnostic child, never a model or discovered PID.
        try:
            stdout, stderr = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            stdout, stderr = process.communicate(timeout=5)
    report["prepare_exit_code"] = process.returncode
    for name, data in (("prepare.stdout.json", stdout), ("prepare.stderr.log", stderr)):
        (output / name).write_text(data.decode("utf-8", errors="replace")
                                  .replace(token, "[REDACTED]"), encoding="utf-8")
    if process.returncode != 0 or report.get("prepare_interrupted"):
        raise ProbeFailure("rust_preparation_failed_or_interrupted")
    report["preparation"] = preparation_receipt(stdout, engine)
    report["warm"] = True


def stop_wrapper(process, events, readers, instance, report, started):
    try:
        if process.poll() is None:
            process.stdin.write((json.dumps({"op": "stop", "instance_id": instance}) + "\n").encode())
            process.stdin.flush()
            report["stop_command_sent"] = True
    except (BrokenPipeError, OSError):
        report["stop_command_sent"] = False
    finally:
        try:
            process.stdin.close()
        except (BrokenPipeError, OSError):
            pass
    deadline = time.monotonic() + STOP_SECONDS
    while process.poll() is None and time.monotonic() < deadline:
        collect_events(events, report, started)
        time.sleep(.05)
    if process.poll() is None:
        report["errors"].append("wsl_bridge_stop_timeout_linux_cleanup_unconfirmed")
        report["bridge_terminated_only"] = True
        process.terminate()  # Only this Popen bridge; this does NOT prove Linux cleanup.
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            report["errors"].append("owned_wsl_bridge_did_not_exit")
    report["bridge_exit_code"] = process.poll()
    for reader in readers:
        reader.join(timeout=1)
    collect_events(events, report, started)
    terminal = [item for item in report["wrapper_events"] if item["state"] != "starting"]
    stopped = len(terminal) == 1 and terminal[0]["state"] == "stopped"
    proved_failure_cleanup = (len(terminal) == 1 and terminal[0]["state"] == "failed"
                              and terminal[0].get("ready") is False and terminal[0].get("warm") is False
                              and terminal[0].get("linux_cleanup_confirmed") is True
                              and type(terminal[0].get("process_started")) is bool)
    report["linux_cleanup_confirmed"] = bool((stopped or proved_failure_cleanup) and process.poll() is not None
                                             and not report["bridge_terminated_only"])
    report["stopped"] = report["linux_cleanup_confirmed"]
    if not report["stopped"]:
        report["errors"].append("matching_stopped_and_bridge_exit_not_confirmed")
    elif not stopped or terminal[0].get("reason") not in {"owner_stop", "owner_eof"} or process.returncode != 0:
        report["errors"].append("wrapper_did_not_finish_normal_owner_shutdown")
    if any(reader.is_alive() for reader in readers):
        report["errors"].append("wrapper_log_reader_did_not_finish")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real-local-synthetic", action="store_true", required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--engine", choices=ENGINES, required=True)
    parser.add_argument("--output", type=Path, required=True, help="A new, nonexistent directory")
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("This diagnostic owns a Windows wsl.exe bridge; run it on Windows")
    root, output = args.root.resolve(), args.output.resolve()
    if not (root / "target/release/tts-stream-probe.exe").is_file():
        parser.error("Build target/release/tts-stream-probe.exe before this explicit diagnostic")
    output.mkdir(parents=True, exist_ok=False)
    instance, token = uuid.uuid4().hex, secrets.token_urlsafe(32)
    report = {"protocol_version": 1, "test_type": "windows_managed_tts_synthetic",
              "engine": args.engine, "instance_id": instance, "success": False,
              "starting": False, "ready": False, "warm": False, "stopped": False,
              "linux_cleanup_confirmed": False, "bridge_terminated_only": False,
              "client_installed": False, "played": False, "microphone_opened": False,
              "starting_and_readiness_deadline_seconds": READY_SECONDS,
              "stop_command_sent": False, "authentication": {}, "wrapper_events": [],
              "prepare_exit_code": None, "errors": []}
    process, readers, events = None, [], queue.Queue()
    fixture = output / "fixture"
    started = time.monotonic()
    try:
        write_private_fixture(output, root, args.engine, token)
        converted = subprocess.run(["wsl.exe", "-d", "Ubuntu", "--", "wslpath", "-u", str(root)],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, timeout=15, check=True,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        wsl_root = converted.stdout.decode("utf-8").strip()
        if not wsl_root.startswith("/") or "\n" in wsl_root:
            raise ProbeFailure("wsl_root_conversion_failed")
        process = subprocess.Popen(
            ["wsl.exe", "-d", "Ubuntu", "--", "python3",
             wsl_root.rstrip("/") + "/scripts/speech-lab/managed_tts.py",
             "--engine", args.engine, "--instance", instance],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            creationflags=subprocess.CREATE_NO_WINDOW)
        for target, params in (
                (stdout_reader, (process.stdout, events, instance, args.engine)),
                (stderr_reader, (process.stderr, output / "wrapper.stderr.log", token, events))):
            reader = threading.Thread(target=target, args=params, daemon=True)
            reader.start()
            readers.append(reader)
        process.stdin.write((json.dumps({"protocol_version": 1, "bearer_token": token}) + "\n").encode())
        process.stdin.flush()
        wait_ready(process, events, report, started, args.engine, token)
        run_preparation(root, fixture, output, args.engine, token, report)
        require_running(process, events, report, started)
    except ProbeFailure as error:
        report["errors"].append(str(error))
    except KeyboardInterrupt:
        report["errors"].append("diagnostic_interrupted")
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        report["errors"].append("diagnostic_" + type(error).__name__)
    finally:
        if process is not None:
            try:
                stop_wrapper(process, events, readers, instance, report, started)
            except (OSError, ValueError, KeyboardInterrupt) as error:
                report["errors"].append("wrapper_cleanup_" + type(error).__name__)
        try:
            report["credential_files_removed"] = cleanup_credentials(fixture, args.engine, token)
            report["credential_cleanup_complete"] = True
        except (OSError, ValueError, ProbeFailure) as error:
            report["credential_cleanup_complete"] = False
            report["errors"].append(str(error) if isinstance(error, ProbeFailure)
                                    else "credential_cleanup_failed")
        report["total_ms"] = round((time.monotonic() - started) * 1000, 2)
        report["success"] = (not report["errors"] and report["ready"] and report["warm"]
                             and report["stopped"] and report["credential_cleanup_complete"])
        path = output / "report.json"
        path.write_text(json.dumps(report, ensure_ascii=False, indent=2)
                        .replace(token, "[REDACTED]") + "\n", encoding="utf-8")
    print(json.dumps({"success": report["success"], "report": str(path),
                      "starting": report["starting"], "ready": report["ready"],
                      "warm": report["warm"], "stopped": report["stopped"]}))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
