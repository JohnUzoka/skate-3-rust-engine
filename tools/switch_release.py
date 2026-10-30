#!/usr/bin/env python3
"""Makes the files an over-the-air Switch update needs, for a GitHub release tagged
switch-v<version> (docs/switch/README.md, "Publishing an update"):

    python3 tools/switch_release.py <skate3rust.nro> <version> <folder>

<folder> gets skate3rust.nro, a copy of the NRO, and switch-release.json, which names the version
and the asset set the NRO reads (switch_assets.json, beside this script; the NRO has it built in).
It checks first that the NRO's NACP shows <version>, which build_game.sh's VERSION sets: the
Switch installs a release's NRO only if it shows the release's version. Upload both files to the
release. GitHub records each file's SHA-256, and the Switch checks its download against it.
Python 3.8 or later, nothing else.
"""
import hashlib
import json
import re
import shutil
import struct
import sys
from pathlib import Path

ASSETS = Path(__file__).resolve().with_name('switch_assets.json')
REPOSITORY = 'JohnUzoka/skate-3-rust-engine'
TAG_PREFIX = 'switch-v'
NRO = 'skate3rust.nro'
MANIFEST = 'switch-release.json'
# The switch-release.json schema crates/skate-switch-update reads.
SCHEMA = 1


def nacp_version(path):
    """The version the NACP of the NRO at `path` shows (libnx's nro.h and nacp.h)."""
    data = path.read_bytes()
    if data[0x10:0x14] != b'NRO0':
        sys.exit(f'error: {path} is not an NRO')
    image = struct.unpack_from('<I', data, 0x18)[0]
    if data[image:image + 4] != b'ASET':
        sys.exit(f'error: {path} has no asset section; build_game.sh adds one')
    offset, size = struct.unpack_from('<QQ', data, image + 0x18)
    nacp = data[image + offset:image + offset + size]
    if size != 0x4000 or len(nacp) != size:
        sys.exit(f'error: {path} has no whole NACP')
    return nacp[0x3060:0x3070].split(b'\0')[0].decode('utf-8', 'replace')


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    nro, version, folder = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
    if version.startswith(TAG_PREFIX):
        version = version[len(TAG_PREFIX):]
    if not re.fullmatch(r'\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?', version):
        sys.exit(f'error: {version!r} is not a version such as 0.1.5 or 0.1.5-rc1')
    shown = nacp_version(nro)
    if shown != version:
        sys.exit(f'error: {nro} shows version {shown!r}, not {version!r}: build it with VERSION={version}')
    assets = json.loads(ASSETS.read_text(encoding='utf-8'))
    folder.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(nro, folder / NRO)
    (folder / MANIFEST).write_text(json.dumps({'schema': SCHEMA, 'version': version, **assets}, indent=2) + '\n',
                                   encoding='utf-8')
    digest = hashlib.sha256((folder / NRO).read_bytes()).hexdigest()
    print(f'{folder / NRO}: version {version}, sha256 {digest}')
    print(f'{folder / MANIFEST}: the asset set of PC {assets["pc_build"]}')
    print(f'Upload both to the release {TAG_PREFIX}{version}, for example:\n'
          f'  gh release upload {TAG_PREFIX}{version} {folder / NRO} {folder / MANIFEST} --repo {REPOSITORY}')


if __name__ == '__main__':
    main()
