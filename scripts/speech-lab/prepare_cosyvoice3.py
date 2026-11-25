"""Prepare the audited CosyVoice3 2512 subset; never installs or starts inference."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess


def hashes(path):
    size = path.stat().st_size
    sha = hashlib.sha256()
    git = hashlib.sha1(f"blob {size}\0".encode())
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            sha.update(chunk)
            git.update(chunk)
    return sha.hexdigest(), git.hexdigest()


def verify(path, item):
    if path.stat().st_size != item["size"]:
        raise ValueError(f"Wrong size: {item['path']}")
    sha, git = hashes(path)
    expected = item.get("sha256")
    if expected and sha != expected:
        raise ValueError(f"LFS SHA-256 mismatch: {item['path']}")
    if not expected and git != item["git_blob_sha1"]:
        raise ValueError(f"Pinned Git blob SHA-1 mismatch: {item['path']}")
    return sha


def fetch(item, upstream):
    relative = PurePosixPath(item["path"])
    if relative.is_absolute() or ".." in relative.parts or "\\" in item["path"] or ":" in item["path"]:
        raise ValueError("Invalid model file path")
    target = upstream / str(relative)
    target.parent.mkdir(parents=True, exist_ok=True)
    origin_record = target.with_name(target.name + ".origin.json")
    if target.exists():
        digest = verify(target, item)
        source = json.loads(origin_record.read_text(encoding="utf-8"))["url"] if origin_record.exists() else "existing_verified_file"
    else:
        failures = []
        for index, source in enumerate(item["urls"]):
            partial = target.with_name(f"{target.name}.source{index}.part")
            print(json.dumps({"starting": item["path"], "source": source}, ensure_ascii=False), flush=True)
            try:
                if os.name == "nt" and source.startswith("https://huggingface.co/") and item["size"] < 16 * 1024**2:
                    # This host's .NET HTTPS route worked while direct curl
                    # to HF timed out. Preserve the same verified transport.
                    # Values stay in environment entries, never shell code.
                    env = dict(os.environ, COSY_DOWNLOAD_URL=source, COSY_DOWNLOAD_PATH=str(partial.resolve()))
                    subprocess.run([
                        "powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
                        "$ErrorActionPreference='Stop'; Invoke-WebRequest -UseBasicParsing "
                        "-Uri $env:COSY_DOWNLOAD_URL -OutFile $env:COSY_DOWNLOAD_PATH -TimeoutSec 60",
                    ], env=env, check=True)
                else:
                    subprocess.run([
                        "curl.exe" if os.name == "nt" else "curl", "--http1.1", "--location", "--fail",
                        "--connect-timeout", "15", "--max-time", "1800", "--retry", "2", "--retry-all-errors",
                        "--speed-limit", "1024", "--speed-time", "45", "--continue-at", "-",
                        "--silent", "--show-error", "--output", str(partial), source,
                    ], check=True)
                digest = verify(partial, item)
                os.replace(partial, target)
                origin_record.write_text(json.dumps({"url": source}, indent=2) + "\n", encoding="utf-8")
                break
            except (subprocess.CalledProcessError, ValueError) as error:
                failures.append(str(error))
                print(json.dumps({"failed": item["path"], "source": source, "reason": str(error)}), flush=True)
        else:
            raise RuntimeError(f"All sources failed for {item['path']}: {failures}")
    result = {
        "path": item["path"], "bytes": item["size"], "sha256": digest,
        "upstream_lfs_sha256_verified": bool(item.get("sha256")),
        "upstream_git_blob_sha1_verified": not bool(item.get("sha256")), "retrieved_from": source,
    }
    print(json.dumps({"verified": result}, ensure_ascii=False), flush=True)
    return result


def link_verified(source, target):
    target.parent.mkdir(parents=True, exist_ok=True)
    source_hash = hashes(source)[0]
    if target.exists():
        if hashes(target)[0] != source_hash:
            raise ValueError(f"Existing overlay/cache differs: {target}")
        return "existing_identical"
    try:
        os.link(source, target)
        return "hardlink"
    except OSError:
        shutil.copyfile(source, target)
        if hashes(target)[0] != source_hash:
            raise ValueError(f"Copy verification failed: {target}")
        return "verified_copy"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("metadata", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--cache", required=True, type=Path)
    args = parser.parse_args()
    metadata = json.loads(args.metadata.read_text(encoding="utf-8-sig"))
    if metadata["revision"] != "29e01c4e8d000f4bcd70751be16fa94bf3d85a18":
        raise ValueError("This preparation is only audited for the fixed 2512 revision")
    args.output.mkdir(parents=True, exist_ok=True)
    required = sum(item["size"] for item in metadata["files"])
    free = shutil.disk_usage(args.output).free
    if free < required * 2 + 20 * 1024**3:
        raise RuntimeError("Insufficient room for model, temporary files and 20 GiB running space")
    print(json.dumps({"free_bytes_before": free, "required_upstream_bytes": required}), flush=True)
    upstream = args.output / "upstream"
    with ThreadPoolExecutor(max_workers=2) as pool:
        records = list(pool.map(lambda item: fetch(item, upstream), metadata["files"]))
    overlay = args.output / "omni-overlay"
    links = {}
    for item in metadata["files"]:
        if item["path"] in ("config.json", "README.md"):
            continue
        links[item["path"]] = link_verified(upstream / item["path"], overlay / item["path"])
    config = {"model_type": "cosyvoice3", "architectures": ["CosyVoice3Model"]}
    local_config = overlay / "config.json"
    config_bytes = (json.dumps(config, indent=2) + "\n").encode()
    if local_config.exists() and local_config.read_bytes() != config_bytes:
        raise ValueError("Existing local overlay config differs")
    local_config.write_bytes(config_bytes)
    cache_path = args.cache / "s3tokenizer" / "speech_tokenizer_v3_25hz.onnx"
    cache_method = link_verified(upstream / "speech_tokenizer_v3.onnx", cache_path)
    record = {
        "model_id": metadata["model_id"], "revision": metadata["revision"], "license": metadata["license"],
        "verified_at": datetime.now(timezone.utc).isoformat(), "files": records,
        "overlay": {"config": config, "config_sha256": hashlib.sha256(config_bytes).hexdigest(), "files": links},
        "tokenizer_cache": {"path": str(cache_path.resolve()), "method": cache_method,
                            "sha256": hashes(cache_path)[0]},
        "inference_started": False, "quality_or_latency_verified": False,
    }
    (args.output / "download-manifest.json").write_text(json.dumps(record, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"complete": True, "manifest": str(args.output / "download-manifest.json")}), flush=True)


if __name__ == "__main__":
    main()
