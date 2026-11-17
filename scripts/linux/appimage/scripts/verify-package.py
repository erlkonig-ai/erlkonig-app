#!/usr/bin/env python3
"""Adapted retained static ELF/SquashFS check; never execute app/runtime."""
import hashlib,json,os,stat,struct,subprocess
from pathlib import Path

MACHINES={'aarch64':183,'x86_64':62}
def elf_header(path,arch,appimage=False):
    with path.open('rb') as f: header=f.read(64)
    if len(header)!=64 or header[:6]!=b'\x7fELF\x02\x01' or struct.unpack_from('<H',header,18)[0]!=MACHINES[arch]:
        raise ValueError('unexpected ELF architecture')
    if not appimage:return None
    if header[8:11]!=b'AI\x02':raise ValueError('not a type2 AppImage')
    section_offset=struct.unpack_from('<Q',header,40)[0]
    section_size,section_count=struct.unpack_from('<HH',header,58)
    offset=section_offset+section_size*section_count
    with path.open('rb') as f:f.seek(offset); magic=f.read(4)
    if magic!=b'hsqs':raise ValueError('SquashFS must begin at runtime ELF end')
    return offset

def extract(package,arch,destination):
    if destination.exists():raise ValueError('new extraction directory required')
    offset=elf_header(package,arch,True)
    subprocess.run(['/usr/bin/unsquashfs','-processors','2','-no-progress','-o',str(offset),'-d',str(destination),str(package)],check=True)
    for p in destination.rglob('*'):
        if p.is_symlink() and not p.resolve().is_relative_to(destination.resolve()):raise ValueError('escaping packaged symlink')
    elf_header(destination/'usr/bin/erlkonig',arch)
    return offset

def inventory(directory):
    entries={}
    for p in sorted(directory.rglob('*')):
        name=str(p.relative_to(directory))
        if p.is_symlink():entries[name]={'symlink':os.readlink(p)}
        elif p.is_file():entries[name]={'sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'size':p.stat().st_size,'mode':oct(stat.S_IMODE(p.stat().st_mode))}
    return entries

def inspect(root,arch,package):
    extracted=root/'package-inspection';offset=extract(package,arch,extracted)
    expected=inventory(root/'AppDir');actual=inventory(extracted)
    if actual!=expected:raise ValueError('archive bytes/modes differ from staged AppDir')
    receipt={'scope':'Static ELF/SquashFS inspection only; no runtime/app/client execution','architecture':arch,'package_sha256':hashlib.sha256(package.read_bytes()).hexdigest(),'package_size':package.stat().st_size,'squashfs_offset':offset,'entry_count':len(actual),'entries':actual}
    (root/'evidence/package-inspection.json').write_text(json.dumps(receipt,indent=2)+'\n')
if __name__=='__main__':
    import argparse
    p=argparse.ArgumentParser();p.add_argument('--root',type=Path,required=True);p.add_argument('--arch',choices=MACHINES,required=True);p.add_argument('--package',type=Path,required=True);a=p.parse_args();inspect(a.root,a.arch,a.package)
