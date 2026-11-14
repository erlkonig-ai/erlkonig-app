#!/usr/bin/env python3
"""Acquire only release assets named in the retained metadata; never execute."""
import hashlib,json,os,pathlib,sys,urllib.request
root=pathlib.Path(__file__).resolve().parents[1]
arch=sys.argv[1]
if arch not in ['aarch64','x86_64']: raise SystemExit('expected aarch64 or x86_64')
out=pathlib.Path(os.environ['APPIMAGE_WORK_ROOT'])/'tools'/arch;out.mkdir(parents=True,exist_ok=True)
receipt=[]
for repo,name in [('appimagetool','appimagetool-'+arch+'.AppImage'),('type2-runtime','runtime-'+arch)]:
    metadata=json.loads((root/'evidence'/(repo+'-release-metadata.json')).read_text())
    asset=next(a for a in metadata['assets'] if a['name']==name)
    digest=asset['digest'].removeprefix('sha256:')
    url='https://api.github.com/repos/'+metadata['repository']+'/releases/assets/'+str(asset['id'])
    destination=out/name
    if not destination.exists():
        request=urllib.request.Request(url,headers={'Accept':'application/octet-stream','User-Agent':'Erlkonig-packaging-candidate','X-GitHub-Api-Version':'2022-11-28'})
        with urllib.request.urlopen(request) as response: data=response.read()
        if len(data)!=asset['size'] or hashlib.sha256(data).hexdigest()!=digest: raise SystemExit('asset hash/size mismatch: '+name)
        destination.write_bytes(data)
    data=destination.read_bytes()
    if len(data)!=asset['size'] or hashlib.sha256(data).hexdigest()!=digest: raise SystemExit('retained asset hash/size mismatch: '+name)
    receipt.append({'file':name,'sha256':digest,'size':len(data),'asset_id':asset['id'],'acquisition_url':url,'release_url':metadata['html_url'],'source_commit':metadata['target_commitish']})
(pathlib.Path(os.environ['APPIMAGE_WORK_ROOT'])/'evidence'/('tool-downloads-'+arch+'.json')).write_text(json.dumps(receipt,indent=2)+'\n')
