"""Observe real camera and live app for 30min; retain only numeric metrics."""
import argparse
import json
import time
import urllib.request
from pathlib import Path

root = Path(__file__).resolve().parents[1]
info = json.loads((root / "runtime/server.json").read_text(encoding="utf-8"))
base = f"http://127.0.0.1:{info['port']}"


def call(path, payload=None):
    headers = {"Cookie": "ai_tutor_local="+info["token"]}
    data = None
    if payload is not None:
        data = json.dumps(payload).encode()
        headers["Content-Type"] = "application/json"
    with urllib.request.urlopen(urllib.request.Request(base+path, data=data, headers=headers), timeout=12) as r:
        return json.load(r)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--seconds", type=int, default=1800)
    args = parser.parse_args()
    call("/api/camera/open", {})
    start = time.monotonic()
    output = root / "runtime/soak.jsonl"
    count, failures = 0, 0
    with output.open("w", encoding="utf-8") as log:
        while time.monotonic()-start < args.seconds:
            try:
                state = call("/api/state")
                camera = state["camera"]
                row = {"elapsed": round(time.monotonic()-start, 1), "camera": camera, "memory_mb": state["metrics"]["memory_mb"], "requests": state["metrics"]["requests"]}
                # status contains no pixels; never record image endpoint.
                log.write(json.dumps(row, ensure_ascii=True)+"\n")
                log.flush()
                count += 1
            except Exception as exc:
                failures += 1
                log.write(json.dumps({"elapsed": round(time.monotonic()-start), "error_type": type(exc).__name__})+"\n")
                log.flush()
            time.sleep(5)
    print(json.dumps({"duration_seconds": round(time.monotonic()-start), "samples": count, "failures": failures}))


if __name__ == "__main__":
    main()
