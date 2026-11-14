#!/usr/bin/env python3
"""Run the fixed source-owned native probe on the final distributed package.

Mac uses the existing noninteractive AppKit-termination acceptance path. This
is runtime acceptance, not observed menu-click, migration, or clean-install QA.
Linux adapts the retained Xvfb/X11 software-Mesa probe; no Wayland/UI claim.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile


def notices(packaged, source):
    def inventory(root):
        result={}
        for p in root.rglob('*'):
            if p.is_symlink(): raise ValueError('symlinked notice')
            if p.is_file(): result[str(p.relative_to(root))]=p.read_bytes()
        return result
    expected=inventory(source)
    if not {'manifest.json','README.txt'} <= expected.keys() or inventory(packaged)!=expected:
        raise ValueError('packaged notices differ from selected source')


def inspect_appimage(artifact,target,app,destination):
    import importlib.util
    script=app/'scripts/linux/appimage/scripts/verify-package.py'
    spec=importlib.util.spec_from_file_location('appimage_inspection',script)
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    arch=target.removesuffix('-unknown-linux-gnu')
    module.extract(artifact,arch,destination)
    notices(destination/'usr/share/licenses/ai.erlkonig.app/notices',app/'notices')
    if (destination/'AppRun').read_bytes()!=(app/'scripts/linux/appimage/packaging/AppRun').read_bytes():
        raise ValueError('packaged AppRun differs from selected source')



def probe(artifact,target,app,fixture,root):
    if target not in ('x86_64-unknown-linux-gnu','aarch64-unknown-linux-gnu','aarch64-apple-darwin'): raise ValueError('unsupported native target')
    if root.exists() or not root.is_absolute(): raise ValueError('new absolute evidence root required')
    root.mkdir(parents=True)
    if target in ('x86_64-unknown-linux-gnu','aarch64-unknown-linux-gnu'):
        script=app/'scripts/accept/linux_isolated_app.py'
        if not script.is_file():
            raise ValueError('missing accepted Linux first-run/forwarder/shutdown probe: scripts/accept/linux_isolated_app.py')
        import platform,os
        arch=target.removesuffix('-unknown-linux-gnu')
        if sys.platform!='linux' or platform.machine()!=arch:
            raise ValueError('AppImage probe requires matching native Linux architecture')
        if not os.access('/dev/fuse',os.R_OK|os.W_OK):
            raise ValueError('native AppImage mounting requires accessible /dev/fuse')
        with tempfile.TemporaryDirectory(prefix='erlkonig-release-') as temp:
            inspect_appimage(artifact,target,app,Path(temp)/'payload')
            # Launch the exact final AppImage, not the extracted binary. A static
            # inspection result can never produce this runtime acceptance receipt.
            subprocess.run(['dbus-run-session','--','xvfb-run','-a',sys.executable,str(script),str(artifact),
                            '--fixture-binary',str(fixture),'--root',str(root/'runtime')],check=True)
    else:
        if sys.platform!='darwin': raise ValueError('macOS probe requires macOS')
        mount=root/'mount'; mount.mkdir()
        attempted=False
        try:
            attempted=True
            subprocess.run(['hdiutil','attach','-readonly','-nobrowse','-mountpoint',str(mount),str(artifact)],check=True)
            package=mount/'Erlkonig.app'
            notices(package/'Contents/Resources/notices',app/'notices')
            subprocess.run(['codesign','--verify','--deep','--strict',str(package)],check=True)
            subprocess.run([sys.executable,str(app/'scripts/accept/isolated_app.py'),str(package),'--fixture-binary',str(fixture),'--root',str(root/'runtime')],check=True)
        finally:
            if attempted:
                # Even on probe failure the exact owned mount is detached. A
                # failed detach fails the receipt; never delete a mounted tree.
                subprocess.run(['hdiutil','detach',str(mount)],check=True)
    result=(root/'runtime/result.json').read_bytes()
    if json.loads(result)['status']!='passed': raise ValueError('native probe result did not pass')
    print(json.dumps({'runtime_result_sha256':hashlib.sha256(result).hexdigest(),'scope':'fixed native lifecycle probe on packaged executable'}))


def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('artifact','app','fixture','root'): p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--target',choices=['x86_64-unknown-linux-gnu','aarch64-unknown-linux-gnu','aarch64-apple-darwin'],required=True)
    a=p.parse_args()
    try: probe(a.artifact.resolve(),a.target,a.app.resolve(),a.fixture.resolve(),a.root)
    except (ValueError,KeyError,OSError,subprocess.CalledProcessError) as e: p.exit(1,str(e)+'\n')
if __name__=='__main__': main()
