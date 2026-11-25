"""Pinned offline BGE-M3 CPU worker. Importing this module loads only stdlib.

The owning manager starts this process after consuming start. This worker loads
once, warms once, then accepts only encode and stop on its private JSONL pipe.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import sys
import time


MAX_LINE = 65536
PROTOCOL_VERSION = 1
REVISION = "5617a9f61b028005a4858fdac845db406aefb181"
MODEL_ID = "BAAI/bge-m3"
# Exact frozen download_bge_m3.ASSETS; never import the downloader here.
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
_WEIGHTS = ASSETS[-1][3]
_TOKENIZER = ASSETS[-2][3]
_KEY_PARTS = [REVISION, _WEIGHTS, _TOKENIZER, "", "", "cls-l2-v1", 1024]
PROFILE = {
    "model": MODEL_ID, "revision": REVISION,
    "model_key": hashlib.sha256(json.dumps(_KEY_PARTS, separators=(",", ":")).encode("utf-8")).hexdigest(),
    "weight_sha256": _WEIGHTS, "tokenizer_sha256": _TOKENIZER,
    "pooling": "cls-l2-v1", "query_prefix": "", "document_prefix": "",
    "dimension": 1024, "artifact": "pytorch-bin-native-fp32",
    "normalization": "L2", "maximum_tokens": 512,
}
_PROFILE_JSON = json.dumps(PROFILE, sort_keys=True, separators=(",", ":"), allow_nan=False)
_INSTANCE = re.compile(r"[0-9a-f]{32}\Z")
_REQUEST = re.compile(r"[A-Za-z0-9_.-]{1,96}\Z")
_HASH = re.compile(r"[0-9a-f]{64}\Z")
_BASE_KEYS = {"protocol_version", "instance_id", "op"}
_READY_TIMINGS = {"hash_audit_ms", "model_load_ms", "warmup_ms", "total_ms"}
_ENCODE_TIMINGS = {"tokenize_ms", "inference_ms", "total_ms"}
WARMUP_TEXT = "Learning starts with a question."


class ProtocolError(Exception):
    """Only a fixed public code; never retains input or an exception message."""
    _CODES = {"invalid_line", "line_too_large", "invalid_json", "invalid_command",
              "invalid_instance", "instance_mismatch", "profile_mismatch",
              "input_hash_mismatch", "invalid_arguments", "invalid_model_dir",
              "unexpected_start", "invalid_output"}

    def __init__(self, code):
        self.code = code if code in self._CODES else "invalid_command"
        super().__init__(self.code)


class StartupError(Exception):
    def __init__(self, code):
        self.code = code if code in {"asset_mismatch", "local_model_load_failed", "warmup_failed"} else "local_model_load_failed"
        super().__init__(self.code)


class InputTooLong(Exception):
    def __init__(self, tokens):
        self.tokens = tokens
        super().__init__("input_too_long")


class EncodeFailure(Exception):
    def __init__(self, tokens=None):
        self.tokens = tokens
        super().__init__("encode_failed")


def _finite_json(value):
    if value is None or type(value) in (bool, int):
        return
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ProtocolError("invalid_json")
        return
    if isinstance(value, str):
        value.encode("utf-8", errors="strict")
        return
    if isinstance(value, list):
        for item in value:
            _finite_json(item)
        return
    if isinstance(value, dict):
        for key, item in value.items():
            if not isinstance(key, str):
                raise ProtocolError("invalid_json")
            _finite_json(key)
            _finite_json(item)
        return
    raise ProtocolError("invalid_json")


def wire_bytes(message):
    try:
        if not isinstance(message, dict):
            raise ProtocolError("invalid_json")
        _finite_json(message)
        raw = json.dumps(message, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8") + b"\n"
    except ProtocolError:
        raise
    except (TypeError, ValueError, UnicodeError, RecursionError):
        raise ProtocolError("invalid_json") from None
    if len(raw) > MAX_LINE:
        raise ProtocolError("line_too_large")
    return raw


def _reject_constant(_):
    raise ProtocolError("invalid_json")


def _unique_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ProtocolError("invalid_json")
        value[key] = item
    return value


def decode_line(raw):
    if not isinstance(raw, bytes) or not raw.endswith(b"\n") or raw.count(b"\n") != 1:
        raise ProtocolError("invalid_line")
    if len(raw) > MAX_LINE:
        raise ProtocolError("line_too_large")
    try:
        value = json.loads(raw.decode("utf-8", errors="strict"),
                           parse_constant=_reject_constant, object_pairs_hook=_unique_object)
        if not isinstance(value, dict):
            raise ProtocolError("invalid_json")
        _finite_json(value)
        return value
    except ProtocolError:
        raise
    except (TypeError, ValueError, UnicodeError, RecursionError):
        raise ProtocolError("invalid_json") from None


def validate_command(value, instance):
    if not isinstance(instance, str) or not _INSTANCE.fullmatch(instance):
        raise ProtocolError("invalid_instance")
    if not isinstance(value, dict) or type(value.get("protocol_version")) is not int or value["protocol_version"] != PROTOCOL_VERSION:
        raise ProtocolError("invalid_command")
    if value.get("instance_id") != instance:
        raise ProtocolError("instance_mismatch")
    op = value.get("op")
    fields = {"start": _BASE_KEYS | {"profile"},
              "encode": _BASE_KEYS | {"profile", "request_id", "role", "text", "input_sha256"},
              "stop": _BASE_KEYS}
    if not isinstance(op, str) or op not in fields or set(value) != fields[op]:
        raise ProtocolError("invalid_command")
    if op in ("start", "encode"):
        try:
            profile = json.dumps(value["profile"], sort_keys=True, separators=(",", ":"), allow_nan=False)
        except (TypeError, ValueError):
            raise ProtocolError("profile_mismatch") from None
        if profile != _PROFILE_JSON:
            raise ProtocolError("profile_mismatch")
    if op == "encode":
        if not isinstance(value["request_id"], str) or not _REQUEST.fullmatch(value["request_id"]):
            raise ProtocolError("invalid_command")
        if value["role"] not in ("query", "document") or not isinstance(value["text"], str):
            raise ProtocolError("invalid_command")
        try:
            digest = hashlib.sha256(value["text"].encode("utf-8", errors="strict")).hexdigest()
        except UnicodeError:
            raise ProtocolError("input_hash_mismatch") from None
        if not isinstance(value["input_sha256"], str) or not _HASH.fullmatch(value["input_sha256"]) or value["input_sha256"] != digest:
            raise ProtocolError("input_hash_mismatch")
    return op


def cpu_environment(inherited):
    environment = dict(inherited)
    for key in list(environment):
        if key.upper() in {"HF_TOKEN", "HUGGING_FACE_HUB_TOKEN", "HUGGINGFACE_HUB_TOKEN",
                           "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "FTP_PROXY"}:
            environment.pop(key)
    environment.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", HF_DATASETS_OFFLINE="1",
                       HF_HUB_DISABLE_TELEMETRY="1", DO_NOT_TRACK="1", USE_TORCH="1", USE_TF="0",
                       CUDA_VISIBLE_DEVICES="", TOKENIZERS_PARALLELISM="false", PYTHONNOUSERSITE="1",
                       OMP_NUM_THREADS="2", MKL_NUM_THREADS="2", OPENBLAS_NUM_THREADS="2",
                       NUMEXPR_NUM_THREADS="2", TRANSFORMERS_VERBOSITY="error")
    return environment


def _model_path(value):
    if not isinstance(value, str) or "\\" in value or "\0" in value or not PurePosixPath(value).is_absolute():
        raise ProtocolError("invalid_model_dir")
    return value


def _verify_asset(path, asset):
    _, size, git_oid, lfs_sha = asset
    if path.stat().st_size != size:
        raise StartupError("asset_mismatch")
    sha = hashlib.sha256()
    git = hashlib.sha1(("blob " + str(size) + "\0").encode("ascii"))
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            sha.update(block)
            git.update(block)
    if (sha.hexdigest() != lfs_sha) if lfs_sha else (git.hexdigest() != git_oid):
        raise StartupError("asset_mismatch")


def _elapsed(start):
    return max(0.0, (time.perf_counter() - start) * 1000.0)


def _timings(value, keys):
    if not isinstance(value, dict) or set(value) != keys or any(type(v) not in (int, float) or not math.isfinite(v) or v < 0 for v in value.values()):
        raise ProtocolError("invalid_output")
    return value


def _tokenize(tokenizer, text):
    started = time.perf_counter()
    encoded = tokenizer(text, add_special_tokens=True, padding=False, truncation=False,
                        return_attention_mask=True, return_token_type_ids=False)
    ids, mask = encoded["input_ids"], encoded["attention_mask"]
    if not isinstance(ids, list) or not isinstance(mask, list) or not ids or len(ids) != len(mask) \
            or any(type(v) is not int or v < 0 for v in ids) or any(type(v) is not int or v not in (0, 1) for v in mask):
        raise EncodeFailure()
    tokens = len(ids)
    if tokens > PROFILE["maximum_tokens"]:
        raise InputTooLong(tokens)
    return tokens, {"input_ids": ids, "attention_mask": mask}, _elapsed(started)


def _encoding(value):
    if not isinstance(value, dict) or set(value) != {"tokens", "vector", "timings"}:
        raise ProtocolError("invalid_output")
    tokens, vector = value["tokens"], value["vector"]
    if type(tokens) is not int or not 1 <= tokens <= 512 or not isinstance(vector, list) or len(vector) != 1024:
        raise ProtocolError("invalid_output")
    if any(type(v) not in (float, int) or not math.isfinite(v) for v in vector) \
            or abs(math.fsum(v * v for v in vector) - 1.0) > 0.001:
        raise ProtocolError("invalid_output")
    _timings(value["timings"], _ENCODE_TIMINGS)
    return value


class BgeEncoder:
    """Construction is the only asset audit, heavy import, model load and warmup."""
    def __init__(self, model_dir):
        total = time.perf_counter()
        _model_path(str(model_dir))
        environment = cpu_environment(os.environ)
        os.environ.clear()
        os.environ.update(environment)
        audit = time.perf_counter()
        try:
            directory = Path(model_dir).resolve(strict=True)
            if not directory.is_dir():
                raise StartupError("asset_mismatch")
            for asset in ASSETS:
                _verify_asset(directory / asset[0], asset)
        except Exception:
            raise StartupError("asset_mismatch") from None
        hash_ms = _elapsed(audit)
        loading = time.perf_counter()
        try:
            import torch
            torch.set_num_threads(2)
            torch.set_num_interop_threads(1)
            from transformers import AutoModel, AutoTokenizer
            from torch.nn import functional
            self.torch = torch
            self.functional = functional
            self.tokenizer = AutoTokenizer.from_pretrained(str(directory), local_files_only=True,
                                                           trust_remote_code=False, use_fast=True)
            if not self.tokenizer.is_fast:
                raise StartupError("local_model_load_failed")
            self.model = AutoModel.from_pretrained(str(directory), local_files_only=True,
                trust_remote_code=False, weights_only=True, use_safetensors=False,
                dtype=torch.float32, attn_implementation="eager")
            self.model.to(device="cpu", dtype=torch.float32).eval()
            if self.model.training or torch.get_num_threads() != 2 or torch.get_num_interop_threads() != 1 \
                    or not all(p.device.type == "cpu" and p.dtype == torch.float32 for p in self.model.parameters()):
                raise StartupError("local_model_load_failed")
        except Exception:
            raise StartupError("local_model_load_failed") from None
        load_ms = _elapsed(loading)
        warming = time.perf_counter()
        try:
            warm = self.encode(WARMUP_TEXT)
        except Exception:
            raise StartupError("warmup_failed") from None
        self.ready_info = {"warmup_tokens": warm["tokens"], "timings": {
            "hash_audit_ms": hash_ms, "model_load_ms": load_ms,
            "warmup_ms": _elapsed(warming), "total_ms": _elapsed(total)}}

    def encode(self, text):
        total = time.perf_counter()
        tokens = None
        try:
            tokens, inputs, tokenize_ms = _tokenize(self.tokenizer, text)
            torch = self.torch
            feed = {name: torch.tensor([values], dtype=torch.long, device="cpu") for name, values in inputs.items()}
            start = time.perf_counter()
            with torch.inference_mode():
                hidden = self.model(**feed).last_hidden_state
                if tuple(hidden.shape) != (1, tokens, 1024) or hidden.device.type != "cpu" or not bool(torch.isfinite(hidden).all()):
                    raise EncodeFailure(tokens)
                pooled = hidden[:, 0]
                if not bool((pooled.norm(dim=1) > 0).all()):
                    raise EncodeFailure(tokens)
                vector = self.functional.normalize(pooled, p=2, dim=1)[0].cpu().tolist()
            return _encoding({"tokens": tokens, "vector": vector, "timings": {
                "tokenize_ms": tokenize_ms, "inference_ms": _elapsed(start), "total_ms": _elapsed(total)}})
        except InputTooLong:
            raise
        except Exception:
            raise EncodeFailure(tokens) from None


def _base(instance, state):
    return {"protocol_version": PROTOCOL_VERSION, "instance_id": instance, "state": state}


def _emit(output, message):
    output.write(wire_bytes(message))
    output.flush()


def run(model_dir, instance, source, output, engine_factory=BgeEncoder):
    """Binary streams; fake engines implement ready_info and encode(text)."""
    if not isinstance(instance, str) or not _INSTANCE.fullmatch(instance):
        raise ProtocolError("invalid_instance")
    _model_path(str(model_dir))
    _emit(output, {**_base(instance, "loading"), "profile": PROFILE, "ready": False, "warmup_completed": False})
    try:
        engine = engine_factory(model_dir)
        info = engine.ready_info
        if not isinstance(info, dict) or set(info) != {"warmup_tokens", "timings"} \
                or type(info["warmup_tokens"]) is not int or not 1 <= info["warmup_tokens"] <= 512:
            raise StartupError("warmup_failed")
        _timings(info["timings"], _READY_TIMINGS)
        _emit(output, {**_base(instance, "ready"), "profile": PROFILE, "ready": True,
            "warmup_completed": True, "device": "cpu", "dtype": "float32", "intra_threads": 2,
            "inter_threads": 1, "cuda_visible_devices": "", "load_count": 1, **info})
    except StartupError as error:
        _emit(output, {**_base(instance, "failed"), "code": error.code})
        return 1
    except Exception:
        _emit(output, {**_base(instance, "failed"), "code": "local_model_load_failed"})
        return 1
    while True:
        try:
            raw = source.readline(MAX_LINE + 1)
            if raw == b"":
                break
            command = decode_line(raw)
            op = validate_command(command, instance)
            if op == "stop":
                break
            if op != "encode":
                raise ProtocolError("unexpected_start")
        except Exception:
            _emit(output, {**_base(instance, "failed"), "code": "invalid_owner_protocol"})
            return 2
        identity = {"request_id": command["request_id"], "role": command["role"],
                    "input_sha256": command["input_sha256"], "text_sha256": command["input_sha256"], "profile": PROFILE}
        try:
            encoded = _encoding(engine.encode(command["text"]))
            _emit(output, {**_base(instance, "encoded"), **identity, **encoded})
        except InputTooLong as error:
            tokens = error.tokens if type(error.tokens) is int and error.tokens > 512 else None
            _emit(output, {**_base(instance, "encode_error"), **identity,
                "code": "input_too_long" if tokens is not None else "encode_failed", "tokens": tokens})
        except Exception as error:
            tokens = error.tokens if isinstance(error, EncodeFailure) and type(error.tokens) is int and 1 <= error.tokens <= 512 else None
            _emit(output, {**_base(instance, "encode_error"), **identity, "code": "encode_failed", "tokens": tokens})
    _emit(output, {**_base(instance, "worker_stopped"), "ready": False, "warmup_completed": False})
    return 0


class _Parser(argparse.ArgumentParser):
    def error(self, _message):
        raise ProtocolError("invalid_arguments")


def main(argv=None):
    try:
        parser = _Parser(add_help=False)
        parser.add_argument("--instance", required=True)
        parser.add_argument("--model-dir", required=True)
        args = parser.parse_args(argv)
        if not _INSTANCE.fullmatch(args.instance):
            raise ProtocolError("invalid_instance")
        _model_path(args.model_dir)
        # Both Python prints and native library writes to fd 1 now go to stderr.
        # Only the duplicated original stdout is used for protocol messages.
        sys.stdout.flush()
        wire_fd = os.dup(sys.stdout.fileno())
        os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
        with os.fdopen(wire_fd, "wb", buffering=0) as output:
            return run(args.model_dir, args.instance, sys.stdin.buffer, output)
    except ProtocolError as error:
        sys.stderr.write(error.code + "\n")
        return 2
    except Exception:
        sys.stderr.write("worker_failed\n")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
