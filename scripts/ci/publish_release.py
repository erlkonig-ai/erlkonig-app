#!/usr/bin/env python3
"""Finalize only same-run accepted native packages; dispatch after public verify.

GH_TOKEN is scoped to this repository's release writes. SITE_DISPATCH_TOKEN is
an external, separately scoped capability; its credential binding is deliberately
not invented in the inactive workflow. Neither is written to disk or logged.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from urllib.parse import quote

REPO='erlkonig-ai/erlkonig-app'
TARGETS=('aarch64-apple-darwin','x86_64-unknown-linux-gnu','aarch64-unknown-linux-gnu')


def gh(*args, **kwargs):
    return subprocess.check_output(['gh',*args],**kwargs)


def transfer(source, destination):
    destination.mkdir()
    records=[]
    for target in TARGETS:
        root=source/target
        record=json.loads((root/'record.json').read_text())
        if record['target']!=target: raise ValueError('artifact transfer target mismatch')
        expected_signing='developer-id-notarized' if target==TARGETS[0] else 'unsigned'
        if record['signing']!=expected_signing: raise ValueError('artifact transfer signing mismatch')
        names=[record['file'],record['acceptance_file']]
        if target==TARGETS[0]: names.append(record['signing_file'])
        if set(p.name for p in root.iterdir())!=set(names+['record.json']):
            raise ValueError('unexpected transfer contents')
        for name in names:
            p=root/name
            if Path(name).name!=name or name in ('','.','..') or p.is_symlink() or not p.is_file():
                raise ValueError('invalid transfer filename')
            with (destination/name).open('xb') as out, p.open('rb') as source_file:
                shutil.copyfileobj(source_file,out)
        records.append(record)
    return records


def publish(app,site,transfers,out,tag,run_id,attempt):
    # No public release may be created if its downstream capability is absent.
    if not os.environ.get('GH_TOKEN') or not os.environ.get('SITE_DISPATCH_TOKEN'):
        raise ValueError('release-write and existing cross-repository dispatch capabilities must be bound before publication')
    from release_inputs import validate
    validate(app,site,tag)
    if json.loads(gh('api','repos/'+REPO))['visibility']!='public':
        raise ValueError('release repository is not public')
    out.mkdir()
    packages=out/'packages'
    records=transfer(transfers,packages)
    record_path=out/'artifact-records.json'; record_path.write_text(json.dumps(records))
    # Fresh draft only; gh refuses an existing release. No clobber, replacement,
    # automatic tag creation, or promotion of arbitrary PR/main artifacts.
    notes=out/'release-notes.txt'
    notes.write_text('Standard native release. Package checksums and source/build provenance are recorded in release-manifest.json.\n')
    gh('release','create',tag,'--repo',REPO,'--draft','--verify-tag','--title',tag,'--notes-file',str(notes))
    assets=[str(p) for p in sorted(packages.iterdir())]
    gh('release','upload',tag,*assets,'--repo',REPO)
    body=gh('api',f'repos/{REPO}/releases/tags/'+quote(tag,safe=''))
    release=json.loads(body)
    if not release['draft'] or release['prerelease'] or release['tag_name']!=tag:
        raise ValueError('not the new stable draft')
    release_path=out/'draft-release.json'; release_path.write_bytes(body)
    manifest=out/'release-manifest.json'
    subprocess.run([sys.executable,str(site/'scripts/produce_release_manifest.py'),
        '--release',str(release_path),'--source-root',str(app),'--artifacts',str(packages),
        '--records',str(record_path),'--run-id',str(run_id),'--run-attempt',str(attempt),'--output',str(manifest)],check=True)
    gh('release','upload',tag,str(manifest),'--repo',REPO)
    gh('api','--method','PATCH',f'repos/{REPO}/releases/{release["id"]}','-F','draft=false','-f','make_latest=false')
    sys.path.insert(0,str(site/'scripts'))
    from refresh_downloads import collect
    from render_downloads import catalog
    incoming=collect(release['id'])
    selected,_=catalog(incoming)
    latest=selected['default_release_id']
    if latest is None: raise ValueError('verified stable release selection is empty')
    # Same SemVer selection as site/app; a later backport never displaces a
    # higher complete stable release. Publication jobs serialize across tags.
    gh('api','--method','PATCH',f'repos/{REPO}/releases/{latest}','-f','make_latest=true')
    if json.loads(gh('api',f'repos/{REPO}/releases/latest'))['id']!=latest:
        raise ValueError('GitHub latest did not match verified stable selection')
    (out/'published-release-id.txt').write_text(str(release['id'])+'\n')
    dispatch(release['id'])


def dispatch(release_id):
    token=os.environ.get('SITE_DISPATCH_TOKEN')
    if not token: raise ValueError('existing cross-repository dispatch capability is unbound')
    # Fixed configured destination and event; this updates site source only. Actual
    # provider publishing and its branch binding are deliberately separate.
    env=os.environ.copy(); env['GH_TOKEN']=token
    body=json.dumps({'event_type':'erlkonig-release-complete','client_payload':{'release_id':release_id}}).encode()
    gh('api','--method','POST','repos/erlkonig-ai/sites/dispatches','--input','-',input=body,env=env)


def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('app','site','transfers','output'): p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--tag',required=True)
    p.add_argument('--run-id',type=int,required=True); p.add_argument('--run-attempt',type=int,required=True)
    a=p.parse_args()
    try: publish(a.app.resolve(),a.site.resolve(),a.transfers.resolve(),a.output,a.tag,a.run_id,a.run_attempt)
    except (ValueError,KeyError,OSError,subprocess.CalledProcessError) as e: p.exit(1,str(e)+'\n')
if __name__=='__main__': main()
