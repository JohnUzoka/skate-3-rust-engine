#!/usr/bin/env python3
"""Copies the PC game's set-up files onto a Switch SD card, for the Switch build
(docs/switch/README.md).

    python switch_export.py <PC game folder> <SD card>

The PC game folder has skate3rust.exe and data/ in it, after the PC game has set up your disc
and reached its menu. The SD card is its root (E:\\ on Windows, /media/... elsewhere) or any
folder that stands for one, such as an emulator's. Files land in <SD card>/switch/skate3rust/:
assets/, maps/, maps.json and settings/, and skate3rust.nro when it sits beside this script.
The PC's settings/graphics.json is left out: the Switch keeps its own. Running it again copies
only what changed. switch_assets.json, beside this script, names the PC build whose files the
Switch build reads. Python 3.8 or later, nothing else.
"""
import argparse
import json
import os
import re
import shutil
import sys
from pathlib import Path

# The asset set the Switch build reads: the asset pipelines of the PC build it was made and tested
# with, which its NRO has built in too. Its updater installs only releases that read the same set.
ASSETS = Path(__file__).resolve().with_name('switch_assets.json')
TARGET = Path('switch', 'skate3rust')
NRO = 'skate3rust.nro'
LEFT_OUT = {Path('settings', 'graphics.json')}


def read_json(path):
    try:
        return json.loads(path.read_text(encoding='utf-8-sig'))
    except (OSError, ValueError):
        return None


def installation(folder):
    """The set-up installation's root and its marker, as the PC game finds them (versions.py)."""
    for base in (folder / 'data', folder):
        marker = read_json(base / 'installation.json')
        if marker is None or 'directory' not in marker:
            continue
        directory = marker['directory']
        if marker.get('version') != 1 or not re.fullmatch(r'installations/[0-9a-f]{32}', directory):
            sys.exit(f'error: {base / "installation.json"} is not an installation record')
        root = base / directory
        if not root.is_dir():
            sys.exit(f'error: {root} is missing; start the PC game so it sets up again')
        return base, root, marker
    sys.exit(f'error: no data/installation.json in {folder}. Give the PC game folder, the one '
             'with skate3rust.exe, after the game has set up your disc.')


def receipt_present(root, files):
    """Each recorded file is there at its recorded size (setup.rs `receipt_present`)."""
    if not isinstance(files, dict) or not files:
        return False
    for name, entry in files.items():
        relative = Path(name)
        if relative.is_absolute() or relative.drive or '..' in relative.parts:
            return False
        try:
            if os.path.getsize(root / relative) != entry.get('size'):
                return False
        except OSError:
            return False
    return True


def unready_groups(marker, root, release):
    """The asset groups the PC game would set up again before it starts (setup.rs
    `pipelines_acceptable`): those whose fingerprint differs, without an equivalence pair from
    the PC release, and whose recorded files aren't all there."""
    installed = marker.get('pipelines') or {}
    outputs = marker.get('outputs') or {}
    expected = release.get('asset_pipelines') or {}
    equivalence = release.get('pipeline_equivalence') or {}
    return [group for group, new in expected.items()
            if installed.get(group) != new
            and [installed.get(group), new] not in equivalence.get(group, [])
            and not receipt_present(root, outputs.get(group))]


def check(folder):
    switch = read_json(ASSETS)
    if not isinstance(switch, dict) or not isinstance(switch.get('asset_pipelines'), dict):
        sys.exit(f'error: {ASSETS} is missing or unreadable; it comes with this script')
    pc_build = switch.get('pc_build', '?')
    base, root, marker = installation(folder)
    print(f'PC installation: {root}')
    problems = []
    if not (root / 'assets/private/game.json').is_file():
        problems.append('assets/private/game.json is missing')
    for group, files in (marker.get('outputs') or {}).items():
        if not receipt_present(root, files):
            problems.append(f'some of the {group} files are missing or changed')
    release = read_json(base.parent / 'release.json')
    if release is None:
        print(f'  note: no release.json beside data/, so the PC build is unknown; this Switch '
              f'build matches PC {pc_build}')
    else:
        groups = unready_groups(marker, root, release)
        if groups:
            problems.append(f'the PC game still has to set up: {", ".join(groups)}')
        different = sorted(group for group, value in switch['asset_pipelines'].items()
                           if (release.get('asset_pipelines') or {}).get(group) != value)
        print(f'  PC build {release.get("build", "?")} ({release.get("tag", "?")}), '
              f'revision {str(release.get("revision", "?"))[:7]}')
        if different:
            print(f'  warning: this PC build prepares {", ".join(different)} differently from PC '
                  f'{pc_build}, which this Switch build matches. If the Switch build fails to '
                  f'start or looks wrong, set up with PC {pc_build}.')
    if problems:
        sys.exit('error: the PC setup isn\'t finished:\n  - ' + '\n  - '.join(problems) +
                 '\nStart the PC game and let it reach its menu, then run this again.')
    if not (root / 'assets/private/audio/audio_manifest.json').is_file():
        print('  warning: no sound files (assets/private/audio/): the game will be silent')
    return root


def plan(root, nro):
    """(source, path under the SD card's game folder) for every file to copy."""
    files = []
    for name in ('assets', 'maps', 'maps.json', 'settings'):
        top = root / name
        if top.is_file():
            files.append((top, Path(name)))
            continue
        if not top.is_dir():
            sys.exit(f'error: {top} is missing; start the PC game so it sets up again')
        for directory, _, names in os.walk(top):
            for file in names:
                source = Path(directory, file)
                relative = source.relative_to(root)
                if relative not in LEFT_OUT:
                    files.append((source, relative))
    if nro is not None:
        files.append((nro, Path(NRO)))
    return files


def unchanged(source, destination):
    """Same size, and a modification time within FAT's two seconds."""
    try:
        a, b = source.stat(), destination.stat()
    except OSError:
        return False
    return a.st_size == b.st_size and abs(a.st_mtime - b.st_mtime) <= 2


def copy(source, destination):
    """The bytes and the modification time; FAT and exFAT keep no permissions to copy."""
    shutil.copyfile(source, destination)
    stat = source.stat()
    os.utime(destination, ns=(stat.st_atime_ns, stat.st_mtime_ns))


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    parser.add_argument('pc_game', type=Path, help='the PC game folder, with skate3rust.exe and data/')
    parser.add_argument('sd_card', type=Path, help='the SD card root, or a folder standing for one')
    parser.add_argument('--check', action='store_true', help='check and count, copy nothing')
    args = parser.parse_args()
    root = check(args.pc_game)
    here = Path(__file__).resolve().parent
    nro = next((p for p in (here / TARGET / NRO, here / NRO) if p.is_file()), None)
    target = args.sd_card / TARGET
    files = plan(root, nro)
    copies = [(s, r) for s, r in files if not unchanged(s, target / r)]
    size = sum(s.stat().st_size for s, _ in copies)
    print(f'{len(files)} files, {len(copies)} to copy ({size / 2**30:.2f} GiB) to {target}')
    if nro is None:
        print(f'  note: no {NRO} beside this script; copy it to {target} yourself')
    if args.check:
        return
    if not copies:
        print('Nothing to copy: the SD card is up to date.')
        return
    if not args.sd_card.is_dir():
        sys.exit(f'error: {args.sd_card} is not a folder')
    target.mkdir(parents=True, exist_ok=True)
    free = shutil.disk_usage(target).free
    if free < size + (64 << 20):
        sys.exit(f'error: {free / 2**30:.2f} GiB free on {args.sd_card}; {size / 2**30:.2f} GiB needed')
    done = reported = 0
    for source, relative in copies:
        destination = target / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        copy(source, destination)
        done += source.stat().st_size
        if done - reported >= 256 << 20:
            reported = done
            print(f'  {done / 2**30:.2f} of {size / 2**30:.2f} GiB', flush=True)
    print('Done. On the Switch, open the homebrew menu with full memory and start '
          '"Skate 3 Rust Engine".')


if __name__ == '__main__':
    main()
