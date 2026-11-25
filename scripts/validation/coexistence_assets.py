"""Assets for the specific camera/speech coexistence fixture; no process startup."""
import json
import os
from pathlib import Path
import shutil

from camera_stream_smoke import ASSETS, PROTECTED
from database_compatibility_smoke import require, sha256, write_json
from local_asr_lifecycle_smoke import BUNDLE as ASR_BUNDLE, MODEL as ASR_MODEL, WEIGHTS
from local_voice_lifecycle_smoke import BUNDLE as TTS_BUNDLE


def stage(root, output, ui, python, engine, distribution, references_manifest):
    fixture = output / "fixture"
    shutil.copytree(ui, fixture / "frontend/dist")
    sources = {}
    def remember(path):
        sources[path] = sha256(path)
    def copy(source, target, hard_link=False):
        require(source.is_file(), "required_fixture_asset_missing")
        target.parent.mkdir(parents=True, exist_ok=True)
        remember(source)
        if hard_link:
            os.link(source, target)
            require(source.samefile(target), "model_hard_link_failed")
        else:
            shutil.copyfile(source, target)
            require(sha256(target) == sources[source], "staged_program_hash_mismatch")
    for name, expected in ASSETS.items():
        require(sha256(root / name) == expected, "fixed_vision_asset_hash_mismatch")
        copy(root / name, fixture / name)
    for folder, names, destination in (("speech-service", ASR_BUNDLE, "local-asr"),
                                       ("speech-lab", TTS_BUNDLE, "local-speech")):
        for name in names:
            copy(root / "scripts" / folder / name, fixture / "bin" / destination / name)
    for name in WEIGHTS:
        relative = Path("models") / ASR_MODEL / name
        copy(root / relative, fixture / relative, hard_link=True)
    tts_model = Path("models/speech") / ("Fun-CosyVoice3-0.5B-2512/omni-overlay" if engine == "cosy"
                                      else "Qwen3-TTS-12Hz-0.6B-CustomVoice")
    require((root / tts_model).is_dir(), "fixed_tts_model_missing")
    model_files = [p for p in (root / tts_model).rglob("*") if p.is_file()]
    require(model_files, "fixed_tts_model_empty")
    for source in model_files:
        copy(source, fixture / source.relative_to(root), hard_link=True)
    copy(root / "runtime/speech-gpu/omni-env-path.txt", fixture / "runtime/speech-gpu/omni-env-path.txt")
    require(python.is_absolute() and python.is_file(), "absolute_asr_python_missing")
    remember(python)
    config = {"engine": engine, "distribution": distribution}
    references = []
    if engine == "cosy":
        require(references_manifest is not None, "explicit_synthetic_reference_manifest_required")
        manifest = json.loads(references_manifest.read_text(encoding="utf-8"))
        remember(references_manifest)
        selected = {}
        for language in ("zh", "en"):
            rows = [row for row in manifest["samples"] if row.get("id") == language + "_concept"]
            require(len(rows) == 1 and rows[0].get("source_type") == "synthetic_sapi_adult_voice",
                    "synthetic_reference_identity_invalid")
            row = rows[0]
            wav = Path(row["path"])
            require(wav.is_absolute() and wav.is_file() and sha256(wav) == row["sha256"],
                    "synthetic_reference_hash_mismatch")
            remember(wav)
            selected[language] = {"wav_path": str(wav), "sha256": row["sha256"], "text": row["text"]}
            references.append({"language": language, "sha256": row["sha256"], "source_type": row["source_type"]})
        write_json(fixture / "runtime/tts-references.json", selected)
        config["references_file"] = "runtime/tts-references.json"
    policies = {
        "local-asr-policy.json": {"protocol_version": 1, "mode": "managed", "python_executable": str(python)},
        "local-speech-policy.json": {"protocol_version": 1, "mode": "managed",
                                     "warmup": "synthetic_bilingual", "managed": config},
    }
    # Start disabled so the shared OwnedWorkstation.ready performs its original
    # empty fixture checks. Only explicit POST retry starts the staged services.
    for name in policies:
        disabled = {"protocol_version": 1, "mode": "disabled"}
        if name == "local-speech-policy.json":
            disabled["warmup"] = "disabled"
        write_json(fixture / "runtime" / name, disabled)
    public_sources = [{"relative": str(p.relative_to(root)) if p.is_relative_to(root) else "explicit_external_asset",
                       "sha256": digest, "bytes": p.stat().st_size} for p, digest in sources.items()]
    return fixture, policies, sources, {"assets": public_sources, "synthetic_references": references,
                                       "engine": engine, "distribution": distribution}


def protected_hashes(root):
    return {name: sha256(root / name) if (root / name).is_file() else None for name in PROTECTED}
