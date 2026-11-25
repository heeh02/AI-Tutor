"""Start one loopback-only workstation and open its authenticated local page."""
import argparse
import asyncio
import json
import multiprocessing
import os
from pathlib import Path
import secrets
import socket
import sys
import webbrowser

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))


async def serve(port, browser):
    import uvicorn
    from ai_tutor.main import create_app
    token = secrets.token_urlsafe(32)
    app = create_app(token=token)
    config = uvicorn.Config(app, host="127.0.0.1", port=port, log_level="warning", access_log=False)
    server = uvicorn.Server(config)
    runtime = ROOT / "runtime"
    runtime.mkdir(exist_ok=True)
    descriptor = runtime / "server.json"
    descriptor.write_text(json.dumps({"pid": os.getpid(), "port": port, "token": token}), encoding="utf-8")

    async def ready():
        while not server.started:
            if server.should_exit:
                return
            await asyncio.sleep(.15)
        if browser:
            webbrowser.open(f"http://127.0.0.1:{port}/?token={token}")
        print(f"AI Tutor ready: http://127.0.0.1:{port} (use Start AI Tutor to authenticate)", flush=True)
        await app.state.workstation.shutdown.wait()
        server.should_exit = True

    companion = asyncio.create_task(ready())
    try:
        await server.serve()
    finally:
        companion.cancel()
        await asyncio.gather(companion, return_exceptions=True)
        if descriptor.exists():
            descriptor.unlink()


def main():
    parser = argparse.ArgumentParser()
    defaults = json.loads((ROOT / "configs/default.json").read_text(encoding="utf-8"))
    parser.add_argument("--port", type=int, default=defaults["default_port"])
    parser.add_argument("--no-browser", action="store_true")
    args = parser.parse_args()
    if not 1024 <= args.port <= 65535:
        parser.error("port must be between1024 and65535")
    # Don't replace another app or kill an unknown listener.
    with socket.socket() as test:
        try:
            test.bind(("127.0.0.1", args.port))
        except OSError:
            print("Port in use. Use Stop AI Tutor for this app or choose --port.")
            return 1
    asyncio.run(serve(args.port, not args.no_browser))
    return 0


if __name__ == "__main__":
    multiprocessing.freeze_support()
    raise SystemExit(main())
