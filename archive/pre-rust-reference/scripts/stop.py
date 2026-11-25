import json
from pathlib import Path
import urllib.request

path = Path(__file__).resolve().parents[1] / "runtime/server.json"
if not path.exists():
    print("AI Tutor is not running.")
else:
    info = json.loads(path.read_text(encoding="utf-8"))
    request = urllib.request.Request(f"http://127.0.0.1:{info['port']}/api/shutdown", data=b"{}", headers={"Content-Type": "application/json", "Cookie": "ai_tutor_local="+info["token"]}, method="POST")
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            print("AI Tutor is releasing its camera, microphone and child processes.")
    except OSError:
        print("Could not contact AI Tutor. No other process was stopped.")
