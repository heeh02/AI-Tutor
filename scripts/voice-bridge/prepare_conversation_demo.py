"""Stage a separate AI Tutor conversation Demo from existing local assets; start nothing."""
import argparse
import json
import os
from pathlib import Path
import shutil
from types import SimpleNamespace

import prepare_demo


UI_FILES = ('index.html', 'assets/bootstrap.js', 'assets/style.css',
            'assets/tutor_ui.js', 'assets/tutor_ui_bg.wasm')
PROVIDER_FILES = ('prompts/tutor_core.md', 'prompts/tutor-agent.md',
    'implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json',
    'runtime/kimi/verification-rust.json')
CPU_HELPERS = ('managed_asr.py', 'asr_service.py', 'recognizer.py')
CPU_MODEL = Path('models/speech/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20')
CPU_FILES = {
    'tokens.txt': 'a8e0e4ec53810e433789b54a5c0134a7eaa2ffca595a6334d54c00da858841d3',
    'encoder-epoch-99-avg-1.int8.onnx': '8fa764187a261844f859d7143ebaa563af5d10adfece4c18a8f414c88cba2a9b',
    'decoder-epoch-99-avg-1.onnx': '2e3b5ec371f8899ee6acd829fd753ba45772df57a91bdf37cde3136354e7db7d',
    'joiner-epoch-99-avg-1.int8.onnx': '1ed689c5ed19dbaa725d9d191bb4822b5f4855a39e1ffd28cbc1f340d25b2ee0',
}
CAMERA_FILES = {
    'models/yolov8n-face-lindevs.onnx': '8d0bfb0c3383c5bd7a78dd24ef79a21e2aa456619b6ab5e53867092d1c7dc414',
    'models/native/onnxruntime.dll': '2bc04b05efbff97049132939640963772cc16225ba1c7f1388993487c8775492',
    'models/native/onnxruntime_providers_shared.dll': '6adee08db23ba190fd1061ee99c32a5a802cac1e3576defaac820c157294c138',
}


def existing_absolute(path, description, directory=False):
    if not path.is_absolute():
        raise ValueError(f'{description} must be an absolute path')
    path = path.resolve(strict=True)
    if not (path.is_dir() if directory else path.is_file()):
        raise ValueError(f'{description} is not an existing {"directory" if directory else "file"}')
    return path


def additional_inputs(args):
    """Check all conversation additions before the shared voice stager creates a root."""
    root = existing_absolute(args.source_root, 'source-root', directory=True)
    executable = existing_absolute(args.executable, 'executable')
    ui = existing_absolute(args.ui_dir, 'ui-dir', directory=True)
    if not args.demo_root.is_absolute() or args.demo_root.exists():
        raise ValueError('demo-root must be a new absolute directory')
    python = existing_absolute(args.partial_asr_python or
        root / 'runtime/speech-lab/.venv/Scripts/python.exe', 'partial-asr-python')
    copies = [(executable, Path('bin/ai-tutor.exe'))]
    copies += [(ui / name, Path('frontend/dist') / name) for name in UI_FILES]
    # Keep local renderers, icons and their licenses alongside the WASM app.
    required_ui = set(UI_FILES)
    for source in sorted((ui / 'assets').rglob('*')):
        if source.is_file() and not source.is_symlink():
            relative = source.relative_to(ui)
            if relative.as_posix() not in required_ui:
                copies.append((source, Path('frontend/dist') / relative))
    copies += [(root / name, Path(name)) for name in PROVIDER_FILES]
    copies += [(root / 'scripts/speech-service' / name, Path('bin/local-asr') / name)
               for name in CPU_HELPERS]
    for source, _ in copies:
        if not source.is_file():
            raise ValueError(f'Missing conversation input: {source.name}')
    links = [(root / CPU_MODEL / name, CPU_MODEL / name) for name in CPU_FILES]
    for source, relative in links:
        prepare_demo.verified_asset(source, source.stat().st_size, CPU_FILES[relative.name])
    if args.with_camera:
        for name, expected in CAMERA_FILES.items():
            source = root / name
            prepare_demo.verified_asset(source, source.stat().st_size, expected)
            links.append((source, Path(name)))
    whisper = root / 'models/ggml-base.bin'
    include_whisper = not args.no_whisper_fallback and whisper.is_file()
    if include_whisper:
        manifest = json.loads((root / 'models/whisper-rust-manifest.json').read_text(encoding='utf-8'))
        if manifest['file'] != 'models/ggml-base.bin':
            raise ValueError('Whisper fallback manifest changed its destination')
        prepare_demo.verified_asset(whisper, manifest['bytes'], manifest['sha256'])
        links.append((whisper, Path('models/ggml-base.bin')))
    return root, python, copies, links, include_whisper


def prepare(args):
    root, python, copies, links, include_whisper = additional_inputs(args)
    destination, voice_helpers, voice_models = prepare_demo.prepare(SimpleNamespace(
        source_root=root, voice_root=args.demo_root, distribution=args.distribution,
        final_asr_python=args.final_asr_python, final_asr_cache=args.final_asr_cache,
        tts_environment=args.tts_environment))
    for source, relative in copies:
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
    for source, relative in links:
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        os.link(source, target)
        if not source.samefile(target):
            raise OSError('Model hardlink verification failed')
    policies = {
        'local-asr-policy.json': {'protocol_version': 1, 'mode': 'managed',
                                 'python_executable': str(python)},
        'teaching-provider-policy.json': {'protocol_version': 1,
            'provider': 'deepseek_flash', 'thinking': 'non_thinking'},
        'tutor-delivery.json': {'protocol_version': 1, 'mode': 'segments',
                                'segment_speech': 'after_display'},
    }
    for name, value in policies.items():
        with (destination / 'runtime' / name).open('x', encoding='utf-8', newline='\n') as stream:
            json.dump(value, stream, ensure_ascii=False, indent=2)
            stream.write('\n')
    return {'demo_root': str(destination), 'executable': str(destination / 'bin/ai-tutor.exe'),
            'voice_helpers': voice_helpers, 'voice_model_hardlinks': voice_models,
            'conversation_files': len(copies), 'additional_model_hardlinks': len(links),
            'camera_assets': args.with_camera, 'whisper_fallback': include_whisper,
            'background_memory_enabled': False, 'database_created': False,
            'credentials_copied': False, 'service_started': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-root', type=Path, required=True)
    parser.add_argument('--demo-root', type=Path, required=True, help='New root on source model volume')
    parser.add_argument('--executable', type=Path, required=True, help='Explicit built ai-tutor.exe')
    parser.add_argument('--ui-dir', type=Path, required=True, help='Explicit built UI directory')
    parser.add_argument('--partial-asr-python', type=Path,
                        help='Existing Windows Python; default: source runtime/speech-lab/.venv')
    parser.add_argument('--distribution', default='Ubuntu')
    parser.add_argument('--final-asr-python', default=prepare_demo.DEFAULT_ASR_PYTHON)
    parser.add_argument('--final-asr-cache', default=prepare_demo.DEFAULT_ASR_CACHE)
    parser.add_argument('--tts-environment', help='Existing Linux environment; defaults to source pointer')
    parser.add_argument('--with-camera', action='store_true',
                        help='Link cached camera assets for later manual opening; never opens a camera')
    parser.add_argument('--no-whisper-fallback', action='store_true',
                        help='Omit optional cached Whisper CPU fallback')
    args = parser.parse_args()
    try:
        result = prepare(args)
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f'Preparation failed: {error}\nAny partial new root is retained; use a fresh path.\n')
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
