"""Explicit C# -> frozen Rust bridge -> local Qwen ASR/TTS consumer check."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import threading
import uuid


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--real-local-models', action='store_true', required=True)
    parser.add_argument('--voice-root', type=Path, required=True)
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    adapter = here.parent
    root = adapter.parent.parent
    bridge = root / 'runtime/goal-releases/20261109-aec-runtime/ai-tutor-voice-bridge.exe'
    corpus = root / 'runtime/goal-baselines/20261103/sapi-corpus'
    expected = {
        'zh_short.wav': 'ad72efd95a1abc540737554f9ccb63b7a9933effb75e5e458824a90363bb9a33',
        'en_short.wav': '670191397605e3f21f2b0e331852b665f91e5ee21cb9573f2ffdd93aff825a9f',
    }
    for name, digest in expected.items():
        assert sha(corpus / name) == digest, 'fixed_corpus_changed'
    assert sha(bridge) == 'a810538cd71adc5ec32b2a95fed4904118a6c406621e6be99eb57b55d982028b', 'bridge_identity_changed'
    voice_root = args.voice_root.resolve(strict=True)
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    output = root / 'runtime/goal-baselines/20261109/destiny-adapter-real' / (stamp + '-' + uuid.uuid4().hex[:8])
    output.mkdir(parents=True, exist_ok=False)
    frozen = output / 'sources'
    frozen.mkdir()
    source_files = sorted(adapter.glob('*.cs')) + [here / 'RealVoiceConsumer.cs']
    manifest = {'bridge': str(bridge), 'bridge_sha256': sha(bridge), 'voice_root': str(voice_root),
                'corpus': expected, 'sources': {}, 'policies_before': {}, 'cloud_calls': 0}
    for source in source_files:
        shutil.copyfile(source, frozen / source.name)
        manifest['sources'][str(source.relative_to(root))] = sha(source)
    policy_names = ['local-speech-policy.json', 'local-final-asr-policy.json', 'gpu-resource-policy.json']
    for name in policy_names:
        manifest['policies_before'][name] = sha(voice_root / 'runtime' / name)
    (output / 'manifest.json').write_text(json.dumps(manifest, indent=2), encoding='utf-8')
    executable = output / 'RealVoiceConsumer.exe'
    compiler = Path(os.environ['WINDIR']) / 'Microsoft.NET/Framework64/v4.0.30319/csc.exe'
    command = [str(compiler), '/nologo', '/target:exe', '/langversion:5', '/optimize+',
               '/reference:System.Web.Extensions.dll', '/out:' + str(executable)]
    build = subprocess.run(command + [str(frozen / path.name) for path in source_files],
                           cwd=root, capture_output=True, creationflags=subprocess.CREATE_NO_WINDOW)
    (output / 'compile.log').write_bytes(build.stdout + build.stderr)
    if build.returncode:
        print(json.dumps({'passed': False, 'stage': 'compile', 'evidence': str(output)}), flush=True)
        return 1
    report = output / 'result.json'
    print('EVIDENCE ' + str(output), flush=True)
    process = subprocess.Popen([str(executable), '--real-local-models', str(bridge), str(voice_root), str(corpus), str(report)],
                               cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, encoding='utf-8', creationflags=subprocess.CREATE_NO_WINDOW)
    stderr_count = [0]
    def drain():
        for block in iter(lambda: process.stderr.read(4096), ''):
            stderr_count[0] += len(block)
    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    unexpected = 0
    with (output / 'progress.log').open('x', encoding='utf-8') as progress:
        for line in process.stdout:
            if line.startswith(('PASS ', 'FAIL ', 'WAIT ')):
                print(line.rstrip(), flush=True)
                progress.write(line)
                progress.flush()
            else:
                unexpected += 1
    code = process.wait()
    reader.join(timeout=5)
    result = json.loads(report.read_text(encoding='utf-8')) if report.exists() else {'passed': False, 'failure': 'result_missing'}
    after = {name: sha(voice_root / 'runtime' / name) for name in policy_names}
    audit = {'exit_code': code, 'policy_unchanged': after == manifest['policies_before'],
             'stderr_characters': stderr_count[0], 'unexpected_stdout_lines': unexpected,
             'passed': code == 0 and result['passed'] and after == manifest['policies_before'] and not unexpected and not stderr_count[0]}
    (output / 'runner.json').write_text(json.dumps(audit, indent=2), encoding='utf-8')
    print(json.dumps({'passed': audit['passed'], 'evidence': str(output)}), flush=True)
    return 0 if audit['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
