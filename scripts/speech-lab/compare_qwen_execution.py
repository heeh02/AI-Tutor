"""Compare two already-created Qwen fixtures, without devices or production writes."""
import argparse
import hashlib
import http.client
import json
import math
from pathlib import Path
import queue
import secrets
import shutil
import subprocess
import threading
import time
import uuid
import wave

from probe_managed_tts import (ProbeFailure, require_running, stderr_reader,
                               stdout_reader, stop_wrapper, wait_ready)

ARMS = {
    "graph": ("main-managed-qwen-first", "a79c0b71f391363df7a3dde0c4d5d6386315dfde997f7940449c96ddb451b9ea"),
    "eager": ("main-managed-qwen-eager-first", "3b44527b8d33d311fda428efcc7d45937462bb47b286325afcf25908775661c4"),
}
ORDER = ("graph", "eager", "eager", "graph")


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixtures(root):
    result = {}
    for arm, (directory, expected) in ARMS.items():
        source = root / "runtime/goal-baselines/20261104" / directory
        previous = json.loads((source / "report.json").read_text(encoding="utf-8"))
        assert previous["success"] and previous["main_exit_code"] == 0
        fixture = source / "fixture"
        assert not (fixture / "runtime/local-speech-policy.json").exists()
        for name, sha in previous["bundle_sha256"].items():
            assert digest(fixture / "bin/local-speech" / name) == sha
        assert digest(fixture / "bin/local-speech/qwen3_tts_12gb.yaml") == expected
        # Both previously verified fixtures retain hard links to the same fixed
        # local model files. This test never downloads or rewrites them.
        for relative in previous["model_files_hard_linked"]:
            assert (fixture / "models" / relative).samefile(root / "models" / relative)
        result[arm] = fixture
    return result


def synthesize(item, seed, token, output):
    row = {"id": item["id"], "language": item["language"], "seed": seed, "success": False}
    body = {"model": "tutor-qwen-tts", "input": item["text"], "stream": True,
            "stream_format": "audio", "response_format": "pcm", "seed": seed,
            "voice": "vivian" if item["language"] == "zh" else "ryan",
            "language": "Auto" if item["id"].startswith("mixed_") else
                        ("Chinese" if item["language"] == "zh" else "English")}
    connection = http.client.HTTPConnection("127.0.0.1", 18868, timeout=30)
    pcm, chunks = bytearray(), []
    started = time.perf_counter()
    try:
        connection.request("POST", "/v1/audio/speech", json.dumps(body),
                           {"Content-Type": "application/json", "Authorization": "Bearer " + token})
        response = connection.getresponse()
        row["http_status"] = response.status
        if response.status != 200:
            raise ProbeFailure("synthesis_http_rejected")
        content_type = response.getheader("Content-Type", "").split(";", 1)[0].strip().lower()
        row["content_type"] = content_type
        if content_type not in {"audio/pcm", "application/octet-stream"}:
            raise ProbeFailure("unexpected_pcm_content_type")
        while chunk := response.read1(8192):
            at_ms = (time.perf_counter() - started) * 1000
            if at_ms > 120000 or len(pcm) + len(chunk) > 24000 * 2 * 180:
                raise ProbeFailure("synthesis_budget_exceeded")
            chunks.append({"at_ms": at_ms, "bytes": len(chunk)})
            pcm.extend(chunk)
        if not pcm or len(pcm) % 2:
            raise ProbeFailure("empty_or_misaligned_pcm")
        if (time.perf_counter() - started) * 1000 > 120000:
            raise ProbeFailure("synthesis_completion_budget_exceeded")
        row["success"] = True
    except Exception as error:
        row["error"] = str(error) if isinstance(error, ProbeFailure) else type(error).__name__
    finally:
        connection.close()
    elapsed = (time.perf_counter() - started) * 1000
    row.update(generation_ms=elapsed, audio_ms=len(pcm) / 48,
               first_pcm_bytes_ms=chunks[0]["at_ms"] if chunks else None,
               rtf=elapsed / (len(pcm) / 48) if pcm else None,
               max_interchunk_gap_ms=max((b["at_ms"] - a["at_ms"] for a, b in zip(chunks, chunks[1:])), default=None),
               chunks=chunks, pcm_sha256=hashlib.sha256(pcm).hexdigest())
    if pcm and len(pcm) % 2 == 0:
        filename = item["id"] + (".wav" if row["success"] else ".incomplete.wav")
        with wave.open(str(output / filename), "wb") as wav:
            wav.setparams((1, 2, 24000, 0, "NONE", "not compressed"))
            wav.writeframes(pcm)
        row["wav"] = filename
    return row


def run_arm(fixture, bundle, output, arm, corpus):
    output.mkdir()
    instance, token = uuid.uuid4().hex, secrets.token_urlsafe(32)
    report = {"arm": arm, "success": False, "starting": False, "ready": False,
              "warm": False, "stopped": False, "wrapper_events": [], "authentication": {},
              "errors": [], "bridge_terminated_only": False, "warmup": [], "rows": [],
              "speaker_output": False, "microphone_opened": False, "gpu_samples": []}
    process, readers, events = None, [], queue.Queue()
    monitor_stop = threading.Event()

    def monitor():
        while not monitor_stop.is_set():
            try:
                result = subprocess.run(["nvidia-smi", "--query-gpu=memory.used,utilization.gpu",
                                         "--format=csv,noheader,nounits"], capture_output=True,
                                        text=True, timeout=3, check=True,
                                        creationflags=subprocess.CREATE_NO_WINDOW)
                used, load = map(int, result.stdout.strip().splitlines()[0].split(","))
                report["gpu_samples"].append({"at_ms": (time.perf_counter()-timer)*1000,
                                               "memory_mib": used, "gpu_percent": load})
            except (OSError, ValueError, subprocess.SubprocessError):
                pass
            monitor_stop.wait(.5)

    started, timer = time.monotonic(), time.perf_counter()
    monitor_thread = threading.Thread(target=monitor, daemon=True)
    monitor_thread.start()
    try:
        converted = subprocess.run(["wsl.exe", "-d", "Ubuntu", "--", "wslpath", "-u", str(fixture)],
                                   capture_output=True, timeout=15, check=True,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        linux_root = converted.stdout.decode("utf-8").strip()
        if not linux_root.startswith("/") or "\n" in linux_root:
            raise ProbeFailure("fixture_path_conversion_failed")
        converted_bundle = subprocess.run(["wsl.exe", "-d", "Ubuntu", "--", "wslpath", "-u", str(bundle)],
                                          capture_output=True, timeout=15, check=True,
                                          creationflags=subprocess.CREATE_NO_WINDOW)
        linux_bundle = converted_bundle.stdout.decode("utf-8").strip()
        if not linux_bundle.startswith("/") or "\n" in linux_bundle:
            raise ProbeFailure("bundle_path_conversion_failed")
        process = subprocess.Popen(["wsl.exe", "-d", "Ubuntu", "--", "python3",
                                    linux_bundle + "/managed_tts.py", "--root", linux_root,
                                    "--engine", "qwen", "--instance", instance],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        for target, params in (
            (stdout_reader, (process.stdout, events, instance, "qwen")),
            (stderr_reader, (process.stderr, output / "wrapper.stderr.log", token, events)),
        ):
            reader = threading.Thread(target=target, args=params, daemon=True)
            reader.start()
            readers.append(reader)
        process.stdin.write((json.dumps({"protocol_version": 1, "bearer_token": token})+"\n").encode())
        process.stdin.flush()
        wait_ready(process, events, report, started, "qwen", token)
        warm_output = output / "warmup"
        warm_output.mkdir()
        for index, item in enumerate(corpus[:2]):
            row = synthesize(item, 9900 + index, token, warm_output)
            report["warmup"].append(row)
            save(output / "report.json", report)
            if not row["success"]:
                raise ProbeFailure("fixed_warmup_failed")
        report["warm"] = True
        for index, item in enumerate(corpus):
            require_running(process, events, report, started)
            row = synthesize(item, 12100 + index, token, output)
            report["rows"].append(row)
            save(output / "report.json", report)
            if not row["success"]:
                raise ProbeFailure("measured_synthesis_failed")
    except KeyboardInterrupt:
        report["errors"].append("diagnostic_interrupted")
    except Exception as error:
        report["errors"].append(str(error) if isinstance(error, ProbeFailure) else type(error).__name__)
    finally:
        try:
            if process is not None:
                stop_wrapper(process, events, readers, instance, report, started)
        except (Exception, KeyboardInterrupt) as error:
            report["errors"].append("cleanup_" + type(error).__name__)
            report["linux_cleanup_confirmed"] = False
        finally:
            monitor_stop.set()
            monitor_thread.join(timeout=4)
            report["peak_sampled_total_vram_mib"] = max((s["memory_mib"] for s in report["gpu_samples"]), default=None)
            report["total_ms"] = (time.perf_counter() - timer) * 1000
            report["success"] = not report["errors"] and report["warm"] and report["stopped"] and len(report["rows"]) == len(corpus)
            save(output / "report.json", report)
    return report


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)] if ordered else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real-local-synthetic", action="store_true", required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root, output = args.root.resolve(), args.output.resolve()
    arms = fixtures(root)
    corpus_path = root / "scripts/speech-lab/education_speech_v1.json"
    corpus = json.loads(corpus_path.read_text(encoding="utf-8"))["samples"]
    assert len(corpus) == 12 and [x["id"] for x in corpus[:2]] == ["zh_short", "en_short"]
    output.mkdir(parents=True, exist_ok=False)
    bundles, bundle_hashes = {}, {}
    for arm, fixture in arms.items():
        bundle = output / ("bundle-" + arm)
        shutil.copytree(fixture / "bin/local-speech", bundle)
        # Preserve prior fixture assets and reports. Only this new diagnostic
        # bundle uses the TIME_WAIT correction; both arms use identical helpers.
        for name in ("managed_tts.py", "tts_gpu_lease.sh"):
            shutil.copyfile(root / "scripts/speech-lab" / name, bundle / name)
        bundles[arm] = bundle
        bundle_hashes[arm] = {file.name: digest(file) for file in bundle.iterdir() if file.is_file()}
    method = {"order": ORDER, "measured_samples_per_run": 12, "runs_per_arm": 2,
              "warmup_ids": [x["id"] for x in corpus[:2]], "warmup_seeds": [9900, 9901],
              "measured_seeds": list(range(12100, 12112)), "corpus_sha256": digest(corpus_path),
              "configuration_sha256": {k: v[1] for k,v in ARMS.items()},
              "bundle_program_sha256": bundle_hashes,
              "bundle_scope": "Fresh copied program bundles with identical TIME_WAIT-corrected owner/lease helpers; original fixture assets unchanged",
              "percentile": "nearest rank", "sample_rate": 24000,
              "cache_state": "Reuse each arm's previous synthetic fixture cache; this is not fresh-cache startup comparison",
              "limits": "Two repeated synthetic runs per arm are diagnostic, not independent human/naturalness or device evidence; ABBA does not isolate all OS/GPU cache effects; fixed seeds do not guarantee identical waveform across execution modes",
              "production_changed": False, "speaker_output_measured": False}
    save(output / "method.json", method)
    report = {"method_sha256": digest(output / "method.json"), "success": False, "runs": []}
    for index, arm in enumerate(ORDER):
        value = run_arm(arms[arm], bundles[arm], output / f"{index+1}-{arm}", arm, corpus)
        report["runs"].append({"arm": arm, "path": f"{index+1}-{arm}/report.json", "success": value["success"]})
        save(output / "report.json", report)
        print(json.dumps({"run": index+1, "arm": arm, "success": value["success"]}), flush=True)
        if not value["success"]:
            return 1
    report["aggregate"] = {}
    for arm in ARMS:
        rows = []
        for run in report["runs"]:
            if run["arm"] == arm:
                rows.extend(json.loads((output/run["path"]).read_text(encoding="utf-8"))["rows"])
        report["aggregate"][arm] = {"count": len(rows), "languages": {}}
        for language in ("zh", "en"):
            selected = [r for r in rows if r["language"] == language]
            report["aggregate"][arm]["languages"][language] = {
                "count": len(selected),
                "first_pcm_p50_ms": percentile([r["first_pcm_bytes_ms"] for r in selected], .5),
                "first_pcm_p95_ms": percentile([r["first_pcm_bytes_ms"] for r in selected], .95),
                "max_rtf": max(r["rtf"] for r in selected),
                "max_interchunk_gap_ms": max(r["max_interchunk_gap_ms"] or 0 for r in selected)}
    report["success"] = True
    save(output / "report.json", report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
