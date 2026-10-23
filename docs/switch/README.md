# Nintendo Switch

The Switch build runs this game on a Switch with custom firmware, through
bevy-horizon (Bevy on the Switch) and NVK (Mesa's open Vulkan driver). It uses the
files the PC version sets up from your own Skate 3 disc. **No Skate 3 assets are
included.**

## What you need

- A Switch running Atmosphère, with the Homebrew Menu (hbmenu) or sphaira.
- About 6 GB free on the microSD card: about 4.6 GB for the game, and up to 1 GB for
  the textures it keeps once each map has been played. FAT32 is fine: no file is
  near 4 GB.
- The PC version, set up from your own Skate 3 Xbox 360 disc. This Switch build
  matches PC **experimental build 49**; with another build, the export script warns
  you if the files differ.
- Python 3.8 or later for `switch_export.py`, or copy the files by hand.

## 1. Set up the PC version

Follow **Play** in the [README](../../README.md): run `skate3rust.exe`, select your
disc, and let setup finish. It is done when the game starts University. Then quit.

## 2. Copy the game to the SD card

Connect the SD card to the PC, then run the script from the release ZIP:

```
python switch_export.py "C:\Games\skate3rust" E:\
```

The first argument is the PC game folder (the one with `skate3rust.exe` and `data`),
and the second is the SD card. The script checks that setup finished, then copies
the game's files and `skate3rust.nro` to `switch/skate3rust/` on the card. Add
`--check` to see what it would copy. After updating the PC version, run it again;
it copies only what changed.

**By hand:** open `data/installation.json` in the PC game folder; its `directory`
names the installation inside `data`, such as `installations/0123…`. From that
folder, copy `assets`, `maps`, `maps.json` and `settings` to `switch/skate3rust/`
on the card, leaving out `settings/graphics.json`. Then copy
`switch/skate3rust/skate3rust.nro` from the ZIP.

The card then has:

```
switch/skate3rust/skate3rust.nro
switch/skate3rust/assets/
switch/skate3rust/maps/
switch/skate3rust/maps.json
switch/skate3rust/settings/
```

## 3. Play

Start the Homebrew Menu with full memory: hold **R** while starting an installed
game. Started from the Album, homebrew gets too little memory. Then choose
**Skate 3 Rust Engine**.

The first launch takes longer while it compiles shaders. The compiled shaders are
kept in `.mesa` at the root of the SD card, so later launches start faster.
The first launch of each map also builds its textures for the Switch and keeps
them in `switch/skate3rust/cache/`, about 250 MB for University, so later launches
read them instead. Deleting `.mesa` or `cache` is safe; they're made again.

Controls are the PC version's controller layout. **(+)** opens the menu (graphics,
difficulty, maps, character); **(B)** goes back. **Quit game** returns to the
Homebrew Menu. Changed settings are saved in `switch/skate3rust/settings/`.

## Options

The Switch build turns on the rendering options that ran best on the console. To
change one, create `switch/skate3rust/options.txt` with a line per option, such as:

```
env SKATE_WORLD_LEAN=0
```

`env NAME=0` turns one of the defaults off; delete `options.txt` to go back to the
defaults. The defaults and the other options are listed at the top of
[`crates/skate-game/src/horizon.rs`](../../crates/skate-game/src/horizon.rs).

The pause menu's render scale draws the world at fewer pixels than the screen's. Below
100 %, the picture is stretched to the screen and sharpened with the sharpening filter
(RCAS) of AMD's FidelityFX Super Resolution 1.0 (MIT), the way the Switch ports
[masseffect-nx](https://github.com/nebadasSwifty/masseffect-nx) and
[usf4-nx](https://github.com/Thorhax/usf4-nx) render below the screen's resolution.
`env SKATE_UPSCALE=easu` uses FSR 1's edge-adaptive upscale (EASU) instead, which costs the
Switch more than the lower scale saves, and `env SKATE_UPSCALE=bilinear` stretches it
without sharpening.

## Logs

Each launch rewrites `switch/skate3rust/log.txt`, and a crash writes `panic.txt`
beside it. Include both when reporting a problem.

## Known issues

- University is the map played on the Switch; DownTown and Industrial haven't been
  tested there. The test world isn't available on the Switch: it stops the GPU.
- A custom skater (from the character customiser) runs slower than the default
  skater: each of its ten parts poses its own skeleton.
- Road lines in the distance can shimmer slightly.
- Docked play hasn't been tested.
- Updates and Steam multiplayer are PC-only. Local multiplayer and mods haven't
  been tested on the Switch, and the export script doesn't copy mods.

## Building

The NRO is built with bevy-horizon's `scripts/build_game.sh` from this branch,
bevy-horizon and mesa-switch; the release's `SOURCE.txt` names the exact revisions.
