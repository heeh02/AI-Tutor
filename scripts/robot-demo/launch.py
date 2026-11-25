"""Launch the local Rust Demo as an app window or a normal web page."""
import argparse
import ctypes
import json
import os
from pathlib import Path
import subprocess
import time
import urllib.request
import webbrowser


def descriptor(root):
    try:
        data = json.loads((root/'runtime/server.json').read_text(encoding='utf-8'))
        port, token = int(data['port']), data['token']
        if not (0 < port < 65536 and isinstance(token,str) and len(token)==64): return None
        opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
        request=urllib.request.Request(f'http://127.0.0.1:{port}/api/state',headers={'Cookie':f'ai_tutor_local={token}'})
        with opener.open(request,timeout=2) as response:
            if response.status!=200: return None
        return data
    except (OSError,ValueError,KeyError,TypeError): return None


def edge_path():
    for variable in ['ProgramFiles(x86)','ProgramFiles','LOCALAPPDATA']:
        base=os.environ.get(variable)
        if base:
            path=Path(base)/'Microsoft/Edge/Application/msedge.exe'
            if path.is_file(): return path
    return None


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root',type=Path,default=Path(__file__).resolve().parent)
    parser.add_argument('--port',type=int,default=8879)
    modes=parser.add_mutually_exclusive_group()
    modes.add_argument('--web',action='store_true')
    modes.add_argument('--no-open',action='store_true')
    modes.add_argument('--stop',action='store_true')
    args=parser.parse_args()
    root=args.root.resolve(strict=True)
    exe=root/'bin/ai-tutor.exe'
    if args.stop:
        # An unresponsive state endpoint does not mean the service has stopped.
        # Let the native stop command authenticate and request normal shutdown.
        if not (root/'runtime/server.json').exists(): return 0
        subprocess.run([str(exe),'--stop','--root',str(root)],check=True,timeout=20,
                       stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,creationflags=subprocess.CREATE_NO_WINDOW)
        deadline=time.monotonic()+120
        while (root/'runtime/server.json').exists() and time.monotonic()<deadline: time.sleep(.4)
        if (root/'runtime/server.json').exists(): raise RuntimeError('本地服务仍在关闭，请稍后检查。')
        return 0
    state=descriptor(root)
    if state is None:
        (root/'runtime').mkdir(exist_ok=True)
        with (root/'runtime/demo-start.log').open('ab') as log:
            subprocess.Popen([str(exe),'--root',str(root),'--port',str(args.port),'--no-browser'],
                             stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT,
                             creationflags=subprocess.CREATE_NO_WINDOW)
        deadline=time.monotonic()+35
        while state is None and time.monotonic()<deadline:
            # A concurrent launcher can lose the native instance lock and exit
            # while the winning service is still starting. Wait for that service
            # within the same deadline rather than reporting a false failure.
            time.sleep(.3); state=descriptor(root)
        if state is None: raise RuntimeError('工作台启动失败，请检查 runtime/startup-error.json。')
    # The local login token never leaves this process as a console/log value.
    url=f"http://127.0.0.1:{state['port']}/?token={state['token']}"
    if args.no_open:
        print(json.dumps({'running':True,'pid':state['pid'],'url':f"http://127.0.0.1:{state['port']}/"}))
        return 0
    edge=edge_path()
    if args.web or edge is None:
        webbrowser.open(url,new=2)
    else:
        profile=root/'runtime/browser-profile'
        profile.mkdir(exist_ok=True)
        subprocess.Popen([str(edge),'--no-first-run','--no-default-browser-check',
                          '--user-data-dir='+str(profile),'--app='+url],stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,
                         creationflags=subprocess.CREATE_NO_WINDOW)
    return 0


if __name__=='__main__':
    try: raise SystemExit(main())
    except Exception:
        ctypes.windll.user32.MessageBoxW(None,'工作台操作没有完成。请检查 Demo 目录中的 runtime/startup-error.json，或使用“停止 AI Tutor”后重新打开。','AI Tutor',0x10)
        raise SystemExit(1)
