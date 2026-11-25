"""Compile and run model-free C# voice adapter consumer tests on Windows."""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import uuid


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def execute(command: list[str], cwd: Path, timeout: int) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command, cwd=str(cwd), capture_output=True, text=True, encoding="utf-8",
        errors="replace", timeout=timeout, creationflags=subprocess.CREATE_NO_WINDOW,
    )


def main() -> int:
    adapter = Path(__file__).resolve().parent
    repo = adapter.parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", type=Path, default=repo / "runtime/goal-releases/20261109-aec-runtime/ai-tutor-voice-bridge.exe")
    parser.add_argument("--output-base", type=Path, default=repo / "runtime/goal-baselines/20261109/destiny-adapter-next")
    args = parser.parse_args()
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    output = args.output_base.resolve() / (stamp + "-" + uuid.uuid4().hex[:8])
    output.mkdir(parents=True, exist_ok=False)
    root = output / "empty-voice-root"
    root.mkdir()
    csc = Path(os.environ.get("WINDIR", r"C:\Windows")) / "Microsoft.NET/Framework64/v4.0.30319/csc.exe"
    source_files = sorted(adapter.glob("*.cs"))
    harness_source = adapter / "tests/VoiceBridgeConsumerTests.cs"
    helper_source = adapter / "tests/SyntheticHelper.cs"
    report: dict = {
        "started_utc": stamp, "output": str(output), "framework": ".NETFramework4/C#5",
        "model_free": True, "rust_prepare_sent": False, "microphone_capture_requested": False,
        "existing_apps_launched": False, "raw_protocol_persisted": False,
        "source_sha256": {}, "checks": [],
    }
    try:
        if not source_files:
            raise RuntimeError("adapter_sources_missing")
        for path in source_files + [harness_source, helper_source, Path(__file__).resolve()]:
            report["source_sha256"][str(path.relative_to(repo))] = digest(path)
        bridge = args.bridge.resolve(strict=True)
        report["bridge"] = {"path": str(bridge), "sha256": digest(bridge)}
        common = [str(csc), "/nologo", "/utf8output", "/target:exe", "/langversion:5", "/optimize+",
                  "/reference:System.Web.Extensions.dll"]
        helper_exe = output / "SyntheticHelper.exe"
        harness_exe = output / "VoiceBridgeConsumerTests.exe"
        compilations = [
            ("synthetic_helper", common + ["/out:" + str(helper_exe), str(helper_source)]),
            ("consumer_harness", common + ["/out:" + str(harness_exe)] + [str(p) for p in source_files] + [str(harness_source)]),
        ]
        report["compilation"] = {}
        for name, command in compilations:
            compiled = execute(command, repo, 60)
            report["compilation"][name] = {"exit_code": compiled.returncode, "diagnostics": compiled.stdout + compiled.stderr}
            if compiled.returncode:
                raise RuntimeError(name + "_compilation_failed")
        ran = execute([str(harness_exe), str(bridge), str(root), str(helper_exe)], repo, 90)
        report["harness_exit_code"] = ran.returncode
        for line in ran.stdout.splitlines():
            parts = line.split("\t", 2)
            if len(parts) >= 2 and parts[0] in ("PASS", "FAIL"):
                report["checks"].append({"name": parts[1], "passed": parts[0] == "PASS", "detail": parts[2] if len(parts) > 2 else ""})
        report["unexpected_stdout_lines"] = sum(not line.startswith(("PASS\t", "FAIL\t")) for line in ran.stdout.splitlines())
        report["unexpected_stderr_characters"] = len(ran.stderr)
        report["synthetic_root_remaining_files"] = len(list(root.rglob("*")))
        report["passed"] = ran.returncode == 0 and len(report["checks"]) >= 12 and all(item["passed"] for item in report["checks"]) and not report["unexpected_stdout_lines"] and not report["unexpected_stderr_characters"] and report["synthetic_root_remaining_files"] == 0
    except subprocess.TimeoutExpired:
        report["passed"] = False
        report["error"] = "consumer_harness_timeout"
    except Exception as error:
        report["passed"] = False
        report["error"] = str(error)
    report["finished_utc"] = dt.datetime.now(dt.timezone.utc).isoformat()
    (output / "results.json").write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")
    print(json.dumps({"passed": report["passed"], "checks": report["checks"], "evidence": str(output / "results.json"), "error": report.get("error")}, ensure_ascii=False))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
