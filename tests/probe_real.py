"""Read-only opt-in probe of the installed Codex. Does not start an AI turn."""
import json
import os
from pathlib import Path
import subprocess
import time
import urllib.parse
import urllib.request

root=Path(__file__).resolve().parents[1]
data=root/'test-data'/'real-probe'
binary=root/'target'/'debug'/('codey.exe' if os.name=='nt' else 'codey')
process=subprocess.Popen([str(binary),'--headless','--no-browser','--data-dir',str(data)],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
try:
    deadline=time.monotonic()+15
    while time.monotonic()<deadline:
        if process.poll() is not None:
            raise RuntimeError(process.stderr.read().decode(errors='replace'))
        try:
            session=json.loads((data/'session.json').read_text(encoding='utf-8'))
            parts=urllib.parse.urlsplit(session['url'])
            base=f'{parts.scheme}://{parts.netloc}'
            token=urllib.parse.parse_qs(parts.fragment)['token'][0]
            break
        except (FileNotFoundError,json.JSONDecodeError):
            time.sleep(.1)
    else:
        raise RuntimeError('Backend did not start')
    def api(path,body):
        req=urllib.request.Request(base+'/api/'+path,data=json.dumps(body).encode(),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
        try:
            with urllib.request.urlopen(req,timeout=60) as response:return json.load(response)
        except urllib.error.HTTPError as error:
            message=error.read().decode(errors='replace');error.close();raise RuntimeError(message) from None
    result=api('runtime',{'path':''})
    threads=api('threads',{})
    print(json.dumps({'runtime':result['runtime'],'accountType':(result['account'].get('account') or {}).get('type'),'accountBindingAvailable':not bool(result.get('bindingError')),'models':len(result['models'].get('data',[])),'threadsOnFirstPage':len(threads.get('data',[])),'aiTurnsStarted':0},ensure_ascii=False,indent=2))
finally:
    if process.poll() is None:
        try:
            api('shutdown',{'mode':'interrupt'})
            process.wait(timeout=15)
        except Exception:
            process.kill();process.wait()
    process.stderr.close()
