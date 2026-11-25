"""Hidden launcher; reuse only the authenticated descriptor for this app."""
import json
import subprocess
import sys
import urllib.request
import webbrowser
from pathlib import Path

root = Path(__file__).resolve().parents[1]
descriptor = root / "runtime/server.json"
running = False
if descriptor.exists():
    info = json.loads(descriptor.read_text(encoding="utf-8"))
    try:
        request = urllib.request.Request(f"http://127.0.0.1:{info['port']}/api/state", headers={"Cookie": "ai_tutor_local="+info["token"]})
        with urllib.request.urlopen(request, timeout=2) as response:
            running = response.status == 200
        if running:
            webbrowser.open(f"http://127.0.0.1:{info['port']}/?token={info['token']}")
    except OSError:
        pass
if not running:
    (root / "runtime").mkdir(exist_ok=True)
    with (root / "runtime/server.log").open("a", encoding="utf-8") as logfile:
        subprocess.Popen([sys.executable, str(root / "scripts/start.py")], cwd=root,
                         creationflags=subprocess.CREATE_NO_WINDOW, stdout=logfile, stderr=logfile, stdin=subprocess.DEVNULL)
