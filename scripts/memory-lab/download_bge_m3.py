"""Download only the pinned official BGE-M3 native assets. No model execution.

Existing differing files and partial files are never overwritten. Small Git blobs
are checked against the author's tree object ID as well as recording SHA-256.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import time
import urllib.request

REVISION = "5617a9f61b028005a4858fdac845db406aefb181"
MODEL_ID = "BAAI/bge-m3"
# path, exact byte length, author Git blob ID, LFS SHA-256 (when applicable)
ASSETS = [
    ("README.md", 15822, "e5a320176edd6ee1cfb256e68ee5ac0004de9447", None),
    ("config.json", 687, "e6eda1c72da8f9dc30fdd9b69c73d35af3b7a7ad", None),
    ("tokenizer_config.json", 444, "dc69ac559dcba2694012009aaa108c614541789a", None),
    ("special_tokens_map.json", 964, "b1879d702821e753ffe4245048eee415d54a9385", None),
    ("1_Pooling/config.json", 191, "9bd85925f325e25246d94c4918dc02ab98f2a1b7", None),
    ("sentencepiece.bpe.model", 5069051, "7a3f40a75f870bc1f21700cd414dc2acc431583c", "cfc8146abe2a0488e9e2a0c56de7952f7c11ab059eca145a0a727afce0db2865"),
    ("tokenizer.json", 17098108, "c8fd8778874b9aa8bdafaa3bbd760f236d237dd6", "21106b6d7dab2952c1d496fb21d5dc9db75c28ed361a05f5020bbba27810dd08"),
    ("pytorch_model.bin", 2271145830, "88e71f33b1af45aa960f3fbf6883edc6f014452a", "b5e0ce3470abf5ef3831aa1bd5553b486803e83251590ab7ff35a117cf6aad38"),
]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def verify(path, asset):
    name, size, git_oid, lfs_sha = asset
    require(path.stat().st_size == size, "Asset size mismatch: " + name)
    sha = hashlib.sha256()
    git = hashlib.sha1(("blob " + str(size) + "\0").encode())
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            sha.update(block)
            git.update(block)
    require(sha.hexdigest() == lfs_sha if lfs_sha else git.hexdigest() == git_oid,
            "Official asset digest mismatch: " + name)
    return {"path": name, "bytes": size, "sha256": sha.hexdigest(),
            "official_git_oid": git_oid, "official_lfs_sha256": lfs_sha,
            "url": "https://huggingface.co/" + MODEL_ID + "/resolve/" + REVISION + "/" + name + "?download=true"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args()
    require(not args.report.exists(), "Use a new report path")
    directory = args.root.resolve() / "models/memory/bge-m3"
    directory.mkdir(parents=True, exist_ok=True)
    require(shutil.disk_usage(directory).free > 4 * 1024 ** 3, "Less than 4 GiB free")
    report = {"model_id": MODEL_ID, "revision": REVISION, "license": "MIT (official model card)",
              "license_source": "https://huggingface.co/BAAI/bge-m3/blob/" + REVISION + "/README.md",
              "tree_source": "https://huggingface.co/api/models/BAAI/bge-m3/tree/" + REVISION + "?recursive=true&expand=false",
              "model_executed": False, "packages_installed": False, "assets": [], "status": "running"}
    start = time.monotonic()
    try:
        for asset in ASSETS:
            name, size, _, _ = asset
            target = directory / name
            if target.exists():
                row = verify(target, asset)
                row["downloaded"] = False
            else:
                require(not args.verify_only, "Missing asset: " + name)
                target.parent.mkdir(parents=True, exist_ok=True)
                partial = target.with_name(target.name + ".partial")
                require(not partial.exists(), "Existing partial retained for inspection: " + name)
                url = "https://huggingface.co/" + MODEL_ID + "/resolve/" + REVISION + "/" + name + "?download=true"
                if sys.platform == "win32":
                    # Match the already verified E5 acquisition's Windows HTTPS
                    # stack; urllib may not share the machine's proxy routing.
                    subprocess.run(["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                                    str(Path(__file__).with_name("download_bge_asset.ps1")),
                                    "-Url", url, "-PartialPath", str(partial)], check=True)
                else:
                    request = urllib.request.Request(url, headers={"User-Agent": "ai-tutor-fixed-model-audit/1"})
                    with urllib.request.urlopen(request, timeout=120) as response, partial.open("xb") as stream:
                        require(response.status == 200, "Unexpected download status")
                        count = 0
                        for block in iter(lambda: response.read(1024 * 1024), b""):
                            count += len(block)
                            require(count <= size, "Download exceeds fixed official size")
                            stream.write(block)
                row = verify(partial, asset)
                require(not target.exists(), "Destination appeared during download")
                partial.rename(target)
                row["downloaded"] = True
            report["assets"].append(row)
            print(json.dumps({"asset_verified": name, "bytes": size}), flush=True)
        require("license: mit" in (directory / "README.md").read_text(encoding="utf-8"), "Official card license changed")
        report["status"] = "verified"
    except Exception as error:
        report.update(status="failed", error_type=type(error).__name__, error=str(error)[:1000])
        raise
    finally:
        report["elapsed_seconds"] = time.monotonic() - start
        args.report.parent.mkdir(parents=True, exist_ok=True)
        with args.report.open("x", encoding="utf-8", newline="\n") as output:
            json.dump(report, output, ensure_ascii=False, indent=2)
            output.write("\n")


if __name__ == "__main__":
    main()
