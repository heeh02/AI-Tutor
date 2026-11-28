import argparse
import concurrent.futures
import datetime
import hashlib
import json
from pathlib import Path, PurePosixPath
import time
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
REPOSITORY = "Qwen/Qwen2.5-Omni-7B-GPTQ-Int4"
ENDPOINT = f"https://modelscope.cn/api/v1/models/{REPOSITORY}/repo"
REQUIRED = {
    "config.json", "generation_config.json", "preprocessor_config.json",
    "tokenizer.json", "tokenizer_config.json", "vocab.json", "merges.txt",
    "model.safetensors.index.json", "spk_dict.pt", "LICENSE",
    "model-00001-of-00004.safetensors", "model-00002-of-00004.safetensors",
    "model-00003-of-00004.safetensors", "model-00004-of-00004.safetensors",
}


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(8 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def request(url):
    return urllib.request.Request(url, headers={
        "User-Agent": "AI-Tutor-Omni/1.0", "Accept-Encoding": "identity",
    })


def download(entry, directory):
    name = entry["Path"]
    size = entry["Size"]
    expected_hash = entry["Sha256"]
    target = directory.joinpath(*PurePosixPath(name).parts)
    if target.is_file() and target.stat().st_size == size and sha256(target) == expected_hash:
        print(f"verified {name}", flush=True)
        return
    target.parent.mkdir(parents=True, exist_ok=True)
    partial = target.with_name(target.name + ".part")
    offset = partial.stat().st_size if partial.exists() else 0
    if offset > size:
        partial.unlink()
        offset = 0
    url = ENDPOINT + "?" + urllib.parse.urlencode({
        "Revision": entry["Revision"], "FilePath": name,
    })
    headers = {"User-Agent": "AI-Tutor-Omni/1.0", "Accept-Encoding": "identity"}
    if offset:
        headers["Range"] = f"bytes={offset}-"
    started = time.monotonic()
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=120) as response:
        append = offset > 0 and response.status == 206
        mode = "ab" if append else "wb"
        if not append:
            offset = 0
        with partial.open(mode) as output:
            while True:
                chunk = response.read(4 * 1024 * 1024)
                if not chunk:
                    break
                output.write(chunk)
    actual_size = partial.stat().st_size
    if actual_size != size:
        raise RuntimeError(f"{name}: expected {size} bytes, received {actual_size}")
    if sha256(partial) != expected_hash:
        partial.unlink()
        raise RuntimeError(f"{name}: SHA-256 mismatch; incomplete file removed")
    partial.replace(target)
    print(f"downloaded {name} ({size / 1024**3:.2f} GiB, {time.monotonic() - started:.1f}s)", flush=True)


def main():
    parser = argparse.ArgumentParser(description="Download and verify the official Qwen Omni GPTQ model")
    parser.add_argument("--model-dir", type=Path, default=ROOT / "models" / "qwen-omni")
    parser.add_argument("--workers", type=int, default=3)
    parser.add_argument("--metadata-only", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.workers <= 4:
        parser.error("--workers must be between 1 and 4")
    args.model_dir.mkdir(parents=True, exist_ok=True)
    listing_path = args.model_dir / "file-list.json"
    if listing_path.is_file():
        listing = json.loads(listing_path.read_text(encoding="utf-8"))
    else:
        url = ENDPOINT + "/files?" + urllib.parse.urlencode({"Revision": "master", "Recursive": "true"})
        with urllib.request.urlopen(request(url), timeout=60) as response:
            listing = json.load(response)
        listing_path.write_text(json.dumps(listing, ensure_ascii=False, indent=2), encoding="utf-8")
    files = listing["Data"]["Files"]
    if not REQUIRED.issubset({entry["Path"] for entry in files}):
        raise RuntimeError("Model repository is incomplete")
    for entry in files:
        path = PurePosixPath(entry["Path"])
        if path.is_absolute() or ".." in path.parts or "\\" in entry["Path"] or ":" in entry["Path"]:
            raise RuntimeError("Unsafe model repository path")
        if entry["Type"] != "blob" or not isinstance(entry["Size"], int) or not 0 <= entry["Size"] <= 5 * 1024**3:
            raise RuntimeError("Unexpected model file metadata")
        if len(entry["Sha256"]) != 64 or any(c not in "0123456789abcdef" for c in entry["Sha256"]):
            raise RuntimeError("Missing upstream SHA-256")
    selected = [entry for entry in files if not args.metadata_only or entry["Path"].suffix != ".safetensors" or entry["Path"].endswith(".index.json")]
    errors = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = {pool.submit(download, entry, args.model_dir): entry["Path"] for entry in selected}
        for future in concurrent.futures.as_completed(futures):
            try:
                future.result()
            except Exception as error:
                errors.append(f"{futures[future]}: {error}")
                print(errors[-1], flush=True)
    if errors:
        raise RuntimeError("Downloads incomplete; rerun to resume verified partial files")
    latest = max(files, key=lambda entry: entry["CommittedDate"])
    manifest = {
        "schema_version": 1,
        "repository": REPOSITORY,
        "source": "ModelScope official Qwen repository",
        "revision": latest["Revision"],
        "downloaded_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "metadata_only": args.metadata_only,
        "files": [{"path": entry["Path"], "revision": entry["Revision"], "bytes": entry["Size"], "sha256": entry["Sha256"]} for entry in selected],
    }
    (args.model_dir / "download-manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"Verified {len(selected)} files in {args.model_dir}", flush=True)


if __name__ == "__main__":
    main()
