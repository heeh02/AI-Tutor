"""Fetch official research candidates; preserve source/hash and reject unsafe archives."""
import argparse
import hashlib
import json
import os
import subprocess
from pathlib import Path, PurePosixPath
import tarfile
import time
import urllib.request


def fetch(asset, destination, transport):
    destination.mkdir(parents=True, exist_ok=True)
    archive = destination / asset["name"]
    partial = archive.with_suffix(archive.suffix + ".part")
    start = time.perf_counter()
    if not archive.exists():
        if transport == "curl":
            subprocess.run(["curl.exe" if os.name == "nt" else "curl", "--location", "--fail",
                            "--max-time", "900", "--retry", "2", "--silent", "--show-error",
                            "--range", f"0-{asset['size'] - 1}", "--output", str(partial),
                            asset["browser_download_url"]], check=True)
        else:
            request = urllib.request.Request(asset["browser_download_url"], headers={"User-Agent": "AI-Tutor-model-baseline"})
            with urllib.request.urlopen(request, timeout=60) as response, partial.open("wb") as output:
                count = 0
                while block := response.read(64 * 1024):
                    count += len(block)
                    if count > asset["size"]:
                        raise ValueError("Download exceeds release asset size")
                    output.write(block)
        if partial.stat().st_size != asset["size"]:
            raise ValueError("Incomplete release asset")
        os.replace(partial, archive)
    if archive.stat().st_size != asset["size"]:
        raise ValueError("Existing archive size differs from release asset")
    checksum = hashlib.sha256()
    with archive.open("rb") as source:
        while block := source.read(1024 * 1024):
            checksum.update(block)
    digest = "sha256:" + checksum.hexdigest()
    if asset.get("digest") and digest != asset["digest"]:
        raise ValueError("Release digest mismatch")
    destination = destination.resolve()
    with tarfile.open(archive, "r:bz2") as package:
        members = package.getmembers()
        total = 0
        for member in members:
            relative = PurePosixPath(member.name)
            if relative.is_absolute() or ".." in relative.parts or "\\" in member.name or ":" in member.name:
                raise ValueError("Unsafe archive path")
            target = (destination / member.name).resolve()
            if not target.is_relative_to(destination) or not (member.isfile() or member.isdir()):
                raise ValueError("Unsafe archive entry")
            total += member.size
            if total > 4_000_000_000:
                raise ValueError("Archive extraction size limit")
        # All paths and entry types checked before any write (Python 3.10 compatible).
        package.extractall(destination, members=members)
    record = {**asset, "computed_digest": digest, "upstream_digest_verified": bool(asset.get("digest")),
              "archive": str(archive.resolve()), "extracted_bytes": total,
              "elapsed_seconds": round(time.perf_counter() - start, 3),
              "transport": transport,
              "license_review": "pending; inspect included license and upstream model card",
              "scope": "local baseline candidate, not installed as production default"}
    (destination / (asset["name"] + ".manifest.json")).write_text(json.dumps(record, indent=2), encoding="utf-8")
    print(json.dumps(record), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("assets", type=Path)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--name", required=True)
    parser.add_argument("--transport", choices=["urllib", "curl"], default="curl")
    args = parser.parse_args()
    entries = json.loads(args.assets.read_text(encoding="utf-8-sig"))
    fetch(next(item for item in entries if item["name"] == args.name), args.destination, args.transport)
