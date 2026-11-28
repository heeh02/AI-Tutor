import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
BUILD = ROOT / "runtime" / "omni-build"
SOURCE = BUILD / "gptqmodel-2.0.0"
ARCHIVE = BUILD / "gptqmodel.tar.gz"
VERSION = "2.0.0"


def main():
    BUILD.mkdir(parents=True, exist_ok=True)
    with urllib.request.urlopen(f"https://pypi.org/pypi/gptqmodel/{VERSION}/json", timeout=60) as response:
        release = json.load(response)
    package = next(item for item in release["urls"] if item["packagetype"] == "sdist")
    if not ARCHIVE.is_file():
        urllib.request.urlretrieve(package["url"], ARCHIVE)
    if hashlib.sha256(ARCHIVE.read_bytes()).hexdigest() != package["digests"]["sha256"]:
        raise RuntimeError("Official GPTQ source archive failed SHA-256 verification")
    if not SOURCE.is_dir():
        with tarfile.open(ARCHIVE) as archive:
            for member in archive.getmembers():
                path = Path(member.name)
                if path.is_absolute() or ".." in path.parts:
                    raise RuntimeError("Unsafe GPTQ source path")
            archive.extractall(BUILD)
    setup = SOURCE / "setup.py"
    original = setup.read_text(encoding="utf-8")
    marker = 'BUILD_CUDA_EXT = sys.platform != "darwin"'
    if marker in original:
        setup.write_text(original.replace(marker, "BUILD_CUDA_EXT = False", 1), encoding="utf-8")
    elif "BUILD_CUDA_EXT = False" not in original:
        raise RuntimeError("Unexpected GPTQ build configuration")
    subprocess.run([
        sys.executable, "-m", "pip", "install", "--disable-pip-version-check",
        "--no-build-isolation", "--no-deps", str(SOURCE),
    ], check=True)
    subprocess.run([
        sys.executable, "-m", "pip", "install", "--disable-pip-version-check",
        "-r", str(Path(__file__).with_name("requirements-gptq-win32.txt")),
    ], check=True)
    from gptqmodel.utils.backend import BACKEND
    from gptqmodel.nn_modules.qlinear.torch import TorchQuantLinear
    print(json.dumps({"backend": BACKEND.TORCH.value, "quant_linear": TorchQuantLinear.__name__, "source_sha256": package["digests"]["sha256"], "custom_cuda_extension": False}))


if __name__ == "__main__":
    main()
