import argparse
import base64
import io
import json
import os
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[2]


def request(port, token, path, payload=None, timeout=300):
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    headers = {"Authorization": "Bearer " + token}
    if data:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=data, headers=headers)
    with urllib.request.urlopen(req, timeout=timeout) as response:
        body = response.read()
        content_type = response.headers.get("content-type", "")
        return (response.headers, body) if content_type.startswith("audio/") else (response.headers, json.loads(body))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=18870)
    parser.add_argument("--checks", default="text,image,speech")
    args = parser.parse_args()
    checks = set(args.checks.split(","))
    token = uuid.uuid4().hex + uuid.uuid4().hex
    environment = os.environ.copy()
    environment.update({
        "AI_TUTOR_OMNI_TOKEN": token, "AI_TUTOR_OMNI_ATTENTION": "sdpa",
        "HF_HUB_OFFLINE": "1", "TRANSFORMERS_OFFLINE": "1", "PYTHONUTF8": "1",
        "AI_TUTOR_OMNI_DEBUG": "1",
    })
    log_path = ROOT / "runtime" / "omni-service-probe.log"
    result = {"model": "Qwen2.5-Omni-7B-GPTQ-Int4", "checks": {}, "speaker_output_measured": False, "physical_microphone_used": False}
    with log_path.open("wb") as log:
        process = subprocess.Popen([
            str(ROOT / "runtime" / "omni-runtime" / "Scripts" / "python.exe"),
            str(ROOT / "scripts" / "omni-service" / "server.py"), "--port", str(args.port),
        ], cwd=ROOT, env=environment, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 900
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError("Omni process exited; inspect runtime/omni-service-probe.log")
                try:
                    _, health = request(args.port, token, "/health", timeout=3)
                    if health.get("ready"):
                        break
                    if health.get("phase") == "error":
                        raise RuntimeError("Omni startup failed; inspect runtime/omni-service-probe.log")
                except (urllib.error.URLError, ConnectionError, TimeoutError):
                    pass
                time.sleep(2)
            else:
                raise RuntimeError("Omni readiness deadline exceeded")
            result["model_loaded"] = True
            if "text" in checks:
                _, response = request(args.port, token, "/v1/chat/completions", {
                    "model": "qwen2_5_omni", "modalities": ["text"], "max_tokens": 8,
                    "messages": [{"role": "user", "content": "Reply with exactly OK."}],
                })
                result["checks"]["text"] = {"reply": response["choices"][0]["message"]["content"], "elapsed_ms": response["x_ai_tutor"]["elapsed_ms"]}
            if "image" in checks:
                from PIL import Image, ImageDraw
                image = Image.new("RGB", (160, 120), "#13527e")
                draw = ImageDraw.Draw(image)
                draw.ellipse((55, 25, 105, 75), fill="#cc9900")
                output = io.BytesIO()
                image.save(output, format="PNG")
                _, response = request(args.port, token, "/v1/chat/completions", {
                    "model": "qwen2_5_omni", "modalities": ["text"], "max_tokens": 24,
                    "messages": [{"role": "user", "content": [
                        {"type": "text", "text": "Name the two dominant colors in this image, in English, using only two words."},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64," + base64.b64encode(output.getvalue()).decode("ascii")}},
                    ]}],
                })
                result["checks"]["image"] = {"reply": response["choices"][0]["message"]["content"], "synthetic_image": True}
            if "audio_input" in checks:
                import numpy as np
                import soundfile as sf
                samples = np.zeros(1600, dtype=np.float32)
                output = io.BytesIO()
                sf.write(output, samples, 16000, format="WAV", subtype="PCM_16")
                _, response = request(args.port, token, "/v1/chat/completions", {
                    "model": "qwen2_5_omni", "modalities": ["text"], "max_tokens": 32,
                    "messages": [{"role": "user", "content": [
                        {"type": "input_audio", "input_audio": {"data": base64.b64encode(output.getvalue()).decode("ascii"), "format": "wav"}},
                        {"type": "text", "text": "Does this recording contain audible speech? Answer only yes or no; this is a silent synthetic sample."},
                    ]}],
                })
                result["checks"]["audio_input"] = {"reply": response["choices"][0]["message"]["content"], "synthetic_silence": True}
            if "speech" in checks:
                import numpy as np
                import soundfile as sf
                headers, audio = request(args.port, token, "/v1/audio/speech", {
                    "model": "qwen2_5_omni", "input": "你好。", "voice": "Chelsie", "response_format": "wav",
                })
                samples, rate = sf.read(io.BytesIO(audio), dtype="float32")
                result["checks"]["speech"] = {
                    "sample_rate": rate, "frames": len(samples),
                    "duration_seconds": round(len(samples) / rate, 3),
                    "rms": float(np.sqrt(np.mean(samples ** 2))),
                    "text_match": headers.get("X-Speech-Validation"),
                    "audible_by_human": False,
                }
            print(json.dumps(result, ensure_ascii=False, indent=2), flush=True)
        finally:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


if __name__ == "__main__":
    main()
