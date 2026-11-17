"""Verify already-provisioned exact Linux compiler+wasm bytes; never install."""
import argparse,hashlib,json
from pathlib import Path

def verify(app, target, root):
    inventory=app/'ci/toolchains'/f'{target}.json'
    if not inventory.is_file(): raise ValueError(f'retained Rust1.98.0+wasm inventory unavailable for {target}')
    data=json.loads(inventory.read_text())
    arch=target.removesuffix('-unknown-linux-gnu')
    if data['version']!='1.98.0' or data['arch']!=arch: raise ValueError('toolchain identity mismatch')
    files=data['files']
    if not {'bin/rustc','bin/cargo'} <= files.keys() or not any(n.startswith('lib/rustlib/wasm32-unknown-unknown/lib/') for n in files):
        raise ValueError('compiler/cargo/wasm identity incomplete')
    for name, record in files.items():
        if Path(name).is_absolute() or '..' in Path(name).parts: raise ValueError('invalid toolchain inventory path')
        path=root/name
        if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest()!=record['sha256']:
            raise ValueError('retained toolchain bytes differ: '+name)
    return data
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--app',type=Path,required=True);p.add_argument('--root',type=Path,required=True);p.add_argument('--target',required=True);a=p.parse_args()
    try:verify(a.app,a.target,a.root)
    except (ValueError,OSError,KeyError) as e:p.exit(1,str(e)+'\n')
