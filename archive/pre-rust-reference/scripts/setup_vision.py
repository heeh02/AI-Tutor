"""Download only the reviewed ONNX face weight and verify its pinned digest."""
from pathlib import Path
import hashlib
import json
import time
import requests

root = Path(__file__).resolve().parents[1]
entry = json.loads((root / "models/manifest.json").read_text(encoding="utf-8"))["models"][0]
destination = root / entry["file"]
if destination.exists() and hashlib.sha256(destination.read_bytes()).hexdigest() == entry["sha256"]:
    print("Face model already verified.")
else:
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_suffix(".partial")
    start = time.monotonic()
    try:
        with requests.get(entry["download_url"], stream=True, timeout=(8, 20)) as response:
            response.raise_for_status()
            size = 0
            digest = hashlib.sha256()
            with temporary.open("wb") as output:
                for part in response.iter_content(128 * 1024):
                    if time.monotonic() - start > 180:
                        raise RuntimeError("Face model download exceeded180seconds")
                    size += len(part)
                    if size > 40 * 1024 * 1024:
                        raise RuntimeError("Face model download exceeds reviewed size bound")
                    output.write(part)
                    digest.update(part)
        if digest.hexdigest() != entry["sha256"]:
            raise RuntimeError("Downloaded face model does not match reviewed SHA256; not installed")
        temporary.replace(destination)
        print("Face model downloaded and verified.")
    finally:
        if temporary.exists():
            temporary.unlink()
