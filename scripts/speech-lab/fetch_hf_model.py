"""Download a pinned public model snapshot and verify supplied LFS digests."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
from urllib.parse import quote


def download_file(item, model_id, revision, output):
    name = item["rfilename"]
    relative = PurePosixPath(name)
    if relative.is_absolute() or ".." in relative.parts or "\\" in name or ":" in name:
        raise ValueError("Invalid model file path")
    path = output / name
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        partial = path.with_suffix(path.suffix + ".part")
        url = f"https://huggingface.co/{model_id}/resolve/{revision}/{quote(name)}"
        subprocess.run(["curl.exe" if os.name == "nt" else "curl", "--http1.1", "--location", "--fail",
                        "--max-time", "1800", "--connect-timeout", "30", "--retry", "2", "--retry-all-errors", "--silent", "--show-error",
                        "--continue-at", "-", "--output", str(partial), url], check=True)
        if partial.stat().st_size != item["size"]:
            raise ValueError(f"Incomplete model file: {name}")
        os.replace(partial, path)
    if path.stat().st_size != item["size"]:
        raise ValueError(f"Wrong model file size: {name}")
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    value = digest.hexdigest()
    expected = (item.get("lfs") or {}).get("sha256")
    if expected and value != expected:
        raise ValueError(f"Model digest mismatch: {name}")
    print(json.dumps({"file": name, "bytes": item["size"], "verified_lfs": bool(expected)}), flush=True)
    return {"file": name, "size": item["size"], "sha256": value, "upstream_sha256_verified": bool(expected)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("metadata", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    metadata = json.loads(args.metadata.read_text(encoding="utf-8-sig"))
    args.output.mkdir(parents=True, exist_ok=True)
    with ThreadPoolExecutor(max_workers=3) as pool:
        files = list(pool.map(lambda row: download_file(row, metadata["id"], metadata["sha"], args.output), metadata["siblings"]))
    record = {"model_id": metadata["id"], "revision": metadata["sha"],
              "license": metadata.get("cardData", {}).get("license"), "files": files}
    (args.output / "download-manifest.json").write_text(json.dumps(record, indent=2), encoding="utf-8")
