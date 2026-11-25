"""Download pinned multilingual ASR with bounded I/O and byte verification.

HF mirror is a transport fallback, not a new model publisher. No credentials used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import time

import requests

ROOT = Path(__file__).resolve().parents[1]
TARGET = ROOT / "models" / "asr" / "faster-whisper-base"
REPO = "Systran/faster-whisper-base"
REVISION = "ebe41f70d5b6dfa9166e2c581c45c9c0cfc57b66"
# model.bin SHA256 is independently published on the original author's HF file page.
# Small-file Git blob identities are pinned from the repository tree for this revision.
FILES = {
    "config.json": {"size": 2309, "git_sha1": "867cf1a0fece1394e01d55e287ba2f09a577c046"},
    "model.bin": {"size": 145217532, "sha256": "d01c3014881c9c6f3133c182f3d2887eb6ca1c789a7538c5c007196857a0a6a9"},
    "tokenizer.json": {"size": 2203239, "git_sha1": "7818adb6de9fa3064d3ff81226fdd675be1f6344"},
    "vocabulary.txt": {"size": 459861, "git_sha1": "c9074644d9d1205686f16d411564729461324b75"},
}


def verify(path, expected):
    if not path.exists() or path.stat().st_size != expected["size"]:
        return False
    if "sha256" in expected:
        return hashlib.sha256(path.read_bytes()).hexdigest() == expected["sha256"]
    data = path.read_bytes()
    return hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest() == expected["git_sha1"]


def download(name, spec, endpoints, overall_deadline):
    dest = TARGET / name
    if verify(dest, spec):
        return "existing-verified"
    partial = dest.with_suffix(dest.suffix + ".partial")
    failures = []
    for endpoint in endpoints:
        url = f"{endpoint}/{REPO}/resolve/{REVISION}/{name}"
        for attempt in range(2):
            try:
                file_deadline = min(overall_deadline, time.monotonic() + 150)
                if time.monotonic() >= overall_deadline:
                    raise TimeoutError("Setup total deadline exceeded")
                with requests.get(url, stream=True, timeout=(5, 15)) as response:
                    response.raise_for_status()
                    written = 0
                    with partial.open("wb") as handle:
                        for chunk in response.iter_content(256 * 1024):
                            if time.monotonic() > file_deadline:
                                raise TimeoutError("Model file download deadline exceeded")
                            written += len(chunk)
                            if written > spec["size"]:
                                raise ValueError("Downloaded file exceeds pinned size")
                            handle.write(chunk)
                if not verify(partial, spec):
                    raise ValueError("Pinned model integrity check failed")
                os.replace(partial, dest)
                return endpoint
            except (requests.RequestException, OSError, ValueError, TimeoutError) as exc:
                failures.append(f"{endpoint} attempt {attempt+1}: {type(exc).__name__}")
                partial.unlink(missing_ok=True)
    raise RuntimeError(f"Could not obtain verified {name}; " + "; ".join(failures))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", choices=["auto", "official", "mirror"], default="auto")
    args = parser.parse_args()
    endpoints = {"auto": ["https://huggingface.co", "https://hf-mirror.com"],
                 "official": ["https://huggingface.co"],
                 "mirror": ["https://hf-mirror.com"]}[args.endpoint]
    TARGET.mkdir(parents=True, exist_ok=True)
    deadline = time.monotonic() + 360
    transports = {}
    manifest_path = ROOT / "models" / "asr-manifest.json"
    try:
        previous_transports = json.loads(manifest_path.read_text(encoding="utf8")).get("transport", {})
    except (OSError, ValueError):
        previous_transports = {}
    for name, spec in FILES.items():
        print(f"Verifying/downloading {name}...", flush=True)
        transports[name] = download(name, spec, endpoints, deadline)
        if transports[name] == "existing-verified":
            transports[name] = previous_transports.get(name, "existing-verified; original transport unknown")
        if transports[name] == "https://hf-mirror.com" and args.endpoint == "auto":
            endpoints = ["https://hf-mirror.com"]
    hashes = {name: hashlib.sha256((TARGET / name).read_bytes()).hexdigest() for name in FILES}
    manifest = {"repo": REPO, "revision": REVISION,
        "source": f"https://huggingface.co/{REPO}/tree/{REVISION}",
        "model": "multilingual Whisper base (99 languages; not base.en)",
        "runtime": "faster-whisper 1.2.1 / CTranslate2 CPU int8",
        "license": "MIT (Whisper and Systran converted-model card)",
        "date": "2026-11-01", "sha256": hashes, "pinned_files": FILES,
        "transport": transports,
        "verification": {"model_bin_sha256": "independently compared with original author's HF model.bin page",
                         "small_files": "pinned Git blob IDs from repository tree; SHA256 recorded locally"}}
    manifest_path.write_text(json.dumps(manifest, indent=2), encoding="utf8")
    print(json.dumps({"downloaded": True, "revision": REVISION, "files": list(hashes)}), flush=True)


if __name__ == "__main__":
    main()
