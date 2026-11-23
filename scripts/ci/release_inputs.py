#!/usr/bin/env python3
"""Fail closed on unresolved checked-in release inputs, before build or signing."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

import soma_input


def validate_sources(inputs):
    names = set()
    for item in inputs['siblings']:
        if not re.fullmatch('[A-Za-z0-9_-]+', item['path']) or item['path'] in names:
            raise ValueError('invalid or duplicate sibling path')
        names.add(item['path'])
        if 'overlay' in item:
            overlay = item['overlay']
            if (not re.fullmatch(r'ci/overlays/[a-z0-9-]+\.patch', overlay['path'])
                    or not re.fullmatch('[0-9a-f]{64}', overlay['sha256'])
                    or not re.fullmatch('[0-9a-f]{40}', overlay['tree'])):
                raise ValueError('invalid source overlay: ' + item['path'])
        if item['path'] == 'soma':
            soma_input.validate_pin(item)
            continue
        if (item['public_ref_verified'] is not True
                or not re.fullmatch(r'https://github.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(?:\.git)?', item['url'] or '')
                or not re.fullmatch('[0-9a-f]{40}', item['commit'])
                or not re.fullmatch('[0-9a-f]{40}', item['tree'])):
            raise ValueError('unresolved public sibling: ' + item['path'])
    for item in inputs['git_patches']:
        if item['public_ref_verified'] is not True:
            raise ValueError('unresolved Git dependency: ' + item['package'])
    if names != {'faculties','triblespace-rs','anybytes','jerky','mary','GORBIE','cubecl-fork','burn-fork','cubek-fork','soma','songbird'}:
        raise ValueError('unexpected source closure')


def validate(root, site, tag=None):
    inputs = json.loads((root / 'ci/source-inputs.json').read_text())
    pending = [key for key, value in inputs['activation_requirements'].items() if value is not True]
    if pending:
        raise ValueError('source activation requirements remain open: ' + ', '.join(pending))
    for filename, key in [('Cargo.lock', 'retained_lock_sha256'), ('scripts/app-icon/Cargo.lock', 'icon_lock_sha256')]:
        if hashlib.sha256((root / filename).read_bytes()).hexdigest() != inputs['app'][key]:
            raise ValueError('retained lock mismatch: ' + filename)
    for target in ('x86_64-unknown-linux-gnu','aarch64-unknown-linux-gnu'):
        if not (root/'ci/toolchains'/f'{target}.json').is_file():
            raise ValueError('retained native Rust1.98.0+wasm identity unavailable: '+target)
    validate_sources(inputs)
    if tag is not None:
        import sys
        sys.path.insert(0, str(site / 'scripts'))
        from source_version import source_version
        version = source_version(root)['version']
        if not re.fullmatch(r'0|[1-9][0-9]*', version.split('.')[0]) or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version):
            raise ValueError('standard release requires stable canonical SemVer')
        if tag != 'v' + version:
            raise ValueError('tag differs from actual shared package version')
        def git(*args):
            return subprocess.check_output(['git','-C',str(root),*args],text=True).strip()
        if git('rev-parse',tag+'^{commit}') != git('rev-parse','HEAD'):
            raise ValueError('tag does not bind source checkout')
        if git('status','--porcelain','--untracked-files=no'):
            raise ValueError('release source must be clean')
    return inputs


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app',type=Path,required=True)
    parser.add_argument('--site',type=Path)
    parser.add_argument('--tag')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--fetch-siblings',action='store_true')
    mode.add_argument('--fetch-development-sources', action='store_true',
                      help='fetch the pinned build inputs for a source checkout')
    args=parser.parse_args()
    try:
        if args.fetch_development_sources:
            if args.tag:
                raise ValueError('development source fetch does not accept a tag')
            inputs = json.loads((args.app / 'ci/source-inputs.json').read_text())
            validate_sources(inputs)
        else:
            inputs = validate(args.app, args.site, args.tag)
        if args.fetch_siblings or args.fetch_development_sources:
            for item in inputs['siblings']:
                path=args.app.parent/item['path']
                if path.exists(): raise ValueError('sibling output already exists: '+str(path))
                if item['path'] == 'soma':
                    soma_input.fetch(path, item)
                    continue
                subprocess.run(['git','init','--quiet',str(path)],check=True)
                def git(*v):
                    return subprocess.check_output(['git','-C',str(path),*v],text=True).strip()
                git('fetch','--depth=1',item['url'],item['commit'])
                git('checkout','--detach','FETCH_HEAD')
                if git('rev-parse','HEAD')!=item['commit'] or git('rev-parse','HEAD^{tree}')!=item['tree']:
                    raise ValueError('fetched source identity mismatch')
                if 'overlay' in item:
                    overlay = item['overlay']
                    patch = (args.app / overlay['path']).resolve(strict=True)
                    if hashlib.sha256(patch.read_bytes()).hexdigest() != overlay['sha256']:
                        raise ValueError('source overlay checksum mismatch')
                    git('apply', '--index', '--binary', str(patch))
                    if git('write-tree') != overlay['tree']:
                        raise ValueError('prepared source tree mismatch: ' + item['path'])
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as e:
        parser.exit(1,str(e)+'\n')
if __name__=='__main__': main()
