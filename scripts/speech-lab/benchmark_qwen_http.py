"""Measure first PCM bytes and streaming gaps; never opens an audio device."""
import argparse
import base64
import http.client
import hashlib
import json
import os
from pathlib import Path
import subprocess
import threading
import time
import wave


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--limit", type=int, default=12)
    parser.add_argument("--engine", choices=["qwen", "cosy"], default="qwen")
    parser.add_argument("--load-note", default="Background load not separately recorded")
    parser.add_argument("--service-state", choices=["cold", "warm", "unknown"], default="unknown")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    root = args.root.resolve()
    if not 1 <= args.limit <= 12:
        parser.error("limit must be 1..12")
    cosy = args.engine == "cosy"
    config = (root / "scripts/speech-lab" / ("cosyvoice3_12gb_candidate.yaml" if cosy else "qwen3_tts_12gb.yaml")).read_bytes()
    token = (root / ("runtime/speech-cosy/api-key" if cosy else "runtime/speech-gpu/omni-api-key")).read_text(encoding="utf-8").strip()
    references = {}
    if cosy:
        manifest = json.loads((root / "runtime/goal-baselines/20261104/cosyvoice3-preparation/reference-audio.json").read_text(encoding="utf-8"))
        for sample in manifest["samples"]:
            audio = Path(sample["path"]).read_bytes()
            if hashlib.sha256(audio).hexdigest() != sample["sha256"]:
                raise ValueError("Synthetic reference hash mismatch")
            references[sample["id"].split("_")[0]] = {
                **sample, "data_url": "data:audio/wav;base64," + base64.b64encode(audio).decode("ascii")}
    corpus = json.loads((root / "scripts/speech-lab/education_speech_v1.json").read_text(encoding="utf-8"))
    stop = threading.Event()
    samples = []

    def monitor():
        command = "nvidia-smi" if os.name == "nt" else "/usr/lib/wsl/lib/nvidia-smi"
        while not stop.is_set():
            try:
                result = subprocess.run([command, "--query-gpu=memory.used,utilization.gpu", "--format=csv,noheader,nounits"],
                                        capture_output=True, text=True, timeout=3, check=True)
                used, load = (int(v.strip()) for v in result.stdout.strip().splitlines()[0].split(","))
                samples.append({"at": time.time(), "memory_mib": used, "gpu_percent": load})
            except (OSError, ValueError, subprocess.SubprocessError):
                pass
            stop.wait(.25)

    thread = threading.Thread(target=monitor, daemon=True)
    thread.start()
    report = {"type": "cosyvoice3_0.5_real_local_streaming" if cosy else "qwen3_tts_0.6_customvoice_real_local_streaming", "speaker_output": False,
              "audible_latency_measured": False, "sample_rate": 24000,
              "model_revision": "29e01c4e8d000f4bcd70751be16fa94bf3d85a18" if cosy else "85e237c12c027371202489a0ec509ded67b5e4b5",
              "deploy_config_sha256": hashlib.sha256(config).hexdigest(),
              "deploy_config_snapshot": config.decode("utf-8"),
              "service_state_declared": args.service_state,
              "load_note": args.load_note,
              "notes": "First byte measures PCM availability, not speaker latency or naturalness; GPU sampling includes other desktop processes.",
              "rows": []}

    def save():
        report["gpu_samples"] = samples.copy()
        report["peak_sampled_total_vram_mib"] = max((s["memory_mib"] for s in samples), default=None)
        (args.output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")

    try:
        for item in corpus["samples"][:args.limit]:
            connection = http.client.HTTPConnection("127.0.0.1", 18869 if cosy else 18868, timeout=120)
            body = dict(model="tutor-cosy-tts" if cosy else "tutor-qwen-tts", input=item["text"],
                        stream=True, stream_format="audio", response_format="pcm")
            row = {**item, "first_in_report": not report["rows"], "success": False}
            if cosy:
                reference = references[item["language"]]
                body.update(ref_audio=reference["data_url"], ref_text=reference["text"])
                row.update(voice="synthetic_sapi_reference", reference_id=reference["id"],
                           reference_sha256=reference["sha256"], reference_naturalness_evidence=False)
            else:
                body.update(voice="vivian" if item["language"] == "zh" else "ryan",
                            language="Auto" if item["id"].startswith("mixed_") else ("Chinese" if item["language"] == "zh" else "English"))
                row["voice"] = body["voice"]
            chunks, pcm = [], bytearray()
            start = time.perf_counter()
            try:
                connection.request("POST", "/v1/audio/speech", json.dumps(body),
                                   {"Content-Type": "application/json", "Authorization": "Bearer " + token})
                response = connection.getresponse()
                row["http_status"] = response.status
                if response.status != 200:
                    row["error"] = response.read(4096).decode("utf-8", "replace")
                else:
                    while chunk := response.read1(8192):
                        chunks.append({"at_ms": (time.perf_counter() - start) * 1000, "bytes": len(chunk)})
                        pcm.extend(chunk)
                        if len(pcm) > 24000 * 2 * 180:
                            raise ValueError("Generated audio exceeds three-minute lab limit")
                    if not pcm or len(pcm) % 2:
                        raise ValueError("Empty or misaligned PCM stream")
                    row["success"] = True
            except (OSError, ValueError, http.client.HTTPException) as error:
                row["error"] = str(error)
            finally:
                connection.close()
            row.update(generation_ms=(time.perf_counter() - start) * 1000,
                       first_pcm_bytes_ms=chunks[0]["at_ms"] if chunks else None,
                       audio_ms=len(pcm) / 48, chunks=chunks,
                       max_interchunk_gap_ms=max((b["at_ms"] - a["at_ms"] for a, b in zip(chunks, chunks[1:])), default=None))
            if pcm and len(pcm) % 2 == 0:
                name = item["id"] + (".wav" if row["success"] else ".incomplete.wav")
                with wave.open(str(args.output / name), "wb") as wav:
                    wav.setparams((1, 2, 24000, 0, "NONE", "not compressed"))
                    wav.writeframes(pcm)
                row["wav"] = name
            report["rows"].append(row)
            save()
            print(json.dumps({k: row[k] for k in ["id", "success", "first_pcm_bytes_ms", "generation_ms", "audio_ms"]}), flush=True)
            if not row["success"]:
                break
    finally:
        stop.set()
        thread.join(timeout=4)
        save()
    if len(report["rows"]) != args.limit or any(not row["success"] for row in report["rows"]):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
