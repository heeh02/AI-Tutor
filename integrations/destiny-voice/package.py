"""Build a standalone adapter DLL and source handoff; never installs or starts it."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    adapter = Path(__file__).resolve().parent
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    source_root = output / 'source'
    source_root.mkdir()
    sources = sorted(adapter.glob('*.cs'))
    files = {}
    for path in sources:
        shutil.copyfile(path, source_root / path.name)
        files[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
    shutil.copyfile(adapter / 'README.md', output / 'README.md')
    compiler = Path(os.environ['WINDIR']) / 'Microsoft.NET/Framework64/v4.0.30319/csc.exe'
    binary = output / 'SoulLink.LocalVoice.dll'
    result = subprocess.run([str(compiler), '/nologo', '/target:library', '/langversion:5', '/optimize+',
                             '/reference:System.Web.Extensions.dll', '/out:' + str(binary)]
                            + [str(source_root / path.name) for path in sources],
                            capture_output=True, creationflags=subprocess.CREATE_NO_WINDOW)
    (output / 'compile.log').write_bytes(result.stdout + result.stderr)
    report = {'compiled': result.returncode == 0, 'source_sha256': files,
              'target': '.NET Framework 4.5+ / C#5', 'namespace': 'SoulLink.Desktop.Voice',
              'installed': False, 'destiny_integrated': False,
              'rust_bridge_bundled': False, 'models_bundled': False, 'credentials_bundled': False}
    if result.returncode == 0:
        report['dll_sha256'] = hashlib.sha256(binary.read_bytes()).hexdigest()
        report['dll_bytes'] = binary.stat().st_size
    (output / 'manifest.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
    print(json.dumps({'compiled': report['compiled'], 'output': str(output)}))
    return 0 if result.returncode == 0 else 1


if __name__ == '__main__':
    raise SystemExit(main())
