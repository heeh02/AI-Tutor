"""Fixed local Qwen1.7B, offline final, no devices or vLLM.

Heavy imports happen only in load(). The dedicated service worker serializes
transcribe(). HTTP cancellation invalidates results; it does not stop CUDA.
"""
from dataclasses import dataclass
import contextlib
import hashlib
import importlib.abc
import importlib.metadata
import json
import os
from pathlib import Path
import sys

MODEL_ID = 'Qwen/Qwen3-ASR-1.7B'
MODEL_REVISION = '7278e1e70fe206f11671096ffdd38061171dd6e5'
SDK_COMMIT = '7c6daf77a2421100f5fb066495372c00129d39ff'
MODEL_DIRECTORY = 'models/speech/Qwen3-ASR-1.7B-7278e1e7'
MAX_PCM_BYTES = 640000
MAX_NEW_TOKENS = 512
BUNDLE = Path(__file__).resolve().parent


@dataclass(frozen=True)
class FinalResult:
    text: str
    detected_language: str
    generation_calls: tuple

    def payload(self):
        calls = [dict(item) for item in self.generation_calls]
        return {
            'text': self.text, 'detected_language': self.detected_language,
            'model_id': MODEL_ID, 'model_revision': MODEL_REVISION,
            'sdk_commit': SDK_COMMIT, 'backend': 'transformers',
            'precision': 'bfloat16', 'attention': 'sdpa',
            'max_new_tokens': MAX_NEW_TOKENS, 'generation_calls': calls,
            'possible_truncation': any(item['hit_token_limit'] and
                                       item['ended_with_eos'] is not True for item in calls),
        }


class InferenceCancelled(Exception):
    """Private cooperative cancellation; never a publishable final or engine fault."""
    def __init__(self, generation_calls=()):
        super().__init__('local_asr_canceled')
        self.generation_calls = tuple(generation_calls)


class ExcludeVllm(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname, path=None, target=None):
        if fullname == 'vllm' or fullname.startswith('vllm.') or fullname == 'qwen_asr.core.vllm_backend' or fullname.startswith('qwen_asr.core.vllm_backend.'):
            raise ModuleNotFoundError('vllm excluded from offline-final engine', name=fullname)
        return None


def _verified_json(path, digest):
    raw = path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError('fixed_manifest_hash_mismatch')
    return json.loads(raw)


class QwenFinalEngine:
    provider = 'Qwen3-ASR-1.7B / PyTorch BF16 / local offline final'

    def __init__(self, root):
        self.root = Path(root).resolve(strict=True)
        self.model_path = (self.root / MODEL_DIRECTORY).resolve(strict=True)
        if not self.model_path.is_relative_to(self.root):
            raise ValueError('model_outside_fixed_project_root')
        self.asr = None
        self._calls = None
        self._cancelled = None
        self._cancel_seen = False

    def _request_cancelled(self):
        if not self._cancel_seen and self._cancelled is not None:
            self._cancel_seen = bool(self._cancelled())
        return self._cancel_seen

    def load(self):
        if self.asr is not None:
            raise RuntimeError('engine_already_loaded')
        plan = _verified_json(BUNDLE / 'qwen-model.json',
                              'a1aeda7dd900d4cafeca85f1c166fcf26e6b459a4401255afa602720b1a9ec83')
        if plan['model_id'] != MODEL_ID or plan['model_revision'] != MODEL_REVISION:
            raise ValueError('wrong_model_manifest')
        # Preparation verifies all ten runtime assets once; never per request.
        for item in plan['files']:
            if not item['runtime_required']:
                continue
            if Path(item['path']).name != item['path']:
                raise ValueError('invalid_asset_name')
            path = self.model_path / item['path']
            if path.stat().st_size != item['size_bytes']:
                raise ValueError('model_asset_size_mismatch')
            digest = hashlib.sha256() if item['lfs_sha256'] else hashlib.sha1()
            if not item['lfs_sha256']:
                digest.update(f"blob {item['size_bytes']}\0".encode('ascii'))
            with path.open('rb') as source:
                for block in iter(lambda: source.read(4 * 1024 * 1024), b''):
                    digest.update(block)
            if digest.hexdigest() != (item['lfs_sha256'] or item['git_blob_sha1']):
                raise ValueError('model_asset_hash_mismatch')
        sdk = _verified_json(BUNDLE / 'qwen-sdk.json',
                             '17167049ae5bd88c0988895967080a4129716a0aabc953dd1ecd588ea8447121')
        if sdk['sdk_commit'] != SDK_COMMIT:
            raise ValueError('wrong_sdk_manifest')
        installed = importlib.metadata.distribution('qwen-asr')
        for name, expected in sdk['files'].items():
            if name.startswith('qwen_asr/') and name.endswith('.py'):
                if hashlib.sha256(installed.locate_file(name).read_bytes()).hexdigest() != expected:
                    raise ValueError('installed_sdk_hash_mismatch')
        if importlib.metadata.version('transformers') != '4.57.6':
            raise ValueError('unsupported_transformers_version')
        if any(name == 'vllm' or name.startswith('vllm.') for name in sys.modules):
            raise RuntimeError('vllm_already_loaded')
        os.environ.update(HF_HUB_OFFLINE='1', TRANSFORMERS_OFFLINE='1',
                          HF_HUB_DISABLE_TELEMETRY='1', TOKENIZERS_PARALLELISM='false')
        sys.meta_path.insert(0, ExcludeVllm())
        with contextlib.redirect_stdout(sys.stderr):
            import numpy as np
            import torch
            from qwen_asr import Qwen3ASRModel
            from transformers import StoppingCriteria, StoppingCriteriaList
            engine = self

            class CancelAtTokenBoundary(StoppingCriteria):
                def __call__(self, input_ids, scores, **kwargs):
                    # No state lock, score capture, or CUDA work interruption.
                    return torch.full((input_ids.shape[0],), engine._request_cancelled(),
                                      device=input_ids.device, dtype=torch.bool)
            if not torch.cuda.is_available() or not torch.cuda.is_bf16_supported():
                raise RuntimeError('bf16_cuda_required')
            torch.set_num_threads(2)
            self.np, self.torch = np, torch
            self.asr = Qwen3ASRModel.from_pretrained(
                str(self.model_path), dtype=torch.bfloat16, device_map='cuda:0',
                attn_implementation='sdpa', local_files_only=True, forced_aligner=None,
                max_inference_batch_size=1, max_new_tokens=MAX_NEW_TOKENS)
            self.asr.model.eval()
            self.asr.model.generation_config.do_sample = False
            if {str(p.device) for p in self.asr.model.parameters()} != {'cuda:0'}:
                raise RuntimeError('model_not_entirely_on_cuda0')
            original_generate = self.asr.model.generate

            def observed_generate(*args, **kwargs):
                if self._request_cancelled():
                    raise InferenceCancelled(self._calls or ())
                if self._cancelled is not None:
                    # Copy the caller list; retain default EOS/max-length merging.
                    criteria = StoppingCriteriaList(list(kwargs.get('stopping_criteria') or ()))
                    criteria.append(CancelAtTokenBoundary())
                    kwargs['stopping_criteria'] = criteria
                generated = original_generate(*args, **kwargs)
                sequences = getattr(generated, 'sequences', generated)
                if sequences.shape[0] != 1 or self._calls is None:
                    raise RuntimeError('unexpected_generation_batch')
                ids = sequences[0, kwargs['input_ids'].shape[-1]:].detach().cpu().tolist()
                eos = self.asr.model.generation_config.eos_token_id
                eos_ids = {eos} if isinstance(eos, int) else set(eos or ())
                self._calls.append({
                    'new_tokens': len(ids), 'hit_token_limit': len(ids) >= MAX_NEW_TOKENS,
                    'ended_with_eos': bool(ids and ids[-1] in eos_ids) if eos_ids else None,
                    'raw_decoder_text': self.asr.processor.tokenizer.decode(ids, skip_special_tokens=False),
                })
                return generated

            self.asr.model.generate = observed_generate
            torch.cuda.synchronize()
        if any(name == 'vllm' or name.startswith('vllm.') for name in sys.modules):
            raise RuntimeError('vllm_import_violation')

    def warm(self, *, cancelled=None):
        # Real inference, including CUDA completion, before warm_ready.
        # Silence warmup is not a claim that all input lengths meet latency goals.
        result = self.transcribe(bytearray(16000 * 2), cancelled=cancelled)
        if result.payload()['possible_truncation']:
            raise RuntimeError('warm_decoder_truncated')
        return result

    def transcribe(self, pcm, *, cancelled=None):
        if self.asr is None or not 0 < len(pcm) <= MAX_PCM_BYTES or len(pcm) % 2:
            raise ValueError('engine_not_loaded_or_invalid_pcm')
        if self._calls is not None:
            raise RuntimeError('concurrent_engine_call')
        if cancelled is not None and not callable(cancelled):
            raise TypeError('cancelled_must_be_callable')
        waveform = None
        self._calls, self._cancelled, self._cancel_seen = [], cancelled, False
        try:
            if self._request_cancelled():
                raise InferenceCancelled()
            waveform = self.np.frombuffer(pcm, dtype='<i2').astype(self.np.float32) / 32768.0
            with contextlib.redirect_stdout(sys.stderr), self.torch.inference_mode():
                outputs = self.asr.transcribe(audio=(waveform, 16000), context='',
                                              language=None, return_time_stamps=False)
                self.torch.cuda.synchronize()
            if self._request_cancelled():
                raise InferenceCancelled(self._calls)
            if len(outputs) != 1 or not self._calls:
                raise RuntimeError('missing_single_final_or_generation_metadata')
            return FinalResult(outputs[0].text, outputs[0].language, tuple(self._calls))
        finally:
            try:
                # Also drain on the exception path. A failed synchronization is
                # an engine failure; service stays unavailable until process exit.
                self.torch.cuda.synchronize()
            finally:
                if waveform is not None:
                    waveform.fill(0)
                self._calls = None
                self._cancelled = None
                self._cancel_seen = False
