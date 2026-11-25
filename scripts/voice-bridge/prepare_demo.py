"""Stage a new voice-only root for ai-tutor-voice-bridge.exe; start nothing."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil


TTS_MODEL = Path('models/speech/Qwen3-TTS-12Hz-0.6B-CustomVoice')
TTS_REVISION = '85e237c12c027371202489a0ec509ded67b5e4b5'
ASR_MODEL = Path('models/speech/Qwen3-ASR-1.7B-7278e1e7')
ASR_REVISION = '7278e1e70fe206f11671096ffdd38061171dd6e5'
TTS_BUNDLE = ('managed_tts.py', 'tts_gpu_lease.sh', 'gpu_admission.py',
              'cuda_link_env.sh', 'start_qwen_omni.sh', 'qwen3_tts_12gb.yaml')
ASR_BUNDLE = ('managed_final_asr.py', 'qwen_final_engine.py',
              'qwen_final_service.py', 'qwen-model.json', 'qwen-sdk.json')
TTS_FILES = {
    'config.json', 'generation_config.json', 'merges.txt', 'model.safetensors',
    'preprocessor_config.json', 'tokenizer_config.json', 'vocab.json',
    'speech_tokenizer/config.json', 'speech_tokenizer/configuration.json',
    'speech_tokenizer/model.safetensors', 'speech_tokenizer/preprocessor_config.json',
}
DEFAULT_ASR_PYTHON = '/mnt/c/Users/RBMS/.cache/ai-tutor-asr-17b-env-20261106/bin/python'
DEFAULT_ASR_CACHE = '/mnt/c/Users/RBMS/.cache/ai-tutor-asr-17b-runtime'


def linux_path(value):
    path = PurePosixPath(value)
    if (not path.is_absolute() or '..' in path.parts
            or any(char in value for char in ('\0', '\n', '\r', '\\'))):
        raise ValueError('Expected an absolute Linux path without traversal or newlines')
    return value


def model_file(root, folder, name):
    path = PurePosixPath(name)
    if (not name or path.is_absolute() or any(p in ('.', '..') for p in path.parts)
            or any(char in name for char in ('\\', ':', '\0'))):
        raise ValueError('Invalid model asset name')
    source = root / folder / name
    if not source.resolve(strict=True).is_relative_to((root / folder).resolve(strict=True)):
        raise ValueError('Model asset escapes its source directory')
    return source


def verified_asset(path, size, expected, git_blob=False):
    if not path.is_file() or path.stat().st_size != size:
        raise ValueError(f'Model asset missing or wrong size: {path.name}')
    digest = hashlib.sha1() if git_blob else hashlib.sha256()
    if git_blob:
        digest.update(f'blob {size}\0'.encode('ascii'))
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(4 * 1024 * 1024), b''):
            digest.update(block)
    if digest.hexdigest() != expected:
        raise ValueError(f'Model asset checksum mismatch: {path.name}')


def model_assets(root):
    """Reuse the existing acquisition manifests, without generating a new one."""
    tts = json.loads((root / TTS_MODEL / 'download-manifest.json').read_text(encoding='utf-8'))
    asr = json.loads((root / 'scripts/final-asr/qwen-model.json').read_text(encoding='utf-8'))
    if (tts.get('model_id') != 'Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice'
            or tts.get('revision') != TTS_REVISION
            or asr.get('model_id') != 'Qwen/Qwen3-ASR-1.7B'
            or asr.get('model_revision') != ASR_REVISION):
        raise ValueError('Existing fixed Qwen model revisions do not match')
    tts_rows = [row for row in tts['files'] if row['file'] in TTS_FILES]
    asr_rows = [row for row in asr['files'] if row['runtime_required'] is True]
    if (len(tts_rows) != len(TTS_FILES) or {r['file'] for r in tts_rows} != TTS_FILES
            or len(asr_rows) != 10 or len({r['path'] for r in asr_rows}) != 10):
        raise ValueError('Required model asset list changed')
    assets = []
    for row in tts_rows:
        source = model_file(root, TTS_MODEL, row['file'])
        verified_asset(source, row['size'], row['sha256'])
        assets.append((source, TTS_MODEL / row['file']))
    for row in asr_rows:
        if PurePosixPath(row['path']).name != row['path']:
            raise ValueError('Final ASR requires a flat model asset name')
        source = model_file(root, ASR_MODEL, row['path'])
        verified_asset(source, row['size_bytes'], row['lfs_sha256'] or row['git_blob_sha1'],
                       git_blob=not row['lfs_sha256'])
        assets.append((source, ASR_MODEL / row['path']))
    return assets


def prepare(args):
    if not args.source_root.is_absolute() or not args.voice_root.is_absolute():
        raise ValueError('source-root and voice-root must be absolute paths')
    root = args.source_root.resolve(strict=True)
    destination = args.voice_root
    # A new leaf prevents overwriting a profile. Refuse redirected target parents.
    for ancestor in (destination, *destination.parents):
        if ancestor.exists() or ancestor.is_symlink():
            if ancestor.is_symlink() or getattr(ancestor.lstat(), 'st_file_attributes', 0) & 0x400:
                raise ValueError('voice-root must not traverse a symlink or reparse point')
    destination = destination.resolve()
    if destination.exists():
        raise ValueError('voice-root must be a new directory; existing roots are never reused')
    if root.drive.lower() != destination.drive.lower():
        raise ValueError('voice-root must be on the source volume for model hardlinks')
    python = linux_path(args.final_asr_python)
    cache = PurePosixPath(linux_path(args.final_asr_cache))
    if cache.parts[:3] != ('/', 'mnt', 'c') or not cache.name.startswith('ai-tutor-'):
        raise ValueError('final-asr-cache must be under /mnt/c with an ai-tutor- basename')
    if not args.distribution.strip() or any(c in args.distribution for c in ('\0', '\n', '\r')):
        raise ValueError('A WSL distribution name is required')
    pointer = args.tts_environment or (root / 'runtime/speech-gpu/omni-env-path.txt').read_text(encoding='utf-8').strip()
    pointer = linux_path(pointer)
    copies = [(root / 'scripts/speech-lab' / name, Path('bin/local-speech') / name)
              for name in TTS_BUNDLE]
    copies += [(root / 'scripts/final-asr' / name, Path('bin/final-asr') / name)
               for name in ASR_BUNDLE]
    copies.append((root / 'scripts/speech-lab/gpu_admission.py', Path('bin/final-asr/gpu_admission.py')))
    for source, _ in copies:
        if not source.is_file():
            raise ValueError(f'Missing runtime helper: {source.name}')
    assets = model_assets(root)
    destination.mkdir(parents=True, exist_ok=False)
    for source, relative in copies + assets:
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        if (source, relative) in assets:
            os.link(source, target)  # Never copy multi-gigabyte weights or fall back to a copy.
            if not source.samefile(target):
                raise OSError('Model hardlink verification failed')
        else:
            shutil.copyfile(source, target)
    runtime = destination / 'runtime'
    (runtime / 'speech-gpu').mkdir(parents=True)
    # bash reads this line directly; a Windows CRLF leaves a literal CR in PATH.
    (runtime / 'speech-gpu/omni-env-path.txt').write_bytes((pointer + '\n').encode('utf-8'))
    policies = {
        'local-speech-policy.json': {
            'protocol_version': 1, 'mode': 'managed', 'warmup': 'synthetic_bilingual',
            'managed': {'engine': 'qwen', 'distribution': args.distribution}},
        'local-final-asr-policy.json': {
            'protocol_version': 1, 'enabled': True,
            'managed': {'distribution': args.distribution, 'python_executable': python,
                        'cache_directory': str(cache)}},
        'gpu-resource-policy.json': {'protocol_version': 1, 'mode': 'qwen_pair_12gb_v1'},
    }
    for name, policy in policies.items():
        with (runtime / name).open('x', encoding='utf-8') as stream:
            json.dump(policy, stream, ensure_ascii=False, indent=2)
            stream.write('\n')
    return destination, len(copies), len(assets)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-root', type=Path, required=True, help='Existing AI Tutor checkout (absolute)')
    parser.add_argument('--voice-root', type=Path, required=True, help='New directory on the source volume (absolute)')
    parser.add_argument('--distribution', default='Ubuntu', help='Existing WSL distribution (default: Ubuntu)')
    parser.add_argument('--final-asr-python', default=DEFAULT_ASR_PYTHON)
    parser.add_argument('--final-asr-cache', default=DEFAULT_ASR_CACHE)
    parser.add_argument('--tts-environment', help='Existing Linux TTS environment; default: source-root pointer')
    args = parser.parse_args()
    try:
        root, helpers, models = prepare(args)
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f'Preparation failed: {error}\nAny new partial voice-root is retained; use another new path.\n')
    print(f'Prepared {root}: {helpers} helpers, {models} verified model hardlinks. No service started.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
