"""Build the Rust workbench and its local 3D renderer without a shell build script."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    frontend = Path(__file__).resolve().parent
    root = frontend.parent
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    assets = output / 'assets'
    assets.mkdir(exist_ok=True)
    bindgen = root / 'runtime/tools/wasm-bindgen/wasm-bindgen-0.2.100-x86_64-pc-windows-msvc/wasm-bindgen.exe'
    subprocess.run(['cargo','build','--locked','-p','tutor-ui','--target-dir','target/ui',
                    '--target','wasm32-unknown-unknown','--release'], cwd=root, check=True)
    subprocess.run([str(bindgen),'--target','web','--no-typescript','--out-dir',str(assets),
                    '--out-name','tutor_ui',str(root/'target/ui/wasm32-unknown-unknown/release/tutor-ui.wasm')],
                   cwd=root, check=True)
    shutil.copyfile(frontend/'index.html',output/'index.html')
    for name in ['style.css','home.css','activity-gallery.css','parent-center.css','home-environment.css','layout.css','bootstrap.js','robot-companion.js','home-environment.js','app-icon.svg','manifest.webmanifest']:
        shutil.copyfile(frontend/name,assets/name)
    for source in (frontend/'vendor/three').iterdir():
        if source.is_file():
            target=assets/'vendor/three'/source.name
            target.parent.mkdir(parents=True,exist_ok=True)
            shutil.copyfile(source,target)
    hashes={str(p.relative_to(output)):hashlib.sha256(p.read_bytes()).hexdigest()
            for p in output.rglob('*') if p.is_file() and p.name!='build-manifest.json'}
    (output/'build-manifest.json').write_text(json.dumps(hashes,indent=2),encoding='utf-8')
    print(json.dumps({'output':str(output),'files':len(hashes)}))


if __name__=='__main__': main()
