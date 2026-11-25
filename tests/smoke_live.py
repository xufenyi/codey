"""Explicit opt-in: two tiny real Codex turns in a read-only scratch project.

This consumes the signed-in Codex account's quota and creates one conversation.
Not run by CI. No tools or file edits are requested.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.parse
import urllib.request
import urllib.error

if '--run' not in sys.argv:
    raise SystemExit('Opt-in required: python tests/smoke_live.py --run (uses real Codex quota)')
root=Path(__file__).resolve().parents[1]
data=root/'test-data'/'live-smoke'
project=data/'project'
project.mkdir(parents=True,exist_ok=True)
binary=root/'target'/'debug'/('codey.exe' if os.name=='nt' else 'codey')
process=subprocess.Popen([str(binary),'--headless','--no-browser','--data-dir',str(data/'data')],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
api=None
try:
    until=time.monotonic()+15
    while time.monotonic()<until:
        if process.poll() is not None:raise RuntimeError(process.stderr.read().decode(errors='replace'))
        try:
            info=json.loads((data/'data'/'session.json').read_text(encoding='utf-8'))
            parts=urllib.parse.urlsplit(info['url'])
            base=f'{parts.scheme}://{parts.netloc}'
            token=urllib.parse.parse_qs(parts.fragment)['token'][0]
            break
        except (FileNotFoundError,json.JSONDecodeError):time.sleep(.1)
    else:raise RuntimeError('Backend did not start')
    def api(path,body=None):
        req=urllib.request.Request(base+'/api/'+path,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
        try:
            with urllib.request.urlopen(req,timeout=65) as response:return json.load(response)
        except urllib.error.HTTPError as error:
            detail=error.read().decode(errors='replace');error.close();raise RuntimeError(detail) from None
    result=api('runtime',{'path':''})
    if result.get('bindingError'):raise RuntimeError(result['bindingError'])
    models=result['models']['data']
    model=next((m for m in models if m.get('isDefault')),models[0])
    efforts=[e['reasoningEffort'] for e in model.get('supportedReasoningEfforts',[])]
    effort='low' if 'low' in efforts else None
    thread=None
    for mode in ['new','existing']:
        body={'title':'Codey live smoke / '+mode,'cwd':str(project),'prompt':'Reply with exactly CODEY_SMOKE_OK. Do not use any tools. Do not read or modify files.','threadMode':mode,'threadId':thread,'sendMode':'time','sendAt':int(time.time()*1000)+1000,'stalePolicy':'confirm' if thread else 'send','missedPolicy':'send','sandbox':'read-only','approvalPolicy':'never','networkAccess':False}
        if not thread:body.update(model=model['model'],effort=effort)
        job=api('jobs',body)['job']
        until=time.monotonic()+180
        while time.monotonic()<until:
            current=next(j for j in api('state')['jobs'] if j['id']==job['id'])
            if current['status']=='completed':break
            if current['status'] in ['review','failed','paused','confirmation','interrupted']:raise RuntimeError(f"{mode}: {current['status']}: {current['reason']}")
            time.sleep(.5)
        else:raise RuntimeError(f'{mode} timed out')
        if 'CODEY_SMOKE_OK' not in current['output']:raise RuntimeError('Completion did not include the expected answer')
        thread=current['threadId']
        print(json.dumps({'mode':mode,'status':current['status'],'answerMatched':True},ensure_ascii=False),flush=True)
finally:
    if process.poll() is None:
        try:
            if api:api('shutdown',{'mode':'interrupt'})
            process.wait(timeout=30)
        except Exception:process.kill();process.wait()
    process.stderr.close()
