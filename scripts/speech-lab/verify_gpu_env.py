"""Verify the isolated lab environment and select it only after a CUDA check."""
import json
from importlib.metadata import version
from pathlib import Path
import sys
import torch

nvcc = version("nvidia-cuda-nvcc")
runtime = version("nvidia-cuda-runtime")
if nvcc.split(".")[:2] != runtime.split(".")[:2]:
    raise SystemExit(f"CUDA compiler {nvcc} and runtime headers {runtime} differ; FlashInfer cannot compile")
kernel_ok = float((torch.arange(16, device="cuda", dtype=torch.float32) ** 2).sum().cpu()) == 1240.0
print(json.dumps({"torch": torch.__version__, "cuda": torch.version.cuda,
                  "nvcc": nvcc, "runtime_headers": runtime,
                  "device": torch.cuda.get_device_name(0), "cuda_kernel_check": kernel_ok}), flush=True)
if not kernel_ok:
    raise SystemExit("CUDA check failed")
if len(sys.argv) == 2:
    Path(sys.argv[1]).write_text(str(Path(sys.prefix)) + "\n", encoding="utf-8")
