# 11 — Game audio (ambience, rolling, trick cues, footsteps, water)

**Branch:** `gameplay/audio` (from `gameplay/water`; partly committed 2026-10-02; the world-audio work since then is uncommitted until the user's batch commit). Upstream form: `audio/retail-audio`, draft PR #32, marked Work in Progress. **Status:** implemented and tuned
by ear over ~15 play sessions with the user (2026-10-01). Most cues are confirmed in play; the rest are marked
**not yet confirmed** below. All sample choices are project choices, not retail event data.

## Problem

The engine made no sound of its own. Only Lua mods could play audio
(`crates/skate-game/src/modding/audio.rs`, PCM WAV through Bevy's `bevy_audio`).
Skate 3's sounds are on the owned disc but nothing extracted or decoded them.

## What is on the disc

Everything audible is under `data/audio/`. All archives except `music/overlays.big` are EB BIG v3
archives readable with `tools/owned_game/big.py`.

| Archive | Contents |
|---|---|
| `ambience.big` + `ambienceresident.big` | 24 per-area ambience beds (`.sns` bodies, `.snr` headers), e.g. `04_dt_main`, `09_univ_campus`; 5-channel 48 kHz, ~2–2.5 min loops |
| `audiofiles.big` | 376 ABKC `.abk` banks, 20 SPLC `.bnk` banks, 23 per-map `.ems` emitter files, 9 MOIR `.csi` AEMS event scripts |
| `grains.big` | 14 rolling "grains", 7 surfaces × soft/hard wheels (`asphalt_smooth`, `asphalt_rough`, `concrete_smooth`, `concrete_rough`, `concrete_aggregate`, `wood_ramp`, `metal_smooth` hard only) + `x_jet_rolling` |
| `wheels.big` | `Whls_spins_Jump_1`, `Whls_spins_Man_1` (+ `.sek` seek tables) |
| `post.big`, `nis.big`, `english/…`, `music/…` | stingers, speech, cutscenes, licensed music — not used |

### Formats (worked out from the archives; `tools/asset_pipeline/audio_formats.py`)

- **Codec:** every sample is an EA SNR (EAAC) stream with codec 3 = **EA-XMA** (Xbox 360 XMA2),
  mostly mono 48 kHz (also 44.1, 36, 24 and 22.05 kHz).
- **SNR header:** `u32 version(4)|codec(4)|channels-1(6)|rate(18)`, `u32 type(2)|loop(1)|samples(29)`,
  optional `u32 loop start`. RAM streams are followed by blocks of `u8 flag (0x80 = last)`, `u24 size`,
  `u32 samples`.
- **SPLC `.bnk`:** stream count at 0x18, 36-byte parameter records, then that many SNR streams back to
  back at **unaligned** offsets. The scanner finds exactly the declared count in all 20 banks (2,266 streams).
- **ABKC `.abk`:** embedded SNR streams; the scanner's count matches vgmstream's subsong count for all
  376 banks (5,168 streams).
- **`.grain`:** `u32` offset of the SNR stream, `f32` duration (matches samples/rate exactly), a
  `.sek`-style seek table, then one SNR stream. Each recording is a **slow-to-fast rolling sweep**: over
  17–22 s it gets ~10 dB louder and brighter (e.g. concrete_smooth_hard −24 → −14 dBFS, centroid
  0.9 → 2.0 kHz), so looping the whole file sounds wrong.
- **5-channel ambience** order is L, C, R, Ls, Rs (from inter-channel correlation).
- **`.csi` (MOIR)** event scripts name events, classes and variables (`play_grind_start`, `Class_Flips`,
  `Ollie_Rattles`, `surface_type`, `playercharacter_footstep`, `body_imp_Torso`, …) in 16-byte records
  (name offset, 16-bit id, default value). The banks hold no per-sample names; linking events to samples
  would mean re-implementing EA's AEMS runtime, which this change does not do.
- Retail mixes at runtime: many raw samples peak at 0 dBFS with RMS up to −6 dBFS, so playback levels
  must be low; footstep recordings are the opposite (peak ≈ −11 dBFS, RMS ≈ −28 dBFS).

## Change — current design

### Setup: new `audio` asset group

- `tools/asset_pipeline/audio_formats.py` — SNR scanner/validator (block-chain check), SPLC and grain
  readers, standalone `.snr` cut-out, gain-free 5→2 downmix (weights per output sum to 1), loopable
  band cutting for grains.
- `tools/asset_pipeline/audio_export.py` — decodes only what the engine plays to PCM16 WAV in
  `assets/private/audio/` with `audio_manifest.json` (version 2): 22 ambience beds (stereo), 14 grains
  cut into **6 loopable speed bands** each (84 files, 0.08 s loop cross-fade), 2 wheel spins and 23 sample
  banks (2,718 samples). No normalisation or gain. ~40 s, ~615 MB.
- **Decoder: vgmstream r2117** (`vgmstream-win64.zip`, SHA-256 `6c4a8a38…dc6c`, matching GitHub's
  published digest), downloaded and verified by the existing `install.dependency()` into
  `data/tools/vgmstream-cli/`. It bundles FFmpeg's XMA2 decoder (LGPL DLLs). vgmstream reads SNR/SNS and
  ABKC itself but not SPLC or grain files, so every stream is cut out to a standalone `.snr` first and
  decoded in batches of 256 (one vgmstream run stops accepting input files between 600 and 1,100 arguments).
- Registered as group `audio` (`versions.py` `GROUPS`/`SOURCES`, `group_receipts.py` `ROOTS`,
  `asset_exports.audio`, `install.py`). **Optional content:** a failed download or decode records
  `assets/private/audio-availability.json` and setup still succeeds; the game then runs silent. Existing
  installs refresh only this group.
- `.gitignore`: retail audio extensions and `*.wav` (except the Skyline Drive mod's own WAVs).

### Engine: `crates/skate-game/src/game_audio/`

| File | Role |
|---|---|
| `mod.rs` | `AudioSettings` (master **75 % by default** — 100 % is unity gain; 25 % (−12 dB) was too quiet to judge retail parity in play, user 2026-10-02 —, ambience 100 %, effects 100 %, 5 % steps) saved to `settings/audio.json`; menu rows in GRAPHICS; `--mute`; `GlobalVolume` follows the master so mod audio obeys it; the single `SpatialListener` (moved out of `modding/audio.rs`) follows the gameplay camera; preloads every cue sample, grain band, wheel spin and water piece at startup (~207 clips, ~35 ms) so riding never reads the disk. |
| `library.rs` | Loads the manifest; reads WAVs into `Assets<AudioSource>`, measuring each clip's peak; rejects paths outside the audio folder; warns once per missing file. |
| `voices.rs` | Voice pool and **loudness rules** (below). Everything pauses while the menu is open or a replay runs. |
| `cues.rs` | The sample table, per-cue minimum gaps, metal-surface test, surface → grain table. |
| `skate_events.rs` | `observe` (FixedUpdate, after each physics tick) turns state changes into cues; `play` (Update) plays them and drives the loops (rolling, grind, powerslide, foot drag, wheel spin). |
| `ambience.rs` | One bed per map, level 0.6, 2 s cross-fade on map change. |
| `emitters.rs` | Retail world emitters from the map's `.ems` file (shape, falloff, 5-emitter pool) with measured bank programs (water banks). |

Outside the module (observation only; physics never reads any of it):

- `physics/animation_input.rs` — `AudioEvents` latch. `push_contact` (flags bit 27), `brake_contact`
  foot-down/up (bits 28 + 30 / 23) and `AudibleFootStepStrength` are cleared within the tick
  (`finish_output_publication`, `ScalarAttributeInputs::reset`), so they are latched while the animation
  attributes are processed and taken by `observe`.
- `physics.rs` `foot_clearance` — each animated foot's height above its ground line test (via
  `biped_ground::services::feet_input`).
- `skate-core` `WheelLineState.audio_surfaces` — audio surface (`surface_tag & 0x7F`) per wheel, next to the
  existing `physics_surfaces`. Additive output only.

### Loudness rules (`voices.rs`)

- A requested volume may raise a quiet clip only until the clip's own peak reaches full scale, and never
  more than ×4; category and master volumes (both ≤ 1) then scale it down. No clip plays hotter than
  full scale × master.
- One-shots start at their final level on the first sample; loops and sounds that ask for a fade start
  silent and fade in (≥ 10 ms). (Until 2026-10-01 every sound faded in and its gain was raised once per
  frame from 0, which ate the attack of 25–35 ms impacts and knocks — pops and landings lost their
  layering.) Retail layer offsets are kept with `Record::delay` (frame-granular). At most 32 voices, 3 of one sound, same sound again only
  after 40 ms; each cue also has a minimum gap (`cues::min_gap`: step 0.2 s, run step 0.15, push 0.3,
  bail 0.5, splash 1.0, others 0.12) as a safety net against flags that stay set for many ticks.
- One-shots get a random ±4 % pitch. Spatial attenuation scale 0.1 (Bevy's attenuation never amplifies).

### Cues (as in `cues.rs` / `skate_events.rs` now)

"Level" is the cue level in `cues.rs`; the scale column multiplies it. Fraction = board speed / 10 m/s.

| Cue | Trigger (per physics tick) | Samples | Level × scale | In play |
|---|---|---|---|---|
| Pop | `ground_animation.launched` rising | retail recipe: knock 1112 + 878 + 891 at once, crack 1096 + one of 1097-1099 at +20 ms, body 1111 +30 ms, low thud 876 +40 ms (traced offsets) | retail median × 2 | Rebuilt from 57 traced pops + captured sound; not yet confirmed |
| Flip / grab whoosh + catch | new trick name while airborne (a held grab re-announced by scoring stays silent) | `Sk8_Air_Flip_Tricks` 1–3 (retail's Class_Flips) + container 1115 (feet catching the board) | 0.45 / 0.8 | Retail mapping new |
| Land | filtered category Air → Ground, air not begun on foot; impact = `riding.ground.maximum_closing_speed` | container 1095 (wheels down, ~1 s) + impact family 1055 / 1058 / 1052 by impact (< 3, < 6, ≥ 6 m/s) | (0.6; 0.5 / 0.64 / 0.72) × (0.3 + 0.7·impact/12) | Retail mapping new; the impact-family choice is ours |
| Board down | Air → Ground when the air phase began on foot (on foot within the last 0.5 s) | container 1126 (foot on deck) + 1119 (knock) + 1054 (wheel touch) | (0.6 / 0.54 / 0.28) × (0.3–1.0 over 0–5 m/s) | Retail mapping new |
| Board down heavy | same, impact > 2.5 m/s ("caveman" jump-on) | + landing set 1095 | 0.6 × (impact − 2.5)/4 | Retail mapping new |
| Grind | `grinds.grinding_316` or a grind state, and not `leaving_317` | start: container 954 (ledge etc.) or 1146 (metal rail object); then metal: `GRINDS` 54–56 loop; other surfaces: container 955 pieces (202–206) re-fired every 63 ms | start 0.5 / 0.8, loop 0.26, pieces 0.3; × (0.5 + 0.5·fraction) | Retail mapping new |
| Powerslide | state 101 SlideGround: random piece every ~0.08 s (±20 %), 10 ms fade-in | `WHEEL_SKID_BANK` 0–7 | 0.2 × fraction | Not yet confirmed |
| Foot drag | between `brake_contact` foot-down ticks (ends at foot-up or 0.1 s after the last foot-down), riding on the ground above 0.5 m/s; piece every ~0.11 s, 50 ms fade-in | `FOOT_DRAG` 36–39, 44–47, 60–64 | 0.25 × (0.3 + 0.7·fraction) | **Confirmed** |
| Push | `push_contact` rising edge | `fstep_skateshoe1_sm` 74–84 | 0.6 | Plays at a sane rate (no flood); sound not judged separately |
| Step / run step | foot strike (below) while on foot (states 500/501) and moving > 0.3 m/s; run set above 3 m/s | `fstep_skateshoe1_sm` 74–84 / 6–17 | 0.85 / 0.68 × (`AudibleFootStepStrength`/4, 0.3–1.0) | **Confirmed** ("almost perfect") |
| Bail | entering WipeoutGround (300), not in water and no splash in the last 0.5 s; tier by body speed (≥ 3.5 / ≥ 7 m/s) | body hits container 1074 (records 667–671) + `Bodyslide` 0–4 / 5–9 / 10–15 layer | 0.52 × (0.6 / 0.8 / 1.0) + 0.25 | Bodyslide tiers heard as right; body hits = retail mapping, new |
| Splash | body water contact (`collision_feedback.flags.material_12`) rising while falling > 1 m/s (1.5 s cooldown); or entering a wipeout in water with no splash in the last 1 s (speed ≥ 2) | `Skate_Collisions` 476–478 | 0.5 × (0.4 + 0.6·speed/8) | **Confirmed** (found by the user); the wading-wipeout path not yet confirmed |
| Rolling (loop) | wheels in contact, not grinding, > 0.3 m/s; grain by majority wheel audio surface; speed band | `grains/<surface>_hard/<band>` | 0.15 × min(speed/1.5, 1) | **Confirmed** |
| Wheel spin | take-off above 1 m/s, cut on landing (0.05 s) | `Whls_spins_Jump_1` | 0.15 × fraction | Not yet confirmed |

**Rolling bands:** board speed is low-passed (0.25 s) and mapped over 0–10 m/s to the 6 bands; the band
changes only when the speed is a full band away and after ≥ 1 s on the current one; changes cross-fade
over 0.15 s, and within a band the pitch follows speed by ±6 %. A new surface must stay under the wheels
0.25 s before the grain changes; after 0.15 s off the ground the loop stops and restarts directly on the
surface it lands on.

**Footsteps:** `FootStrike` (in `skate_events.rs`) learns each foot's resting clearance (follows the
lowest value, drifts back up over ~2 s) and fires when a foot that lifted > 5 cm above rest comes back
within 2 cm. `AudibleFootStepStrength` only scales the volume.

**Surface → grain (`cues::grain_for`), CORRECTED 2026-10-02 to retail's own surface map.**

The old table was our own guess by surface name. The grain player research
(`audio-specs/grain-player-spec.md` §1.4) decoded retail's chain: a wheel's 7-bit audio surface tag →
material (tag − 1) → the vault's `Sk8::AudioSurfaceMap` → rolling surface 1–14 → grain. The function now
follows it exactly:

| Rolling surface | Grain | Tags |
|---|---|---|
| 1 | asphalt_rough | 2 |
| 2 | concrete_rough | 4, 66 |
| 3 | asphalt_smooth | 1, 55, 56, 61, 62, 71–83, 86–88, ≥ 95, 0 (no material): **the default** |
| 4 | concrete_smooth | 3, 51–54, 57, 60, 63–65, 92, 93 |
| 5 | wood_ramp | 6, 7, 41–50, 58, 59, 94 |
| 6 | concrete_aggregate | 5 |
| 9 | metal_smooth | 9, 11–36, 38–40, 84, 85, 89, 91 |

**What changed audibly:**
- **Asphalt is the default:** unlisted and high tags now play asphalt_smooth, not concrete_smooth.
- **Wood:** tags 47–50, 58, 59 and 94 now play wood.
- **Asphalt:** 55 and 56 now play asphalt, not aggregate.
- **Concrete:** 51–54, 57, 60, 63–65, 92 and 93 stay concrete_smooth (now explicit).
- **Metal:** 84 is now metal.

**Still interim, marked in the code:**
- **No grain in retail.** Rolling surfaces 7 (tags 10, 70), 8 (8), 10 (67), 12 (68, 69) and 13 (37)
  play a per-surface `Class_rolling` patch (selectors 1, 2, 10, 11, 9), which needs the AEMS evaluator.
  Until then they keep stand-in grains: aggregate for 8/10/70, metal for 37/67–69.
- **Uncertain:** tag 90 (rolling surface 0, no grain member) keeps wood_ramp.
- **Soft wheels:** only the hard members play. Retail picks the soft member when an audio-state flag is
  set (Motion+200 < 0.5, meaning not decoded yet). Surface 9 has only a hard member.
- **The speed-band loops are still interim:** retail's grain player reads windows from the sweep (two
  players per truck, a Bézier speed → position curve, equal-power crossfades, a grain every 0.3 s). See
  the native-runtime research.

### Measured retail mapping (2026-10-01)
The pop/flip/landing/board-down/grind/bail cues above use the retail SPLC patches that a **local
recompiled build** of the game (github.com/mchughalex/skate3recomp, built and run locally only, no code
taken) was measured playing for the same moments. The recomp was instrumented to log every sample start
with its final gain (gain modules × send), every SPLC patch request with the gameplay function that asked,
and every audio-object post; the user played normally while it logged, and the events were grouped by
the game's own requesting function. Exact moments came from a scripted ollie/kickflip:

| time after the flick | retail plays |
|---|---|
| +0.24 s (pop) | containers 1096 + 1098/1099, flip object (Sk8_Air_Flip_Tricks 1–3) |
| +0.3–0.6 s (catch) | deck knocks 1112 / 1115 / 1118 (548–552 + 530–535) |
| +0.95 s (landing) | 1095 (396–404, ~1 s) + one impact family of 1052/1055/1058 + wheel skid |
| getting on the board | 1126 (422–426) + 1054 + 1095 + knock 1119 |

Measured levels (linear, retail mix): knocks 0.36–0.57, pop/landing sets 0.2–0.46, board-down 1126 up to
0.76, ledge grind pieces ≤ 0.35, rail start 0.49, bail hits ≤ 0.26, Bodyslide ≤ 0.19, GRINDS ≤ 0.13,
rolling grains p90 0.065. Our levels = retail × 2.0 (`cues::RETAIL_SCALE`), calibrated so retail's
rolling level lands on our play-tested `ROLL_LEVEL`. Not decoded yet: how retail picks among the impact
families 1050–1059 (surface/force); we choose by impact.

### World emitters from the retail `.ems` files (`emitters.rs`, 2026-10-02)

Replaces our own water placement (`water.rs`), which grouped water collision into bodies and started a
random 1–2.5 s piece every 0.7–1.3 s. The pieces piled up 2–3 deep; the user heard the University plaza
water "overlapping or playing too many times".

**Data (setup, `audio_formats.py` / `audio_export.py`, manifest version 4):**
- `.ems` layout: BE u32 count + 72-byte records: index, flags, position[3], extent[3], scalars[4],
  u64 sound id, gains[4].
- The sound id is Bob Jenkins' lookup8 64-bit hash (seed `0xABCDEF0011223344`) of a name, written as named
  or lower-cased.
- Each id keys a record of the emitter attribute class `0xF0CEF367088EFFF8` in the disc's
  `skatercollections` database, resolved through its parents:
  - volume, bank file, patch (the selector the bank's program plays);
  - `eVolumeType` (1 = looping emitter, 5 = reverb zone);
  - `eVolumeFalloffType`;
  - a float that defaults to 10 (probably a one-shot duration; unverified).
- The exporter writes all 23 `.ems` files with resolved attributes. It also decodes every bank the
  `sfx_*` / `skateschool` emitters use: 100 banks, 95 of them new. All 486 sound emitters resolve to a bank
  on the disc.

**Game side (from TU3, reference only):**
- **Shape:** a record is a sphere (equal extents) or an ellipsoid. Its semi-axes are the extents along
  forward = scalars[1..4], up and side (side = Y × forward).
- **Core:** scalars[0] is an inner core with full level. Outside it the distance is rescaled to the edge.
- **Level** = attribute volume × falloff: (1−d)², 1−d or flat.
- **Pool:** at most 5 emitters play (`CSTATEMGR_Emitter`'s pool), taken in the order they were reached. An
  emitter the listener leaves is released at once (we use a 50 ms fade only against clicks).
- **Flags:** records with non-zero flags are gated by a context mask we haven't identified, so they are
  skipped.
- **Gains:** the four gains are never read by the game.

**Bank programs (interim):**
- What a bank plays is its own AEMS patch program. Run through the PoC evaluator:
  - `water_fountain`, `water_lapping`, `water_lapping_pond` and `ocean_wave_small` are two-voice relays of
    one-shots from per-voice shuffle bags, the next piece starting about 0.17 s before the current one ends;
  - `water_dam_*` and `fountains_waterlaps_*` are single loops;
  - each has slow level and pitch modulation (e.g. fountain pitch 0.82–0.94).
- `emitters.rs` `PROFILES` reproduces those measured patterns for the water banks until the AEMS evaluator
  is ported. Banks without a profile (trees, birds, HVAC, interiors, …) are not played yet.

University: the plaza channel is `fountains_waterlaps_left` (a 37 s loop; patch 347) in a 6 × 4 × 47 m
ellipsoid, next to `water_fountain` in a 13 × 7 × 61 m one.

### Random distant one-shots: sirens, horns, dogs, jets (`random_sets.rs`, 2026-10-02)

Retail has occasional police sirens and other distant city sounds. They come from a location-based
random one-shot scheduler, not from placed emitters.

**Location → set:**
- The game queries the district's world-painter region layer `audio_emitters` at the skater's x, z.
- The layer is 128 m tiles of quadtrees in the `cSim_*.xsf` streams of `worldDIST_<district>.big`.
- Each leaf holds the key of an `aud_wp_emitters` record (57 sets). Examples:
  - `e_dwtn_office_buildings`: a sound every 2–5 s at level 0.2–1.0, 23 weighted sounds including city
    sirens;
  - `e_dwtn_spillway_brewery` (the Aletown spawn): every 4–8 s, 15 sounds including 5 sirens.
- Setup exports the sets (`audio_export.random_sets`) and the region tiles (`audio_export.regions`,
  parser `audio_formats.region_layers`), and decodes their banks. The manifest now has 222 banks.

**Scheduler (from TU3):**
- At most 2 banks are loaded, each picked by weight.
- A timer fires the loaded sound that was loaded longest ago, every uniform min..max seconds.
- Level = sound volume × a 0.1-step random factor in the set's range.
- A post comes from a random fixed direction (not positional) and ends with its program or its timeout:
  sirens 19.5 s, dogs 5 s.
- Each bank's program (layers, delays, pan sweep) is measured per bank in `random_programs.rs` until the
  AEMS evaluator is ported.
- Four banks never answer their attribute's selector (`Siren_city_5/6`, `car_horn_city_2/3`), so they
  stay silent here as in retail.

**Verified:**
- A recomp trace at the Aletown spawn shows set `e_dwtn_spillway_brewery` firing a sound every 4.5–7.8 s,
  including `Siren_distant_2` and `Siren_city_8`.
- Our region lookup returns the same set there, and `e_univ_megapark` at the University start, as the
  recomp does.
- A 15-location recomp run (5 teleports each in Downtown, Industrial and University, 45 s at each) gives
  our lookup the same set as retail on **1,859 of 1,859** position samples, across 15 different sets.
  Retail's fired sounds fit each place: sirens, horns and a car crash downtown; seagulls at the docks;
  wolves and coyotes at the Spillway; hawks and rockfalls at the mega-park.
- Retail fired `Siren_city_5` at Hotel District and no siren sample played, so the banks we keep silent
  are silent in retail too.
- **Long runs (5 minutes per location, 2026-10-02)** — *re-verify on clean data: the long_* sessions were deleted
  (malformed, interleaved trace lines; user, 2026-10-02):*
  - **All 36 Challenge Map locations** (Downtown 14, Industrial 12, University 10). Our location sets match
    the game on **26,817 of 26,817** samples, across 28 sets.
  - The game's ambience zone (trace hook AMBST) equals our `audio_ambience` lookup at every location in
    all three districts.
  - Every drawn interval lies within its set's range.
  - About 60% of intervals fire a sound, matching the 2-loaded-banks rule.
  - Rare sounds need long listening: `work_whistles` never fired in about 20 minutes of the factory set,
    nor hawks and rattle_snake at the mega-park or cicadas at the Observatory in 5 minutes each.
  - Tools: `check_sets_vs_trace.py`, `long_run_stats.py` (local tools).
- Muted in-game run: our engine at Aletown selects the same set and fires sirens, horns, buses and bangs.
- The small parks have no `audio_emitters` layer, so no random one-shots play there.
- `SKATE_AUDIO_SET=<name>` forces a set for testing.

### Zone ambience beds (`ambience.rs`, 2026-10-02): replaces the per-map bed choice

**Data:**
- The zone is the district's region layer `audio_ambience` at the skater's x, z: an `aud_wp_ambiences`
  record (23 zones) with volume, bed number, fade-out time (+8) and fade-in time (+12).
- Bed number `NN` is the `ambience.big` stream whose name starts with `NN_`. Index 19 exists twice; retail
  takes the first in TOC order, `19_reclaimed_b_fix`, and so do we now.
- `aud_wp_ambience_crossfades` (23 pairs, e.g. `dt_03_main2dt_open`) map a zone pair to a group of the
  district's `Main_Ambience_Crossfade_DT/Ind/Uni` bank, with a level.
- Setup exports zones and crossfades (`audio_export.ambience_zones`) and decodes the three crossfade banks.

**Behaviour (TU3 SFXObj_Ambience, reference only):**
- **One bed at a time.** On a zone change the old bed fades out linearly over the old zone's fade-out
  time and stops, then the new bed fades in over the new zone's fade-in time. A return during the
  fade-out fades the same bed back in; a change during a fade-in waits until it ends.
- **Crossfade layer.** For the whole transition the crossfade group of the pair (either order; group 1,
  level 1.0 by default) plays: four looping 2 s voices at quad directions, rear voices at ≈0.70 (measured
  per group, `crossfade_groups.rs`). It stops when the fade-in ends.
- **Districts.** The parks have no crossfade bank, so no crossfade layer there.
- **No panning.** Beds aren't positional.
- **Old installs.** Without zone data (manifest v3) the old per-map table below is the fallback.

**Verified (muted run, DownTown):**
- The spawn is `dt_less_busy`. Moving into `dt_main` played crossfade group 2 at level 1.75 (the data's
  `dt_02_less_busy2dt`), then the `dt_main` bed.
- The teleport to Aletown played group 3 at 1.25 (`dt_03_main2dt_open`), and `06_dt_open` started after
  `dt_main`'s 1.75 s fade-out.
- The teleport to Hotel District used the same pair in reverse.

**Bed level (verified 2026-10-02):** the Ambience MixMap's bed output carries a **−11 dB base**. Retail
captures at four zones were checked with a local tool (`check_bed_level.py`): the bed's
5-channel stream × zone volume, folded like the recomp's capture.

| Zone (volume) | Retail median | Predicted with −11 dB | Without |
|---|---|---|---|
| dt_open (0.75) | −42.1 dBFS | −42.1 | −31.1 |
| dt_main (0.6) | −39.2 | −38.7 | −27.7 |
| dt_rez (0.75) | −44.2 | −43.6 | −32.6 |
| indu_quarry (0.8) | −47.7 | −47.8 | −36.8 |

The bed gain is now zone volume × 10^(−11/20) × `cues::RETAIL_SCALE`. Our levels run at 2 × retail, so
this keeps retail's bed-to-player balance. Before this the beds were 11 dB too loud relative to retail.

**Not decoded yet:** the other MixMap outputs feeding the bed (pitch, low-pass), the crossfade level
(taken as full scale), and ducking. Trace hooks for them are listed in the notes.

### Ambience per map: fallback only (installs without zone data; project choice by bed name)

| Map | Bed |
|---|---|
| University | `09_univ_campus` |
| StartPark | `10_univ_housing` |
| DownTown | `04_dt_main` |
| DownTownSkatePark, MegaPark | `05_dt_parks` |
| Industrial | `11_indu_shipyard` |
| IndustrialSkatePark | `20_indu_old_factory` |
| SkateSchool | `18_skate_school` |
| MaloofMoneyCup | `21_interior_arena_amb` |
| BlackBoxPark | `22_interior_tunnel_amb` |
| Test world | none |

"Ambience good so far" (user, University); other maps not yet judged.

### Logging (always on)

- `AUDIO_CUE <cue> <bank>:<index> volume=…` for every one-shot (`(voice limit)` if refused).
- `AUDIO_LOOP start <file>` for rolling/grind/water loops, `AUDIO_LOOP start <name> (<bank>)` for shuffles.
- `AUDIO_EVENT brake down|up`, `push`, `grind start|stop surface= ledge= flag= leaving=`,
  `rolling surface= grain= band=` (on change only).
- Startup: `Game audio: 22 ambience beds, 14 rolling grains, 23 sample banks`, `Game audio: preloaded N clips`,
  `Ambience: <bed> for map …`, `Water audio: N bodies (…)`.
- `SKATE_AUDIO_TRACE=1` adds per-tick `AUDIO_TRACE` lines (footstep strength, push/brake flags, foot heights).

### Dev tools

- (Removed 2026-10-03: `tools/audio_audition.py`, the cue audition page; see "PR #32 review fixes, step 1".)
- `cargo run -p skate-data --release --example audio_surfaces -- <map.skate>` lists audio surface IDs per
  map; with `AT=x,y,z[,r]` it lists the collision surfaces (audio ID, physics type, slope) within r m
  (default 3) of a point, e.g. a HUD position the user reports.

### Retail patch records (SPLC) and prior work upstream
- **Prior work:** upstream PRs #4 ("Audio/retail exact player sound") and #1 (an earlier snapshot of it,
  plus a procedural vehicle-engine synth) run retail's AEMS sound routing through a function-by-function
  transcription of recompiled game code and a dumped TU3 memory image. That runtime conflicts with this
  fork's rule (re-implement; disassembly is reference only; no game code/data), so it is not used. Their
  format notes were used as *knowledge*, re-derived and verified here.
- **SPLC patch tree** (`audio_formats.splc_patches`): 60-byte header (+8 sample-table offset from byte 60,
  +12 records, +16 containers, +20 extras = 0, +24 samples); 36-byte records (+4 id, +7 group count); 72-byte
  containers (+4 record ids, +68 count, +69 mode); per record its groups (12-byte header: +8 member count,
  +9 mode) of 72-byte members (+0 sample index, +8 gain, +44 pitch spread, +48 gain range, +64 probability;
  +20/+52 delay, not used yet). Verified on all 20 SPLC banks: the walk ends exactly at the sample table,
  every member names a valid sample (5,973 members → 2,266 samples), and sample-table offsets sit at a
  constant distance from our SNR scan — **SPLC sample index n is our stream n**.
- **Semantics** (per PRs #1/#4, implemented independently in `skate_events.rs` `play_record`): a record's
  groups are layers played together, one member each; group mode 0 random, 1 in sequence, 2 shuffled
  without an immediate repeat; a member plays unless rand > probability; gain ± range; pitch random between
  1/s and s for spread s > 1; ids ≥ the record count are containers that pick one record.
- **Validation of the ear-picked samples:** every group the user picked is exactly one retail record in
  `Skate_Collisions`: pop 1074–1078 = record 818, landing 1088–1092 = records 824/825, heavy 1097–1100 =
  records 832/833, board-down 0/1/2 = records 125/126/127, splash 476–478 = part of record 869.
- **Corrected 2026-10-02 from the retail Splice code** (`sub_82975CC8` / `sub_82976860`): member `+4` is the gain,
  `+8` the pitch, `+44` the gain spread, `+48` the pitch randomisation, `+64` the probability (the field roles
  above and PR #4's were swapped); the native Splice player uses these (see "Board contacts, skid and squeaks").
- **Splash now plays record 869** (three layers: 169 at 75 % probability + one of 476–478 + one of 171/172).
- Setup exports each SPLC bank's patch tree in `audio_manifest.json` (`patches`, manifest v3).
- PR #4's own pop/landing ids are different, layered patches (e.g. container 1097 → records 745–750, each
  5 single-member layers from 260–267 / 635–657 / 1000–1015); they come from game tuning values we do not
  read. Not adopted; the user's picks stay.

## How it was tuned (condensed session history)

Each step came from the user's report of an in-play session plus the `AUDIO_*` log lines of that session.

1. **First pass:** push, foot drag, footsteps silent — the animation events are cleared inside the tick
   before any ECS system can see them → `AudioEvents` latch. Bails silent (`board_wiping_out` is reset by the
   ground phase) → WipeoutGround state. Grinds silent (raw `category_12`) → `player_state.current()`.
2. **Flood:** the next log showed **push fired 1,723 times** and steps ~60/s: push contact and
   `AudibleFootStepStrength` stay set for many ticks. That flood caused scratchiness, odd pick-up sounds and
   likely the hitches (now fixed). → rising edges, per-cue minimum gaps, preloading.
3. **Footsteps:** a `SKATE_AUDIO_TRACE=1` run showed `AudibleFootStepStrength` is a **held level**
   (2.5–4, higher when running), **not a step event**; a peak detector fired only when the level changed.
   → foot strikes from foot clearance (planted 0.02–0.04 m, swing up to 0.4–0.55 m; 56 steps over ~16 s at
   ~6 m/s). Footstep clips are ~10 dB quieter than impacts → the headroom rule replaced "volume ≤ 1".
4. **Foot drag:** `brake_contact` foot-down **repeats every tick** while braking (47 and 139 ticks seen),
   then foot-up repeats while lifting (24–28 ticks); bit 28 is set by both, so the first latch also dragged
   on foot-up. → drag only between foot-down ticks, above 0.5 m/s, overlapping 0.2 s scuffs.
5. **Rolling:** grain files are speed sweeps → 6 bands. Then 53 restarts in one session (band jumps right
   after landings, concrete seams flickering) → smoothing, holds. On University's plywood ramp the sound
   "played twice": `AT=` confirmed the ramp is audio 41 (Wood_1) throughout, so the mapping was right; 36 of
   38 quick restarts were band changes → switch only a full band away, ≥ 1 s per band. Remaining doubling at
   low speed: on returning to the ramp the loop **restarted on the debounced old surface** (concrete) and
   switched to wood 0.25 s later → stop after 0.15 s off the ground and restart on the surface underneath.
   User: "much better".
6. **Board down:** every touch-down after stepping on played `land`, because the physical state leaves
   BipedGround a few ticks before the filtered category reaches Air → "began on foot" = on foot within 0.5 s;
   level by impact (0.18–0.45 m/s seen for gentle set-downs).
7. **Deck family (found by the user):** the user picked out `Skate_Collisions` 1066–1092 as "noises heard a
   lot while skating". Measured: 0.13–0.37 s knocks, most energy at 400–2500 Hz (the deck), unlike the low
   thumps either side (1060–1065, 1093–1100; 70–90 % of energy under 400 Hz). Pop = sharpest
   (1074–1078, 5–20 ms attack); land = 1088–1092; weight = 1097–1100. User: pop and landing "10000% better".
   Earlier rejected landings: 35–40 (harsh), 18–20.
8. **Splash (found by the user):** a full decode of all 7,434 streams scored for splash shape found no
   long splash; candidates (`water_misc`, `Skate_Collisions` 741–762) were rejected ("not water related at
   all"). The user found **`Skate_Collisions` 476** ("literally a splashing noise") on a candidate page;
   477–478 are its variations (same length 1.6–2.0 s and spectrum). 473–475 (shorter) were tried for small
   falls and rejected → always 476–478. Each water entry also played a bail ~20 ms later → bail suppressed
   in water. Wading in then falling gave no fresh fast contact → a wipeout in water splashes by itself.
9. **Grinds:** sound followed the named state, which lags the selector → `grinding_316` / `leaving_317`.
   A University rail sounded like wood: `Skate_Metal` has no clip ≥ 1 s, so `GRINDS` (123 clips,
   1.4–2.6 s) holds every material. **By tonality** (energy share of the strongest 1 % of spectral bins)
   9 and 11 are among the least tonal (0.30–0.36), **54–56 the most tonal and brightest** (0.65–0.85,
   4–5.6 kHz) → 54–56 on metal.
10. **Bail:** `Bodyslide` 0–15 is three blocks of five (one longer slide + four hits), heard as rising
    impact → tiers. In play they sound like **additions to a fall, not the body impact** → kept as a quiet
    sweetener layer (0.25) until a main impact is chosen (EA's scripts
    name body impacts per body part: `body_imp_Torso`, `_leg`, `_Head`, `_foot`).
11. **Grab whoosh** repeated while holding a grab (3 in 0.8 s): scoring re-announces a held grab with the
    same name → whoosh only for a new name in the air phase. Confirmed.
12. A game window that closed with no report was Windows Terminal crashing (the game was attached to its
    console via `PLAY.bat`), not the game.

## Files

- New: `tools/asset_pipeline/audio_formats.py`, `audio_export.py`, `test_audio_formats.py`,
  `tools/audio_audition.py` (removed 2026-10-03), `crates/skate-game/src/game_audio/{mod,library,voices,ambience,cues,skate_events}.rs`,
  `crates/skate-data/examples/audio_surfaces.rs`.
- 2026-10-02 world audio:
  - `game_audio/emitters.rs` replaces `water.rs`;
  - new `random_sets.rs`, `random_programs.rs` (generated), `crossfade_groups.rs` (generated);
  - `ambience.rs` rewritten for the zone beds; `library.rs` reads manifest v4;
  - `audio_formats.py`: `.ems` reader, `name_id`, region layers; `audio_export.py`: emitters, location
    sets, zones and crossfades, regions, emitter and crossfade banks.
- Changed: `tools/asset_pipeline/{asset_exports,install,versions,group_receipts}.py`,
  `tools/asset_pipeline/test_versions.py` and `tools/test_setup_assets.py` (hand-written group lists),
  `crates/skate-core/src/physics/board_ground.rs` (+ test), `crates/skate-game/src/{main,app,config,graphics_menu,physics}.rs`,
  `physics/animation_input.rs` (audio event latch), `modding/audio.rs` (listener moved),
  `retail_character.rs` and `tests/map_transition.rs` (`Config.mute`), `.gitignore`.

## Verification

- `test_audio_formats` (synthetic streams only); setup tests 42/42; all tracked tools tests identical to the
  clean tree (the same 3 pre-existing import errors).
- Every SPLC bank: scanner count = declared count; every ABKC bank: scanner count = vgmstream subsongs.
- `cargo test`: `game_audio` (cue table sanity, bail tiers, band hysteresis, foot strikes, board-down levels,
  water bodies and fade, voice limits, headroom), `graphics_menu`, `modding::audio`, `map_transition`,
  `skate-core board_ground`.
- Export into the dev install: 22 beds, 14 grains (84 bands), 2 wheel spins, 23 banks; `--check-assets` →
  `SKATE_ASSETS_READY`.
- Muted smoke runs (`--mute`): the startup lines above, no panics, errors or missing-sink warnings.
- ~15 in-play sessions by the user (University, DownTown); confirmed items are marked in the cue table.
- **Not yet done:** full `regression-check` pass and a setup refresh through the installer path (only the
  `audio` group should rebuild).

### Native audio runtime: research (2026-10-02)
The measured per-bank tables (`emitters.rs` PROFILES, `random_programs.rs`, `crossfade_groups.rs`, the
`cues.rs` tables) are interim. The plan is to replace them with our own implementation of the retail
runtime:
- the AEMS patch-program evaluator;
- the voice graph: sample player, resampler, RBJ Q = 1 filters, gain with a 64-sample de-click,
  6-channel pan, sends;
- the MixMap mixer and the granular rolling bed.
Specs written so far: evaluator (all 40 opcodes, checked against the recompiled game across all 376
banks), voice graph, and a prior-art survey. The MixMap and grain player specs are in progress.

**Design goal: modular and moddable (2026-10-02).**
- Each subsystem sits behind a clear boundary: formats, evaluator, voice graph, mixer and buses, MixMap,
  grain player, world systems, output host.
- Every sound resolves by its retail identity (bank + patch, sample, set/zone key) through a content
  layer: mod override first, then retail. Mods can replace samples, banks and programs, location sets,
  emitters and ambience.
- The Lua SDK grows from today's play/update/stop of mod WAVs. Planned: posting retail sounds, setting
  globals, reading MixMap values, hooking game audio events (to replace, suppress or layer), and mod
  emitters on the same voice graph and buses. The existing commands keep working.

The evaluator research found that upstream #4's evaluator mishandles op 5 (CallFunction), so its
sample-selection functions never run, and lacks ops 19/20/38. Measurements taken through it are therefore
reliable for timing, layers and pans, but not for which sample a function-driven bank (sirens, dogs, birds)
picks.

### Native AEMS runtime: implementation (2026-10-02, `crates/skate-audio`)
**Problem.** Every world bank's behaviour was a measured table. Retail runs each bank's own patch
program through the AEMS evaluator and a fixed voice graph.

**Approach.** A new engine-independent crate, `crates/skate-audio` (`#![forbid(unsafe_code)]`, no Bevy, no
dependencies), written from our specs (`audio-specs/aems-evaluator-spec.md`,
`aems-voice-graph-spec.md`):
- `formats/`: ABKC module banks (modules, programs, templates, interface exports, the `S10A` sample bank),
  MOIR Csis projects, EA SNR sample headers. Bank load rejects opcodes ≥ 40 and programs whose block
  walk does not end at the template size.
- `eval/`: the evaluator. Instance memory is a byte copy of the module's template, read and written as
  big-endian words at retail's offsets, so programs run untranslated. All 40 opcodes, including 19/20
  (n-ary min/max) and 38 (ControlClass, a child post); the shared 6-word RNG from zero; the 6-block tick
  with tick scale f32 31.999998; Csis classes (constructor clients, newest bank first), functions
  (synchronous delivery, seen at the subscriber's next op 37) and global variables (notify on change);
  post / redeliver / release with refcounted nodes; Destroy releasing voices and child posts; the
  Player op (open applies the full input set, changed inputs pushed, query → time left / time current,
  restart only on a 0 → 1 edge, no restart of a voice that died while paused).
- `dsp/`: Resample (16.16 linear with retail's weight constant 0x377FFC9C, ≤ 4×), High/LowPassIir2
  (RBJ Q = 1, bypass outside 24 Hz … 0.999·Nyquist), Gain (64-sample de-click), Send (64/65 ramp, 16-tap
  release fold), Pan2D1 (6 outputs, pairwise constant power with centre extraction, distance spread,
  multichannel layouts, 64-sample matrix ramp), the route tables and the output stage's stereo fold
  (0.707·L + 0.5·C + 0.5·Ls, clamp ±1, no master gain).
- `mixer.rs`: voices built from those modules (first block silent = format change, 16-frame stop fade,
  loops, source-time remaining/elapsed for the Player op), implementing the evaluator's `VoiceHost`;
  routing codes (ids ≥ 9) are parsed and kept, every voice mixes into the default bus.
- `runtime.rs`: evaluator + mixer on one 256-frame block clock (render, then tick: a walk's voice
  commands apply from the next block, as retail drains its command ring per block).

**Game side.** `game_audio/native.rs` hosts it: off by default; `SKATE_AEMS=1` or `"native": true` in
`settings/audio.json` turns it on. All Csis projects install at startup, `emitter_utility` is posted as at
boot, banks load on first use (their WAVs as PCM) and unload on map change; one device-paced rodio stream
pulls 48 kHz stereo; volume = master × ambience, paused while silenced. So far only the `.ems` world
emitters use it (`emitters.rs`): with it on, **every** emitter record whose bank is in the install plays
its retail program (DownTown: 176 of 176 records, against 30 with a measured profile), posting
`c_emitter` with w0 32767, w1 = dry level (our falloff level; MixMap not ported), w3 = azimuth
(0 = ahead, clockwise, 65536 = 360°), w4 4096, w5 25000, w8 = the attribute patch; redelivered every
frame, released when the listener leaves. Location sets, zone beds, crossfades and skate cues keep their
tables.

**Setup.** `audio_export.aems_files` copies every `.csi` (archive order) and the exported banks' `.abk`
files plus `emitter_utility.abk` byte for byte into `private/audio/aems/` (52 MB), listed under the
manifest's `aems` key; manifest version 5 (`library.rs` reads 3..5). All 376 banks keep their rebase and
interface lists after the sample data, so the whole file is needed. S10A slot i = WAV i on all 376 banks.

**Evidence.**
- Format census against the spec on every disc bank (`tests/disc_banks.rs`): 376 banks / 385 modules
  parse; opcode counts equal the spec's census exactly; every op 0/1/2/37 block sits on the laid-out
  subscription state; op 5/27/38 block sizes match their layouts; 1,059 exports resolve except exactly
  the 4 the spec lists; 291 banks bind to `c_emitter`.
- **Evaluator vs the PoC oracle** (our probe `aems_golden` in the local PoC worktree, with its op 5
  fixed and n-ary min/max added locally, never committed; identical mock voice device on both sides):
  **234 of 235 scripts identical — 88,597 device calls (open / set / azimuth / release / end) and 599
  voice opens, 0 differences.** Scripts: one per emitter and location-set bank in the manifest
  (utility at boot, release at 20 s), both crossfade banks with group changes, sirens posted twice, and
  28 player/world classes (grinds, footsteps, cloth, seams, rolling, traffic, crowds incl. op 20, car
  alarms, moveables, flips…) with moving payloads. Every opcode used on the disc ran except 38. Before
  the PoC's op 5 was fixed, 30 scripts differed, all at the first sample choice of a function-driven
  bank: the native port delivers `*_msg` calls, as retail does (spec §11).
- The remaining script, Tazer (op 38), has no oracle: `tazer_control_class_owns_and_releases_its_child`
  checks the child post's lifecycle (refcount 3 while the child lives, 1 after it ends itself; parent
  release clears every instance, node and voice).
- **Voice graph vs the PoC's replay-verified kernels** (`tests/dsp_oracle.rs`, vectors from our local
  probe `dsp_vectors`): Resample **16,384 / 16,384 samples bit-exact** (8 steps from 1/65536 to the 4×
  clamp); LPF coefficients **40 / 40 words bit-exact** (image trig); biquad kernel **16,384 / 16,384
  bit-exact** over 8 cutoffs (25 Hz … 23975 Hz) after fitting retail's per-position association of the
  fused multiply-adds black-box against the oracle (`fit_biquad_association`, every position 100 %);
  gain ramp 1,013 / 1,024 bit-exact, the rest 1 ulp (lane arithmetic for irregular steps not recovered).
- Pan gains match all 13 worked cases of the spec (≤ 1.5e-4).
- Headless renders (`examples/aems_render.rs`, real banks and WAVs through the whole graph and the
  stereo fold): water_fountain relay −28 dBFS RMS, trees_rustle gusts −38…−44, Siren_city_4 two layers
  then silence after its sample; about 2,000× real time.

**Files.** New: `crates/skate-audio/**` (lib, `tests/disc_banks.rs`, `tests/dsp_oracle.rs`, examples
`aems_replay`, `aems_list`, `aems_render`), `crates/skate-game/src/game_audio/native.rs`. Changed:
`Cargo.toml` (workspace member), `Cargo.lock`, `crates/skate-game/Cargo.toml`, `game_audio/{mod,library,emitters}.rs`,
`tools/asset_pipeline/audio_export.py` (+ test). Plus local tools (not published).

**Verification run.** `cargo test -p skate-audio --release`: 45 unit + 3 disc + 3 oracle tests pass
(2 ignored fit tests). `cargo test -p skate-game --release --bin skate3rust`: `game_audio` 35/35, the
whole suite 354 passed with only the known `pipelines_accept_valid_group_outputs_when_fingerprint_changes`
failure. Pipeline tests (`test_audio_formats`, `test_versions`, `test_setup_assets`) 32/32. One muted
start with `SKATE_AEMS=1` on DownTown: runtime on, 176/176 records, `trees_rustle` started natively,
no panic; with it off, the old 30/176 path unchanged. **Needs a listening check in game by the user.**

**Not modelled yet / open** (marked UNCERTAIN in the specs, nothing invented): the environment send
(Send A) and effect returns (Send B, codes 4096/8192/16384), the material buses and 512/2048 buses (their
authored values are not recovered), so every voice mixes dry into the default bus; the sample-group
"level %" byte (not applied); pause/resume Send modes; loop time-left semantics (we report time left in
the current pass); SndPlayer pre-roll; the gain ramp's last ulp; who writes `c_emitter` w0 (we pass
32767) and the MixMap channels that should feed w1/w2/w3 (done since, next section). Next: retiring
`random_programs.rs`, `crossfade_groups.rs` and the cue tables one at a time.

### MixMap and the granular rolling bed: implementation (2026-10-02, `crates/skate-audio`)
**Problem.** The native runtime's `c_emitter` words were stubs (dry = our falloff level, no send, fixed
pitch/filter), and the rolling bed was our own approximation: each `.grain` recording cut into 6 looped
speed bands. Retail mixes every audio object through the MixMap (`MixMapSK8.mxb`: 247 controllers feeding
per-object volumes, pitches, filters and pans) and plays the bed with a granular player.

**Root cause / retail mechanism** (specs `audio-specs/mixmap-spec.md`, `grain-player-spec.md`): the
MixMap is a fixed graph of input products (A), distance/azimuth lookups with Doppler (B), AHR/ADSR ducks
(F), clamped sums (C) and output sums (E) evaluated once per 60 Hz frame; the bed is two GrainPlayers per
truck reading windows of one slow-to-fast recording around a speed-driven read position, picked without
repeats and cross-faded with square-root fades, rescheduled every 256-frame block.

**Change.**
- `crates/skate-audio/src/mixmap/`: `format.rs` (file decoding), `tables.rs` (POW/LOG/CURVE/SEMI/FINE by
  formula, the four FINE ulps and three image floats as stored: K = ±0x3FFF1FC4, 16.666, 16.66667), the
  evaluator `mod.rs` (instances per slot, cross-slot reference expansion, input runs by curve kind, the
  one-tick lags, envelopes, Doppler slew, the output conversions and their quirks: 0 mB → 32730, pitch
  below −4800 → 0, a disabled block writes −10000 read back as 22768) and `keys.rs` (controller keys).
  Owner readers `level` / `raw` / `pitch_4096` / `filter_hz`. Ported from our own Python reference
  evaluator (local tool `mxb_tool.py`, not published: it carries a name table taken from the game), no PR #4 code.
- `crates/skate-audio/src/grain/`: `format.rs` (`.grain` header, stored duration, the run-length varint
  seek table, EAAC header), `player.rs` (GrainPlayer: the pick with its 16-entry recent list and collapse
  rule, the per-block scheduler with the drift cut and timer × pitch in sustain, voices SndPlayer1 →
  Resample → square-root GainFader → Send into the player's mono bus), `board.rs` (per-surface tuning,
  the Bézier speed → position curve in retail's factored single-precision form, the A/B records, push
  envelope, slews, the rocket record), `bed.rs` (2 trucks × players A/B with their chains HighPass →
  LowPass → Gain → Pan2D1 → default bus; the rocket straight into the default bus; one title-wide
  generator seeded with the image constants). `runtime.rs` renders the bed on the block clock after the
  AEMS voices.
- Game (`SKATE_AEMS=1` only; the default path is unchanged): `native.rs` loads the MixMap and runs
  `mixmap_frame` before the cue systems (fixed 1/60 s steps): inputs Master.in1–4 / Music.in1, 2, 5 /
  Reverb.in5 = 32767, Pause.in0 while the menu is open, the local player's PlayerPhysics speed words
  (image scales, clamped), no-contact, wheel count, brake and id 9 = 0, its two 3DObjPos blocks (board seen
  from the camera, 0.25 m pull-back), Contacts.in1 on landings, the board owner's 0/4/6 pulses. The 5
  emitter states are the MixMap Emitter instances: `emitters.rs` writes each state's 3-D input and posts
  `c_emitter` with w1 = out4 × level, w2 = out8 × level, w3 = out0, w4 = out5 (pitch reader), w5 = out6.
  `grain_bed.rs` drives the bed when the install has the whole recordings: surface routing (one sounding
  truck, `cues::grain_for`), records from SkateBoard level(1)/(2)/pitch(3), chains from level(11)/(12)/
  raw(0) and the graph-1 level ramp, the rocket above 35 km/h; the interim speed-band loop then stays
  silent. Volumes: stream = master, AEMS voices × ambience, bed × effects.
- Setup (`audio_export.py`): `mixmap_file` copies `MixMapSK8.mxb` to `aems/` (`manifest.aems.mixmap`);
  `grain_whole` keeps each member's whole decoded recording (`grains/<stem>.wav`) and raw `.grain`;
  `grain_tuning` exports the vault's grain class, owner class and `Sk8::AudioSurfaceMap` exactly
  (`manifest.grain_player`). All keys are optional, so the manifest version stays 5. Dev installs:
  The local tool `stage_grain_mixmap.py` adds them without a full setup run.

**Evidence.**
- **MixMap vs the PoC's port** (PR #4's MixMap run locally as a black box, `mixmap_golden`) **and our
  Python reference**: 3 golden scripts, 1,500 evaluations, **87,000 / 87,000 output cells identical**
  (local tool `mixmap_compare.py`). golden1/2: ambience fade/crossfade, emitter distance/angle,
  player speed and Doppler, pause/menu/reverb/HOM ducks, dt changes; golden3 (new): 10 Collision
  instances, a Traffic drive-by with Doppler and sign-flip resets, the ADSR envelopes (F133/F137/F138 with
  retrigger), F24, Jitter-driven sums, a dt change.
- Disc census: 14 slots; 547 input entries, 853 A, 635 B, 350 F, 240 C, 1044 E; 154 output blocks;
  **247 controllers** (= the retail capture). Generated tables equal the image's (all 512/602/512/12/100
  entries and the four float constants).
- **Grain picks: all 16 golden sequences** (A and B params × positions 0 / 0.25 / 0.63 / 1 × generator
  from zero and from the image seed, 24 picks each) **and the 3 candidate tilings identical** to the PoC's
  vectors; **positions bit-exact on all 31 POS rows** (concrete_rough_hard and default, 0–80 km/h) after
  using retail's factored Bézier rounding.
- The 14 disc members parse (`tests/disc_grains.rs`): single-entry seek tables, pre-roll 384, stored
  durations; `cues::grain_for` agrees with the install's own AudioSurfaceMap for every grain surface
  (tags 0–127).
- Board outputs with our inputs (`examples/board_mix_probe.rs`): grain pitch 0.790 at rest, 0.864 at
  1 m/s, 0.996 from 5 km/h (retail medians 3209 / ~3500 / ~4080 of 4096); high-pass 77 Hz and low-pass
  24971 Hz (retail modal 77 / 24971 — 1109 Hz with id 9 = 32767, which is how id 9 = 0 for the local
  player was confirmed); gain A 0.23–0.26 from 5 km/h (retail p90 7859 = 0.24).
- Whole bed headless (`examples/grain_bed_render.rs`, concrete_rough_hard): 6.6 grain starts/s after the
  bind (retail modal 6/s for one truck), at most 4 voices, stereo RMS −40.7 dBFS at 5 km/h rising to
  −33.3 dBFS at 40 km/h (the recording's own rise plus the MixMap).
- Unchanged: the evaluator oracle still 234/235 scripts identical.

**Verification run.** `cargo test -p skate-audio --release`: 63 unit + 3 disc-bank + 1 disc-grain + 3 DSP
oracle tests pass (2 ignored fit tests). `cargo test -p skate-game --release --bin skate3rust`: `game_audio`
40/40; the whole suite 359 passed with only the known
`pipelines_accept_valid_group_outputs_when_fingerprint_changes` failure. Pipeline tests
(`test_audio_formats`, `test_versions`, `test_setup_assets`) 34/34. All headless; **the bed and the new
emitter words need a listening check in game by the user (`SKATE_AEMS=1`).**

**Files.** New: `crates/skate-audio/src/mixmap/{mod,format,tables,keys,tests}.rs`,
`crates/skate-audio/src/grain/{mod,format,player,board,bed}.rs`, `crates/skate-audio/tests/disc_grains.rs`,
examples `mixmap_golden`, `board_mix_probe`, `grain_bed_render`, `game_audio/grain_bed.rs`. Changed:
`skate-audio` `lib.rs`, `runtime.rs`; `game_audio/{mod,native,emitters,library,skate_events}.rs`;
`tools/asset_pipeline/audio_export.py` (+ tests). Local tools: `mixmap_compare.py`,
`stage_grain_mixmap.py`; golden3 kept locally.

**Not modelled / open** (nothing invented):
- MixMap inputs without a game source yet stay 0: Jitter (random every frame in retail), VU meter,
  Menu/NIS/HOM/Challenge flags, Contacts 2/6, Rail, OffBoard, HandGrabs, PlayerPhysics 3/5/6/11/12/13, the
  skater-frame azimuths (camera frame used for both), relative speeds (no Doppler), the second player.
  Retail's medians of gain A (0.06 at 5 km/h, 0.08–0.17 above) are lower than ours (0.23–0.26, near
  retail's p90); the missing inputs (Jitter scales A63, for one) are the first suspects.
- Bed: turn intensity (state +204 not mapped), slope, the seam-pattern envelope and the special latch are
  not wired, so player B (the turning / downhill layer) is silent; FrequencyShiftSsb (allpass
  coefficients not recovered), graph 3 (clip + shelf copy), the wobble gains and the environment send
  are not modelled; the fade curve is our equal-power form of retail's square-root code (exact
  rsqrte/Newton rounding not reproduced); the `Class_rolling` surfaces still use grain stand-ins; in the
  air the last member keeps playing (retail UNCERTAIN); soft-wheel members unused.
- Emitter positions reach the MixMap one frame late (inputs written in the cue systems, ticked at the
  next frame's start).
- `mixmap-spec.md` §6.2 said PlayerPhysics.in9 = 32767 for the local player; the PoC's input writer and
  the board's 77 Hz high-pass say 0. With 0 the ambience bed's speed duck (A[Player.5], up to −3 dB) is
  active — the ambience beds are not native yet, so nothing changes there today.

### Player inputs and the first native player components (2026-10-02, `skate_audio::player`)
**Problem.** The native runtime left most MixMap inputs at 0 (Jitter, Contacts 2/6, Rail, OffBoard,
PlayerPhysics 3/5/6/11/12/13, relative speeds), the rolling bed had no turn, slope, seam or special
input (player B silent), and every player sound still came from the measured cue tables.

**Retail mechanism** (read in the TU3 recompilation; word tables cross-checked with upstream PR #4's
driver notes): per 60 Hz frame the audio-state bridge `sub_824B0DA8` fills one record per player; the
state controller writes PlayerPhysics (`sub_824B19C8`); every component *processes* (owner inputs,
posts, releases), the MixMap evaluates once, then every component *updates* its held packets from the
owner's outputs and redelivers them.

**Change** (`crates/skate-audio/src/player/`, engine independent, unit-tested):
- `state.rs`: the audio-state record (`AudioState`, retail offsets in the docs). The game fills it in
  `skate_events::observe` (`audio_state`): ground speed, COM velocity/position (SystemReckoning),
  board position/velocity, per-wheel contact / material (tag − 1, 143 none) / **seam pattern** (new
  in `skate-core` `WheelLineState.seam_patterns`, tag bits 12..15, the part `board_ground.rs` used to
  drop), turn (`animation_input.fields.turn` = Processed+2676), slope `+712` (pumping absorption),
  KnownAir (state 200..300 with no wheel down) and our own air timer, the State52/54/59/60 flags
  (brake, manual brake, bail, balance), trick active / hippy jump (the score packet's scorable id with
  flag bits 24/25), feet in the deck box (Skeleton 600/601), soft wheels (wheel hardness
  Processed+2764 < 0.5), latched grind family / material.
- `inputs.rs`: PlayerPhysics 0–14 (3 = 0 and 13 = 0, see below; 11 = the bail-camera byte held 110
  frames after a bail — PR #4's capture shape, writer unidentified), Contacts (1 landing pulse, 2 the
  landing class from the per-wheel air buckets of `sub_824B2350`: ≥ 1 s air → 32767, ≥ 0.62 s →
  16000; 6 the landing-flag material), Rail 0/1, OffBoard 0.
- `jitter.rs`: `SFXObj_Jitter`, 24 bounded random walks from the vault (6 write inputs 0–5), own
  instance of the title generator.
- `objpos.rs`: the 3DObjPos writer as retail binds it — 60010010 follows the skater's COM (with its
  velocity), 60010020 the board; camera distance from the camera itself, frame-A azimuth from the
  0.25 m pulled-back origin, horizontal-plane angles, **signed relative speeds** (Doppler) and the
  sign-flip bits 31/30.
- `components.rs`: `Class_grind` (poster `sub_824C28B0`, constructor `sub_824AF8C8`, updater
  `sub_824C39E0`: layer by family, the family-0 layer-1 companion, V/F levels per grind surface, speed
  word capped at 9000, the updater's w10 = 1 quirk for family 0), `SenseOfSpeed_rattle` /
  `SenseOfSpeed_wind` (`sub_824E7980` / `sub_824E7CB0`: rattle ≥ 30 km/h ground speed, wind ≥ 15 km/h
  COM speed or 1 km/h while bailing), `Class_foot_drag` (`sub_824BB540` / `sub_824AF498` /
  `sub_824BEEE8`: brake or manual brake, Contacts outputs 4/5/16/17/18/22, foot-drag surface).
- `grain/board.rs`: turn intensity (`sub_824C8588`: min(clamp(|COM v|·0.24, 0, cap)·turn, cap),
  forced 0 by a latch, signed slew with the rise step when the target is above), the manual / trick
  latches, slope levels (owner inputs 2/3, A79 = +244 mB on gain A downhill) and the seam-pattern gain
  envelope (`sub_824CA448` / `sub_824CA318`: **one** linear ramp from 1 to a random gain per pattern
  change, then hold — the grain spec's "random walk" corrected; only spidercrack has a wobble ≠ {1, 1}).
- `mixer.rs`: per-bank volume groups (world / player, our user volumes) and a voice snapshot for
  diagnostics.

**Game side.** `game_audio/player_audio.rs` hosts it inside `native::mixmap_frame` (inputs → process →
tick → update). `grain_bed.rs` now feeds turn, special, downhill and the seam envelope into the records
and picks the **soft** members when the wheels are soft. With `SKATE_AEMS=1` the components run
(`SKATE_AEMS_PLAYER=0` keeps them off); their banks (GRINDS, sense_of_speed, FOOT_DRAG) load at start
in the player group (effects volume) and stay across map changes; the interim metal GRINDS loop, the
sense_of_speed bed cue and the foot-drag pieces are then silent. The default path (native off) is
unchanged. Setup: `audio_export.player_tuning` exports the AudioSurfaceMap rows, Jitter channels, seam
wobbles, grind levels (grind surface keys from the image table `0x82249F90`, as constants), wheel
landing buckets and — only with a TU3 image, so not in setup — the landing-flag materials; dev
installs: the local tool `stage_player_tuning.py`.

**Two values read as "0 for the local player".** PlayerPhysics.in13 is |state+96 − `[G+0x2F078]`+32|:
`G+0x2F078` is the audio-state record array (player 0 first), so for the local player both are its own
COM velocity (PR #4 read it as a listener distance). In3 is a facing factor of two PhysOut slot-0
vectors we have not identified; only A25 reads it, as a factor of the combo emphasis (Music.in3 = 0
outside combos).

**Evidence.**
- **Wind level vs retail**: SenseOfSpeed level(4) (= wind w7) at matched COM speeds against PR #4's
  retail medians: 25 km/h 339 / 343 (−0.10 dB), 30 km/h 693 / 730 (−0.45 dB), 35 km/h 1239 / 1194
  (+0.32 dB), 40 km/h 2074 / 2017 (+0.24 dB) — test `wind_level_matches_the_retail_medians`.
- **Components through the real banks, MixMap and voice graph** (test
  `components_play_their_retail_banks`, 3 s each; gain = master × dry): Class_grind metal 6 m/s
  median 0.018 / max 0.199, ledge (family 0, two layers) 0.026 / 0.237 — retail Class_grind 0.004–0.11
  / ≤ 0.28 (session 20261001_211347 — deleted, re-verify on clean data), GRINDS 0.004 / p90 0.027 / max 0.083 (all_20261002_163809);
  wind + rattle 55 km/h 0.033 / 0.132, 40 km/h 0.004 / 0.015 — retail sense_of_speed 0.003 / p90 0.093
  / max 0.387; foot drag 4 m/s 0.023 / 0.057 — retail FOOT_DRAG 0.006 / p90 0.075 / max 0.281.
- Unit tests: Jitter walk bounds and a hand-computed step; 3DObjPos signs and flip bits; landing
  buckets; grind posts (one layer, family 0 two layers, release, speed cap); rattle/wind thresholds and
  words; foot drag words; turn intensity cap/slew/latch; latches; slope; seam envelope draw and ramp.
- Python: `PlayerTuning` export test (leaf channels, parent chain, zero-block patterns, grind defaults).
- Unchanged: evaluator oracle, MixMap oracle, DSP oracle tests.

**The bed's gain A, investigated** (test `bed_gain_a_with_the_full_inputs`, straight rolling, camera
3.5 m behind and 1.4 m above):

| km/h | level(1) before (no Jitter) | level(1) with Jitter p10 / p50 / p90 | gain A, turn 0 | gain A / B, turn 0.5 |
|---|---|---|---|---|
| 5 | 0.228 | 0.156 / 0.187 / 0.217 | 0.187 | 0.156 / 0.094 |
| 15 | 0.239 | 0.192 / 0.212 / 0.230 | 0.212 | 0.127 / 0.261 |
| 30 | 0.250 | 0.231 / 0.243 / 0.249 | 0.243 | 0.146 / 0.301 |
| 45 | 0.253 | 0.224 / 0.242 / 0.251 | 0.242 | 0.146 / 0.283 |

- Jitter (A63, A69: small ducks scaled by Jitter.in0/in2/in3) lowers gain A by 1.7 dB at 5 km/h and
  0.4 dB from 30 km/h; the p90 under Jitter (0.217–0.251) sits on retail's level(1) p90 of 0.240.
  So the MixMap side matches retail.
- Retail's lower medians (0.06 at 5 km/h, 0.07–0.17 above, PR #4's table) are of the *record* gain:
  level(1) × (1 − max(I, Bk)) × special × seam, pooled over a session with turning, braking, manuals
  and mixed surfaces; a turn of 0.5 alone takes gain A to 0.13–0.15. The 163809 session's per-start
  grain levels (A and B pooled, idle and air time included) are lower still (asphalt_smooth_hard
  median 0.000 / p90 0.032; concrete_rough_hard 0.009 / 0.071), as expected when every start at rest
  or in the air counts. **Not settled:** a straight-roll retail measurement at turn 0 (GREC hook, grain
  spec §5.3) would close it; we invented no factor.

**Not modelled / open:** `Class_wheels_skid` (needs slip `+232`: the vault pair 45 / −0.75 of holder
`0xBA9837A6CF4C26ED` does not fit PR #4's formula as written — check `sub_82772E18`), Class_Seams,
squeaks, rolling rattle, Class_rolling, cloth/body slide, footsteps, Class_Flips / Treatment, and the
SPLC Contacts (pops, landings, touchdowns: a Splice player plus the unrecovered pop bank pick
`sub_824B9AD8`) — their interim cues play. Contacts.in6 needs the TU3 material key table (dev only).
Grind SPLC starts/pieces (collision manager `sub_824D2318`) stay interim. The bool-class word of
Class_grind (a global setting) is 0. Owner input 5 (heading rate), skid input 1, PlayerPhysics 4/5 and
Rail.in1 reach no MixMap output. Retail's B layer is silent ≥ 40 km/h in PR #4's capture; ours would
sound with a turn input there (level(2) 0.72 at 40 km/h) — check in game.

**Later stages (data from the main session):** traffic (horn `sub_824D6BE8`, skids `sub_824D7440`,
engine `aud_traffic_engine`; vehicle positions in VEHSTATE lines for Doppler validation; no special
skitch sounds) and pedestrian speech (not AEMS: streamed from `livingworldspeech.big` with
`*_Events.evt` / `hdr.big` / `sth.big` event tables; reaction → clip map in
`speech_161849.txt`; summary in [doc 15](15-world-audio.md) and
`audio-specs/npc-livingworld-re.md` §5b–§6e). AI skaters are full skaters with their own board
objects (voice budget question). Retail sessions with per-bank levels: `all_20261002_163809`, `all_20261002_164620` (clean trace; traffic horn / skid /
engine-class and ped-footstep levels for the world stages).

**In-game listening checklist** (`SKATE_AEMS=1`; `SKATE_AEMS_PLAYER=0` to hear the interim cues
instead):
1. Roll straight at 10–30 km/h on asphalt: the bed is 1–2 dB quieter at a crawl than before (Jitter)
   and breathes slightly.
2. Carve left/right at ~15 km/h: a second, slightly different-sounding layer (player B) comes in with
   the turn and the main layer dips; straight again → B fades out within ~10 frames.
3. Manual: the bed drops (special gain 0.65) and the turn layer stays off until four wheels are down.
4. Roll down a slope / pump a transition: when `+712` (our pumping absorption; what retail means by it
   is UNCERTAIN) goes negative the bed gets a little louder (A79, up to +244 mB) and B joins in —
   listen for a bed that swells on every pump (that would mean the mapping is wrong).
5. Ride over a spidercrack pavement: one dip of the bed (to 0.6–0.8) per pattern change.
6. Change the board's wheels to soft (hardness < 0.5): the soft grain members play.
7. Grind a metal rail and a concrete ledge: GRINDS loops (native) plus the interim start / scrape
   pieces; the old interim metal loop is gone.
8. Ride fast (> 30 km/h): rattle comes in; the wind from 15 km/h of body speed, louder with speed;
   bail at speed: wind keeps blowing while tumbling (1 → 10 km/h range).
9. Brake with the foot (and manual brake): native foot drag instead of the interim pieces (the interim
   one was confirmed by ear — compare).

**Files.** New: `crates/skate-audio/src/player/{mod,state,tuning,inputs,jitter,objpos,components}.rs`,
`crates/skate-game/src/game_audio/player_audio.rs`, the local tool `stage_player_tuning.py`.
Changed: `skate-core` `physics/board_ground.rs` (+ test), `skate-audio` `lib.rs`, `mixer.rs`,
`runtime.rs`, `mixmap/keys.rs`, `grain/board.rs`, `examples/grain_bed_render.rs`;
`game_audio/{mod,native,grain_bed,library,skate_events}.rs`; `tools/asset_pipeline/audio_export.py`
(+ test).

**Verification run.** `cargo test -p skate-audio --release`: 77 unit + 3 disc-bank + 1 disc-grain + 3 DSP
oracle tests pass (2 ignored fit tests). `cargo test -p skate-game --release --bin skate3rust --
game_audio::` 42/42. `skate-core` 621 pass; the 2 failures (`predictive_contacts_and_retention_match_full_scan_for_every_primitive`,
`a_moving_group_8_body_reaches_native_impact_feedback_for_a_stationary_actor`) fail identically with
our change reverted (pre-existing in release builds). The whole `skate-game` suite: 361 passed, only the
known `pipelines_accept_valid_group_outputs_when_fingerprint_changes` failure. Pipeline tests
(`test_audio_formats`, `test_versions`, `test_setup_assets`) 35/35. All headless;
**needs the listening check above.**

### Board contacts, skid and squeaks; the end-to-end PoC comparison (2026-10-02, `skate_audio::splice`, `player::contacts`)
**User listening check of the previous stage** (native build staged 16:59, `SKATE_AEMS=1`): "definitely closer,
but it still needs tuning"; rolling "wasn't quite right as far as sync and being too loud"; grinds "sounded ok but …
not at the right octave … just wasn't the same sound"; overall "pretty underwhelmed". Decision: "the PoC sounded
closer, use it as a reference" → the end-to-end PoC comparison harness below. A note on the reference: on one launch
of the PoC build the wheels-over-cracks (seam) sounds played louder/harsher, rattle-like; on the next launch at the
same spot they were the normal, quieter version; not reproduced since — a first-play effect of the PoC (it decodes XMA
into a PCM cache at run time). **Requirement recorded for our port:** the first trigger of every sample must sound like
the later ones (decoded before use, same gain/filter path): the native banks decode their WAVs when they load, and
`splice::tests::the_first_trigger_of_a_sample_renders_like_the_next` renders a sample twice, bit-identical.

**Problem.** Pops, landings and touchdowns, powerslides and squeaks still came from the measured cue tables; the
native build had no Splice (SPLC) player at all, so with the native components on the board's impacts were the
interim samples at interim levels, and the rolling bed dominated the mix.

**Retail mechanism** (read in the TU3 recompilation; addresses are facts, the code is ours):
- **Splice player** (`sub_82975700` start, `sub_829757D0` voices, `sub_82975A60` / `sub_82975CC8` draws,
  `sub_82975B08` per frame, `sub_82976860` voice frame, `sub_82976360` start, `sub_82976CF0` / `sub_82976FF0` fades,
  `sub_82976DD8` picks): an id below the bank's record count is a record, then containers (one record picked), past
  both the last record. One voice per group; a member by pick mode (0 random, 2 shuffled halves from the disc's state
  word, else sequential), skipped unless rand/32768 ≤ its probability (`+64`). Member fields (corrected): `+4` gain,
  `+8` pitch, `+16` pan offset (× block[4]; −127 = no panner), `+20/+52` delay, `+24` start, `+28` length, `+32/+36`
  fade-in end / fade-out start, `+40` curve (0: 1−cos, 1: (1−cos)², 2: linear, 3: sin, 4: sin² over π/2), `+44` gain
  spread (gain × [s, 1/s], linear in 2U−1), `+48` pitch randomisation. Record `+8` gain and `+12 + U·+16` pitch
  factor. A voice stops when its elapsed time (advancing by the previous frame's pitch × dt) passes
  `+8 / pitch × +28 + 0.16` or the sample ends. The CRT `rand()` (MS LCG) drives every draw.
- **Contacts** (`SFXObj_Contacts`; vault class `0xC26949FCB638A2CA` `default`): frame order `sub_824B8218` →
  manual landing `sub_824BB330` → touchdowns `sub_824B86E0` → process `sub_824B90D8` (pop `sub_824B9CC8` on entering
  the air with an audio trick other than −1/31/32/35/36; landing `sub_824BA630`) → update `sub_824BE1B8` after the
  tick. Pop: selector 2/1/0 by the jump velocity `+468` (|Air+112|/2.65 while Air440) over 0.42/0.25, ids
  1097–1099 (1103–1105 on a hollow surface), gain trunc(level(2) × [0.5, 0.75, 1.0]) (hollow [0.58, 0.76, 0.99]);
  roll 1111 above 4 m/s; ollie 1096 and landing impact 1095 for the local player (levels 12/13). Touchdowns: per wheel
  the conditioner's landed latch (`sub_82772FD8`) and the bridge bucket `+448`; kind 0 four wheels, 1 a pair from
  nothing, 2 the last pair, 3 one wheel, 4 manual landing; set = table[tier][3·kind + variant] (tier `sub_824BA310` =
  2·hollow + soft wheels; the second voice of a bucket-2 landing reads tier 0), gain trunc(level(3) × K[tier][…]).
  The block every sound gets: [level/32767, pitch(1)/4096, raw(0)·360/65535, dt, 1 (local 3-D), 1].
- **Class_wheels_skid** (`sub_824C7438` / `sub_824C72F0` / `sub_824AF678` / `sub_824C7A20`): held while the
  predicate holds — any slip (with a wheel down; slip `+232` = clamp((|deck velocity · deck Ri| + 0.75)/45, 0, 1),
  `sub_82772E18`, vault holder `0xBA9837A6CF4C26ED` 45 and −0.75: **settled**, so straight rolling keeps a skid at slip
  word 1), not grinding (unless family 4), no audio trick but a grab; without slip the revert flag `+690` / counter.
  w10 = clamp(counter + trunc(90 × slip), 0, 90).
- **Class_Squeaks** (`sub_824C7738` / `sub_824AFF48` / `sub_824C7DD0`): both feet in the deck box and > 1 wheel, posted
  once |deck tilt `+264`| × 114.59 ≥ 15, a tilt sign change reposts; w9 = deck spin |`+488`| / 1.5 × 1000 (< 50 → 0).

**Change.**
- `crates/skate-audio/src/splice/` (`format.rs` patch tree, `mod.rs` player, tests): the Splice player; its voices
  are mixer voices (`Mixer::open_direct` / `set_direct`: SndPlayer1 → Resample → Gain → Pan2D1 → default bus).
  `Runtime::splice`, `Runtime::splice_host()`.
- `player/contacts.rs`: the Contacts component (pop, roll, ollie, landing impact, touchdowns with the second voice,
  manual landing) behind the `SpliceHost` trait. `player/components.rs`: `Skid`, `Squeaks`.
- `player/state.rs`: `jump_velocity` (+468), `audio_trick` (+348, resolved by the host from the scorable through the
  vault's eSk8AudioTricks), `scorable`, `slip` (+232), `revert` (+690), `deck_tilt` (+264), `deck_spin` (+488);
  `state::slip`, `state::jump_velocity`. `tuning.rs`: `audio_tricks`, `name_hash` (lookup8).
- Game: `player_audio.rs` runs the contacts (when `Skate_Collisions`' patch tree is in the install) and the skid /
  squeaks; `native.rs` loads the Splice banks with their WAVs at start; `skate_events.rs` fills the new state fields
  (deck Ri / At from the part transform) and silences the interim pop layers, landing impacts and powerslide pieces
  when the native ones run; `library.rs` reads `aems.splice` and `player_tuning.audio_tricks`.
- Setup: `audio_export.splice_trees` writes each SPLC bank's patch tree to `aems/<stem>.splc` (`manifest.aems.splice`;
  optional key, version unchanged); `player_tuning.audio_tricks`. Dev install: the local tool `stage_splice.py`
  (manifest backup `audio_manifest.before-splice-stage.json`) and `stage_player_tuning.py`.
- **Audio state log** (`game_audio/state_log.rs`): `SKATE_AUDIO_STATE_LOG=<path>` writes one TSV row per frame in the
  e2e scenario columns plus elapsed ms and board / COM positions (buffered, flushed every 6 rows, no cost when unset,
  with or without `SKATE_AEMS`). `scenarios.py --from-log LOG --cut A-B NAME` turns a window of it into a scenario both
  stacks render.

**End-to-end comparison with the PoC** (`tools/audio-e2e/`: `scenarios.py` writes per-frame
situations; our probe `game_audio::e2e::e2e_render` and the PoC's local probe render them headless to raw 6-channel
f32; `compare.py` folds both the same way and compares per segment: RMS, octave bands, centroid, pitch, onsets).
Ours vs PoC, RMS dB of the common fold:

| situation | before this stage | after |
|---|---|---|
| straight roll 10 / 20 / 30 / 45 km/h | +0.4 / +0.1 / 0.0 / 0.0 (bands ≤ ±1 dB) | same |
| speed sweep 0 → 40 → 0 km/h | +2.3 … −0.7 (+1.3…+2.3 below 8 km/h) | same |
| ollie at 20 km/h: air / after the landing | −24.4 / −9.2 (no pop, no landing) | −1.0 / 0.0 |
| powerslide 20 → 8 km/h | −9.6 / −8.9 / −4.6 (no skid) | +0.4 / 0.0 / −1.0 |
| foot brake | −1.0 … −1.3 | −0.7 … −1.1 |
| carve ±0.8 turn at 20 km/h | +3.4 | +3.4 |
| manual at 15 km/h | −4.3 | −4.3 |
| metal rail / concrete ledge grind | −3.1 / +1.7, different spectrum | +0.7 / +2.0, different spectrum |

Causes of the remaining differences, each checked against retail rather than the PoC:
- **Grinds:** the PoC plays every grind as surface 2 (its "user-accepted interim" policy, GRINDS samples 94/95);
  retail's GRINDS voices in all_20261002_164620 / 20261001_211347 / long_dt_c (the last two deleted: re-verify on
  clean data) are samples 13/14, 20–23, 55/56
  (= our surfaces 0/1 and 4–9 through the program, the local tool `grind_census.py`), never 94/95, with pitch ratio
  p50 0.98–1.00 and LPF 24971 / HPF 0 like ours (`tools/retail_voices.py`). So **pitch and sample choice are retail;
  the PoC's grind is not**. Retail's grind sound is mostly the collision manager's Splice layers (ledge start 954 with
  464 events, 968, 958, 969, 876–878 in 164620, levels 0.1–0.4) with GRINDS underneath (p50 0.007, p90 0.15): those
  layers are the next port (below) and the likely reason ours "isn't the same sound". **Corrected in the next
  section:** 954 / 955 / 956 are body-impact (skin / denim / arm) sounds, mostly outside grinds; the collision
  manager's grind part is one start contact (board / truck against the grind material).
- **Carve:** our B layer follows the turn input (`sub_824C8588`, from the retail code); the PoC's B record stays 0 in
  this harness even with its SkateBoard level(2) at 0.68–0.71. Retail's capture medians of gain B (0.01–0.10) are
  pooled over mostly straight frames. Not changed; checking it needs the GREC hook below.
- **Manual:** ours applies the grain spec's special gain 0.65 to gain A in a manual (−3.7 dB); the PoC does not. Kept
  (retail code reading); a GREC session settles it.
- **Rolling level ("too loud"):** the native bed equals the PoC's to ±0.4 dB at every speed, so the bed itself is not
  louder than the PoC. What differs is the host: our stereo fold is the title's table (0.707·L + 0.5·C + 0.5·Ls, spec)
  while the PoC trims its downmix (0.6 × (L + 0.707·C + 0.5·Ls), not retail) — ours is 1.4 dB (front) to 4.4 dB
  (surround) hotter for the same mix — and until this stage the impacts that retail puts 17–23 dB over the bed were
  missing, so the bed was all there was. Against the retail capture (`tools/e2e/retail_windows.py`) the bed cannot be
  isolated: the capture's floor at rest is −32 dBFS (ambience, emitters) and only three clean straight-roll windows
  exist; the per-voice levels agree (retail grain voices p50 0.15 / p90 0.27, ours gain A 0.21–0.24).
- **"Sync":** nothing headless shows a lag (our and the PoC's envelopes follow the same speed sweep); the in-game
  path adds the device buffer and the bed is fed per rendered frame. The audio state log replays real play into both
  stacks to look at this with the user's own runs.

**Contacts against retail** (all_20261002_164620 report, per-sample audible level medians): roll 1111 retail
0.13–0.18, ours 0.16–0.24; pop selector 1 (1098) retail 0.07–0.40, ours 0.15–0.35; touchdown kind 0 (1052) retail
0.42–0.64, ours 0.22–0.48. Ollie 1096 and landing impact 1095 are silent outside combos in both: their outputs 12/13
carry the envelope F[Global.135] triggered by Music.in3 (the combo emphasis), which no one writes yet — retail's
medians (0.01–0.12) are consistent with that.

**Not modelled / open:** the owner buses of the Splice voices (eEQChain presets, Contacts levels 14/15 as send levels)
— they mix dry; the landing's collision-manager pair contact; foot taps / scuffs on the deck (`sub_824B95A0`,
board knocks 1112/1115 — frequent and loud in retail, next with the collision manager); the grind start contact
`sub_824BB0E0`; body / deck impacts; Music.in3 (combo emphasis); `+308`, `+720`, `+814` (not published); the slip's
B40+16 ring. Still interim: seams (`Class_Seams`, `sub_824C14C8` / `sub_824C1698` / `sub_824C1CA0` grid / `sub_824C1F18`),
rolling rattle, `Class_rolling`, footsteps, cloth / body slide, flips / treatment, the collision manager
(`sub_824D2318`, `sub_82486EF0` posts from landing, grind start, body and deck impacts).

**Verification run.** `cargo test -p skate-audio --release`: 90 unit + 3 disc-bank + 1 disc-grain + 3 DSP oracle
(2 ignored fit tests). `cargo test -p skate-game --release --bin skate3rust`: 362 passed, only the known
`pipelines_accept_valid_group_outputs_when_fingerprint_changes` failure (game_audio 43/43). `cargo test -p skate-game
--release --no-run` builds every target. Pipeline tests (`test_audio_formats`, `test_versions`, `test_setup_assets`)
37/37. All headless. **Needs the user's listening check** (checklist below).

**In-game listening checklist** (`SKATE_AEMS=1`; `SKATE_AEMS_PLAYER=0` for the interim cues):
1. Ollie on concrete at ~20 km/h: a crack (pop) at take-off with a short roll; landing on four wheels: one impact (plus
   a heavier second layer after ≥ 1 s of air). The interim pop/landing layers are gone.
2. Land on two wheels then the other two (nose/tail first): two separate touchdowns.
3. Manual, then put all four wheels down: one manual-landing touchdown.
4. Ollie on wood (a hollow surface): a different, hollower pop and touchdowns.
5. Powerslide: skid noise that follows the slide (louder the more sideways), with board squeaks when both feet are on
   the board and the deck tilts past 15°; straight rolling has no audible skid.
6. Compare the overall balance: impacts should now sit well above the rolling bed.
7. Optional: play with `SKATE_AUDIO_STATE_LOG=<file>` set so we can replay your exact runs headless.

**Ready to retire after that check:** `cues::POP`, `cues::POP_TAIL`, `cues::LAND_NORMAL` / `LAND_HOLLOW` /
`LAND_KIND_CHANCE` / `land_tier` / `land_scale` and `cues::LAND` (the native contacts replace them),
`cues::POWERSLIDE` / `POWERSLIDE_SHUFFLE`; `cues::LAND_CLOTH` and the board-down / bail / step / splash cues stay.

**Next stage:** the collision manager (Splice layers of grinds, landings, impacts; then grind start, foot taps), seams,
rolling rattle, `Class_rolling`, footsteps, cloth / body slide, flips / treatment; Music.in3.

**Files.** New: `crates/skate-audio/src/splice/{mod,format,tests}.rs`, `crates/skate-audio/src/player/contacts.rs`,
`crates/skate-game/src/game_audio/{e2e,state_log}.rs`, `tools/audio-e2e/{scenarios,compare}.py`,
`tools/recomp-trace/{retail_windows,retail_voices}.py`, `tools/audio-file-inspect/splc_fields.py`,
`tools/vault-inspect/vault_fields.py` (PR #37); local tools `grind_census.py`, `img.py`, `stage_splice.py`. Changed: `skate-audio` `lib.rs`, `mixer.rs`, `runtime.rs`, `player/{mod,state,tuning,inputs,
components}.rs`; `game_audio/{mod,library,native,player_audio,grain_bed,skate_events}.rs`;
`tools/asset_pipeline/audio_export.py` (+ tests). The PoC probe (`player_audio/e2e.rs` in the PoC worktree) is local.

### Collision manager, wheel spin, foot taps; the second listening test and the retail bed data (2026-10-02, `player::collision`, `player::wheels`)
**User's second listening test** (native build 18:02, `SKATE_AEMS=1`, relayed by the main session): (1) carving "too
easily triggered, just turning a little bit causes them to trigger"; (2) background ambience and noises like cars "a
little loud and don't sound like they're in the distance enough"; (3) "when jumping the rolling sound continues to
play"; (4) "manuals sound completely wrong"; (5) falling in water "almost right, maybe a tiny bit too loud"; (6)
grinding "still not in sync properly and doesn't play out the continuous grinding noise as in retail"; (7) "after
riding and then picking up the board, the riding noise continues too long"; (8) the put-down board sound "much better
but not quite right; it should sound like wheels hitting the ground and whatever effects the original engine uses
aren't being recreated".

**Validation data used.**
- Retail **GREC** session `all_20261002_180430` (11 min, the user playing the recomp; hook on `sub_824C6BD8` added by
  the main session from our spec, tool `tools/recomp-trace/grec_level.py`; credit: skate3recomp by
  @mchughalex, run locally): per-frame grain records A/B of both trucks, turn intensity, brake slew, state words.
- Our engine's **real-play log** `state_20261002_181631.tsv` (`SKATE_AUDIO_STATE_LOG`, the
  user's own 4 min of play), cut with `scenarios.py --from-log` into `log_jump`, `log_ollie`, `log_manual`,
  `log_grind`, `log_pickup`, `log_carve` and rendered headless.
- Clean retail level sessions only (`all_20261002_163809`, `all_20261002_164620`, `all_20261002_180430`); the
  sessions with malformed trace lines were deleted by the user (see "re-verify" notes in this document).

**The bed's low-speed level ("~5–6 dB too loud") — not confirmed.** The pooled straight-roll medians (`grec_level.py`:
0.104 at 5–10 km/h, 0.114 at 10–15, 0.188 at 15–20, 0.233 at 30–35) mix in frames the bed is meant to be quiet
in: the `+336` word (brake, push) and the `+340` word (balance, **grinding**, trick) are not filtered, a paused game
(material 4 at 25–30 km/h: speed fixed at 7.9362 m/s for ~10 s, gain 0), the seam envelope (`+1456` 0.74–0.98 on
material 1), a second board owner (`40C34020`, material 65) and Doppler moments (pitch A 1.18–1.24). On **clean**
frames (`tools/recomp-trace/grec_clean.py`: four wheels, |turn| ≤ 0.02, I/Bk ≈ 0, both words 0, 30 settled
rows, gain ÷ seam envelope) retail's level(1) is 0.219 (material 2, 5–10 km/h), 0.21 (material 40, 10–15), 0.229–0.244
(20–30), 0.245–0.25 (30–50) — our MixMap gives 0.231–0.256 at the test camera (3.5 m), i.e. within ~1 dB (Jitter −0.4
dB). On clean carves A/(1−I) = 0.20–0.27 (= level(1)) and B/I = 0.61–0.83 (ours level(2) 0.62–0.78): the bed formula
is retail's. Not changed. Open: the board's camera distance (B[Player.2] roll-off: 5 m −0.6 dB, 7 m −1.8, 10 m −3.9) is
not in GREC — adding the `0x60010020` block's input 1 to the hook would settle the residual. Analysis:
`grec_180430_clean.txt`.

**(1) Carving.** Our turn intensity follows `sub_824C8588` exactly: I vs |turn input| medians ours / retail 0.1 →
0.06 / 0.066, 0.3 → 0.18 / 0.185, 0.5 → 0.24 / 0.30, 1.0 → 0.50 / 0.60 (cap 0.8 vs retail p90 0.99 on some
surfaces). The difference is upstream: **our turn input (`Turn` animation attribute, ProcessedPhysIn+2676) is almost
binary** — in the user's log 29 % of riding frames sit at |turn| = 1.0 and 7 % in between, retail 16 % / 25 %; a stick
movement takes our value 0 → 0.5 → 0.75 → 1.0 in three frames. After the stick returns our I decays for ~13 frames
(retail's input decays through the middle values, so retail's I is 0 at turn 0: p90 0.0 vs ours 0.196). That is the
animation side (the `Turn` scalar our animation graph publishes), not audio; reported to the main session. No audio
change.

**(3) Rolling in the air / (4) manuals: `SFXObj_Wheels` was missing.** In the air our bed goes silent within ~5
frames (the MixMap's no-wheels duck F[Player.0] on level(1), confirmed headless on `log_jump`: gain A 0.25 → 0 in 3
frames). What kept playing was the interim `AIR_WHEELS` loop — `Whls_spins_Jump_1` from its beginning at a constant
level for the whole air time. Retail (`sub_824CD6F8` / `sub_824CDC70` / `sub_824CDD28`, our port `player/wheels.rs`):
a **spin-down recording started part-way in**, t0 = 14 s × (1 − clamp01(speed / 50 km/h)) (vault `0xC1831BDB6CB1B1EA`
`0x03B710C80E1AC13E`), slot 0 in the air, slot 1 **while balancing (manuals)**, the jump and manual recordings
alternating, gain Wheels level(1) (level(5) while balancing), pitch(2); stopped on landing / when the manual ends.
Retail levels (164620): `Whls_spins_Jump_1` 92 starts median 0.240, `Whls_spins_Man_1` 81 / 0.264. Ours on the user's
log: manual 0.20–0.21 (level(5)), air 0.21–0.24 after the pop. In manuals the bed itself matches retail (GREC manual rows gain A
0.141–0.157 at 10–40 km/h; ours 0.13–0.15); the missing spinning wheels were the wrong part. The +150 Hz frequency
shift of the special state is still not modelled.

**(6) Grinding: the collision manager** (`CSTATEMGR_Collision`, our port `player/collision.rs`; full mechanism in the
module docs): `sub_82486EF0` posts a 48-byte message (materials A/B, impact tiers, position, two contact levels) to an
LRU router over ten slots; each slot's `SFXObj_Collision` (`sub_824D1E00` / `sub_824D1F68` / `sub_824D2318`) starts one
Splice voice per material — the sample from the material's AudioSurface record (layout read from the disc schema,
`tools/vault_layout.py`) by its **kind** (the TU3 table at `0x8302D6E8`: 0 Skate_Collisions, 1 Skate_Metal, 2
HOM_Set_1), its tier and the other material's collision class — and updates it from the slot's Collision controller
outputs (category level 12..21, pitch 1/22, azimuth from the slot's 3-D block at the message position). Contact levels
`sub_82496C58` (the 13E20D39 window record), impact bands `sub_82497088` (the 7DAFF70B record). Ported posters:
- **grind start** `sub_824BB0E0`: truck 96 (board 95 for families 1, 2, 5) against the grind material, tier by the
  last grind impact `+228` over Class_grind's 0.25 / 0.5, 0.5 s cooldown;
- **landing pair** `sub_824BA630`: on a landing-flag material only (66 materials: the metals and a few others), board
  95 against the first landed wheel's material, tier by the latched air time / 0.4 over 0.1 / 0.3, levels × 0.65 / 1;
- **deck impact** `sub_824BD000`: board 95 (113 on foot or bailing) against the deck contact's material, each by its
  impact band on `+668` = max of the last 4 clamp01(|deck acceleration · deck normal| × 0.00125); a band-2 hit posts
  a second message at tier 1 × the window record's `+40`; 6-frame cooldown. Needs the deck contact's tag:
  `skate-core` `BoardGroundState.part_audio_surfaces` (CollisionInfo+4/+8/+12, + test).
**Correction of the previous section:** Skate_Collisions sounds 954 / 955 / 956 / 947 / 948 are the **skin / denim /
arm / leg** body-impact sounds (materials 107, 108/109, 100, 99), posted by the body-impact posters (`sub_824BC188`,
ragdoll `sub_824E3BE0`); only 29 of 464 events of 954 in 164620 fall within 1.5 s of a GRINDS voice. The interim
grind start (954) and the re-fired "scrape pieces" (955 every 63 ms) were body sounds, which is part of why grinds were
"not the same sound"; with the native contacts they are now silent. Retail's grind = Class_grind's GRINDS loop
(native since the previous stage) plus this start contact. Retail pitch of the start samples (163809 MOD lines):
streams 690–692 p10 0.44 / p50 0.61–0.95 — the material's pitch word 2096 (0.51) shows in retail too. The body
posters are **not ported**: they need the skeleton's per-region contacts (Collision+80..195, `sub_82BD60C8`), which
the engine does not compute. "Sync" of the continuous grind layer: GRINDS starts on the grind's first frame in the
log replay (`log_grind`).

**Foot taps and scuffs** (`sub_824B95A0` → `sub_824B9508` / `sub_824B9268`, `sub_824B9948` / `sub_824B97A8`, updates
`sub_824BEBD8` / `sub_824BF4A8`): a foot coming back into the deck box (Skeleton 600/601) after more than 25 ms out
taps the deck — first foot 1112–1114, second 1115–1117, both 1118–1120 by toe speed (≥ 2 / ≥ 5 m/s, Skeleton+196/+212),
hippy-jump set 1176–1178 — at Contacts level(8); a foot in the box moving faster than 0.35 m/s across the deck (max |x|,
|z| of its local velocity) scuffs it (`sk8_foley` 95 / 94) at level(9). Retail (164620): board knocks 1112 ×152 / 1115
×132 (sample medians 0.11–0.41), sk8_foley 94/95 ×434/×488 (0.06–0.25); ours on the log 0.2–0.3. The engine already
publishes the toe velocities (`FootPhysicalOutput.local_velocity`). The soft variants (`+484` from the unidentified
`+769` flag) are unused.

**(8) Board put-down.** Natively the wheels' touchdown sets (Contacts) and the feet's deck taps now play from the
retail mechanism; the interim knock (1119) and wheel touch (1054) doubled them and are silent with the native
contacts. The interim foot-on-deck set (1126) stays: retail's step-on / step-off (`sub_824B85B0` → `sub_824B8310` 1124
in the air / 1125 on the ground, `sub_824B8448` 1126) needs Skeleton+602/+603 (feet planted on the deck), which the
engine does not publish. The owner buses (eEQChain presets) are still not modelled — that is the "effects" part.

**(2) Distance / (5) water: the interim layer's ×2.** Every interim cue plays at measured retail level × `RETAIL_SCALE`
(2.0), a factor anchored to the interim rolling loop (by ear). The native player sounds play at retail level, so with
them on, the interim layer (zone beds, location-set one-shots incl. cars, crossfades, splash, steps, bails) was 6 dB
hot relative to retail's balance. With the native player components running the interim voices now play at the
measured level (`Voices.scale` = 1/RETAIL_SCALE, `native::follow_volume`); the default path is unchanged. Measured
remainder (open, item 7 of the plan): our native host folds 6 channels to stereo with the title's table, so a mono voice
at per-voice gain g reaches each ear at 0.5 g (front) … 0.35 g (rear) (`examples/pan_fold_probe.rs`), while an
interim Bevy voice reaches 0.75 g (spatial, near) or g (beds) — the interim layer is still ~3–6 dB above the native
one. The distance cue retail adds through the emitters' positional send (MixMap out8 into the send buses) is not
modelled (buses not ported).

**(7) Riding noise after picking up the board — not reproduced.** In the `log_pickup` replay every voice is silent
within 0.1 s of state 500 (no wheels → the no-contact duck) and wind within 0.75 s. Retail's wind (SenseOfSpeed) also
keeps running on COM speed. Needs the user's timestamp (or a fresh state log) of the moment.

**Music.in3 ("combo emphasis") — a game-mode flag, not a gap.** `SFXObj_Music`'s process `sub_824D1208` writes in3 =
32767 when `sub_824898C8` holds: a bit of `*(0x83083C38)+0x2F0D0` (bit 13), or the manager's `+932` with `+476`, or the
manager's mode `+1060` = 8. None is part of free skating, so outputs 12/13 (ollie 1096, landing impact 1095) stay
gated — retail 164620: 1095 69 events, sample medians 0.002–0.115. Left at 0.

**First-trigger rule.** The grain recordings are now decoded when the bed is created (`grain_bed::Bed::new`), not at
first use; the wheel-spin streams and the Skate_Metal / sk8_foley Splice banks are decoded at start like
Skate_Collisions.

**End-to-end vs the PoC** (standard scenarios, RMS of the common fold, ours / PoC): rolling ±0.4 dB (unchanged),
ollie air −0.9, after landing 0.0; ledge grind +2.0 (start contact), metal +0.7; manual −4.0 (special gain; ours now
adds the wheel spin, +28 dB in the 125 Hz band); carve +3.4 (unchanged). Log replays (ours
only): grind start voices Skate_Metal 194 + Skate_Collisions 619 on the first grind frame, GRINDS 0.66; knocks after
landings; wheel spin in the air and in the 6.5-s manual.

**Files.** New: `crates/skate-audio/src/player/{collision,wheels}.rs`, `crates/skate-audio/examples/pan_fold_probe.rs`,
`tools/vault-inspect/{vault_layout,find_field}.py` (PR #37), `tools/recomp-trace/grec_clean.py`,
`tools/audio-e2e/voices_summary.py`. Changed: `skate-audio` `player/{mod,state,tuning,contacts}.rs`, `runtime.rs` (stream bank);
`skate-core` `physics/board_ground.rs` (+ test); `skate-game` `game_audio/{player_audio,native,e2e,library,
skate_events,grain_bed,state_log,voices}.rs`; `tools/asset_pipeline/audio_export.py` (`collision_tuning`, the image's
material kind / key table as constants) + test; `tools/audio-e2e/scenarios.py` (real-play extra columns).
Dev install: `stage_player_tuning.py` re-run (`player_tuning.collision`, optional key; manifest backup
`audio_manifest.before-collision-stage.json`).

**Verification run.** `cargo test -p skate-audio --release`: 102 unit tests + the disc / oracle tests pass (collision 5,
wheels 3, contacts 8). `cargo test -p skate-game --release --bin skate3rust`: 362 passed, only the known
`pipelines_accept_valid_group_outputs_when_fingerprint_changes` failure (game_audio 43/43); `--no-run` builds every
target. `skate-core` lib: 621 pass, the 2 known pre-existing failures. Pipeline tests (`test_audio_formats`,
`test_versions`, `tools/test_setup_assets`) 38/38. All headless; **needs the user's listening check**.

**In-game listening checklist** (`SKATE_AEMS=1`):
1. Ollie at ~20 km/h: in the air a wheel-spin that is already slowing down (fainter and lower the slower you go), gone
   on landing — no rolling sound in the air.
2. Manual at ~15 km/h: the lifted wheels spin down audibly under a quieter bed; ends with the manual.
3. Grind a metal rail and a concrete ledge: a hit at the start (truck or board on the rail, metal ring on rails), then
   the continuous GRINDS layer; no repeated skin/denim "pieces" any more.
4. Land a jump on a metal surface (e.g. a rail top or metal ramp): an extra board-on-metal contact over the
   touchdowns.
5. Slam the deck onto a ledge / board slide hard: a deck hit (board 95 against the ledge).
6. Lift a foot and put it back on the deck (pushing, after an ollie): a board knock; dragging a foot on the deck: a shoe
   scuff.
7. Put the board down from walking: the wheels' touchdown and a knock when the feet land on it.
8. World balance: ambience beds, cars, sirens and the splash are 6 dB quieter than in the 18:02 build.
9. Carving: unchanged (the turn input is the cause; see (1)).

**Ready to retire after that check:** `cues::GRIND_START`, `GRIND_PIECES`, `GRIND_PIECE_INTERVAL`,
`GRIND_METAL_START` (collision grind start), `cues::AIR_WHEELS` (SFXObj_Wheels), `cues::BOARD_DOWN_KNOCK`,
`BOARD_DOWN_TOUCH` (taps / touchdowns); plus the previous stage's list.

**Not modelled / open:** body impacts (needs skeleton region contacts), step-on / step-off (Skeleton+602/+603), the
owner buses / eEQChain presets and the emitters' send buses, the HOM override (global `+564`), the stamp counter of the
LRU (we use the post order), Class_Seams (next), rolling rattle, Class_rolling, footsteps, cloth / body slide,
flips / treatment, the +150 Hz special shift, the turn input (animation), camera distance in GREC.

### Listening tests 3–5, the native-audio stutter, the riderless board (2026-10-02, `SKATE_AEMS=1`)

**Stutter (19:01 build).** User: tricks with `SKATE_AEMS=1` were "super stuttery … kinda impossible to do anything".
No build was running at the time.
- *Measured first.* A headless replay of the user's state log (`state_20261002_190234.tsv` through the e2e
  harness, `E2E_TIMING=1`) puts the native player audio on the game thread at p50 26 µs, p99 140 µs and max 1.5 ms per
  frame. Render is p50 16 µs per 256-frame block. The audio work itself was not the cause.
- *Root cause.* `native.rs` spawned the native output stream with `PlaybackSettings::LOOP`. Bevy turns LOOP into rodio
  0.20's `repeat_infinite()`, which wraps the source in `Buffered`. That has three effects:
  - It pulls **32,768 samples (64 blocks, 341 ms) at a time inside the device callback**. The runtime's mutex is
    taken 64 times back to back every 341 ms, so the game thread waits behind the burst.
  - It keeps every chunk forever: about 23 MB of memory per minute.
  - It adds up to 341 ms of variable latency between a game event and its sound.
  - Burst cost (`examples/voice_load_probe.rs`): 24 voices ≈ 64 × 130 µs ≈ 8 ms per burst; 96 voices ≈ 33 ms. The
    19:01 build added voices (collision manager, contacts, wheel streams), which lengthened the bursts.
- *Fix.* `PlaybackSettings::ONCE`. The stream never ends, so it plays forever without buffering. Same block
  sequence, about 5 ms latency. Test `native::tests::the_stream_renders_one_block_per_pull_unless_looped`: a looped
  stream renders 64 blocks on its first pull, ONCE renders 1.
- *Readout for real play.* `SKATE_AUDIO_TIMING=1` (`game_audio/timing.rs`) logs one `AUDIO_TIMING` line per second.
  It gives max, average and count for each audio system, the game-thread and audio-thread lock waits, the render
  per block and the slowest frame. It also reports `state_log_dropped=N` when rows were dropped.

**Third listening test (19:19 build, ONCE fix).** User:
- "much better for almost every sound";
- "only every now and again did a grind not quite sound right";
- "the ambience sounded great";
- "the rolling sound was almost perfect, there was one instance where it ran a tiny bit long after I jumped";
- "a few tiny hitches … should still be looked into at the end";
- "the landings and pops sounded almost perfect!".

User-confirmed, ready to retire in the "remove the interim cue tables" step (not deleted yet): `cues::POP`,
`POP_TAIL`, `LAND_NORMAL` / `LAND_HOLLOW` / `LAND_KIND_CHANCE` / `land_tier` / `land_scale`, `cues::LAND`,
`cues::POWERSLIDE*`.

**Hitches in `observe` (19:20 session, `AUDIO_TIMING`).** The per-second maximum of `observe` was 751 ms, 554 ms
and 143 ms on three frames. The median was 36 µs. `observe` wrote the `SKATE_AUDIO_STATE_LOG` rows on the game thread,
and a flush or OS stall blocked the frame.
- Fix: `state_log::write` now only formats the row and does a `try_send` into a bounded queue of 4,096 rows. A
  thread, `audio-state-log`, creates the file, writes the rows in order and flushes every 6 rows. When the queue is
  full the row is dropped and counted, and the count shows in AUDIO_TIMING.
- Two other hitches (197 ms and 144 ms frames) had every audio system under 0.2 ms. They are not audio, and are
  noted for the frame-timing todo.
- The remaining small costs belong to the final optimisation pass (below): mixmap_frame p90 220 µs, grain_bed
  58 µs, render_block 157 µs, game lock wait at most 131 µs.

**"Riding sound continues too long" (19:29 session) — the riderless board.**
- Replaying the windows headless (new tool
  `tools/audio-e2e/frame_levels.py`, which prints per-frame folded dBFS and per-voice-group gains next to the
  state) shows the bed is not lingering after takeoff: gain A drops to 0 within 3 frames of air.
- The audible part came after the air phase. Our engine put the skater into Offboard (state 500) and Wipeout (300),
  and in those states the **riderless board's** contacts (`physics.riding.ground`) kept flickering: 1/8/12/15 wheels
  at 3–7 m/s.
- At 39.3 s that produced a bed + Class_wheels_skid burst at **−28.8 dBFS**: skid voice gain 0.245, grain 0.176,
  against −53 dBFS in the air just before.
- Retail's audio record reports no wheels through such stretches. The clean GREC sessions show whole stretches moving
  at speed with wheels 0, air 0 and material 143, with no flicker (e.g. 180430 at 246 s, 22 m/s for more than 1 s;
  five stretches over 20 frames).
- Fix (`skate_events::board_unridden`, audio only): in states 300 and 500–502 the record gets wheel count 0, no wheel
  or deck contacts, materials 143 and seam 0. Ground speed is kept, as retail does. The interim cues see
  `rolling = false` there too.
- After the fix: **−79 dBFS**, then silence. In state 300 the bed stops at once.
- UNCERTAIN: the record writer itself (+152 bits 20–22) is not located; this rests on the measured retail behaviour.
- Physics note for the main session: the user called these "mid-air tricks", but our engine moved the skater into
  Offboard or Wipeout.

**Walking with the board (19:32 test).** The user picked the board up and put it down repeatedly while walking:
"sounded right, nothing out of ordinary fired". The log still shows 23 skid posts and 48 wind post/release cycles in
state 500. Those voices are held at about 0 by the MixMap, the speed and the no-contact inputs. **Post counts are
not audible sound**: measure the rendered level before calling something a bug. The wasted posts belong to the
optimisation pass.

**Air tricks #1 and #3 (19:38 session, "played in the air and at the landing").** All 8 tricks rendered headless:
- In the air: the bed is at 0 within 4 frames, and every trick sits at −49…−56 dBFS.
- The pop's roll layer (id 1111 → record 742, samples 625/626/260) plays at **gain 0** throughout, as in retail:
  163809 has 3 and 7 voices of streams 625/626, gain median 0.000. Its trigger is retail's (`sub_824B9CC8`: ground
  speed above 12 / 8 / 4 m/s, vault `C26949FCB638A2CA`, or byte +344).
- Our "+344" stand-in is `audio_trick ≥ 0`. Retail's +344 is record+148 bit 23, meaning unrecovered. All three ids
  are 1111, so this changes nothing.
- The audible in-air voice is SFXObj_Wheels' spin-down: 0.21–0.36, against retail's Whls_spins_Jump_1 median 0.240.
  It is a recording of wheels spinning down, entered 5.6 s in at 30 km/h, and can sound like rolling. It is retail's
  mechanism, the same in all 8 tricks.
- At landing: #3 lands on a new surface (tag 3 → 4), so the grain bed rebinds, as retail's does.
- The only state difference between #1/#3 and the rest is one frame of state 103 with no wheels and not airborne
  before the air phase. It does not change the native render.
- Open: the interim layer and frame pacing are not in e2e. A short retail ollie at about 30 km/h in the recomp would
  settle the spin-down.

**Grinds (19:20 session, "now and again a grind didn't sound right").** Six grinds: families 0/2/3/4 on tags 3/4,
including a family change 0 → 3 mid-grind (states 401 → 403 at 28.25 s). The GRINDS voices follow the family (slots
13/14 → 6/7/8). The routing records show the GRINDS voices go to eEQChain bus 5 (jittered EQ, below) and to the
FlangeSub return. That return is not modelled yet and is the remaining audible difference on grinds.

**Interim voices through the native fold** (native mode only, `voices::native_fold_gain`):
- With the native host running, every interim (Bevy) voice gets the gain that makes it reach the ears with the power
  a native voice of the same per-voice gain has after Pan2D1 and the stereo fold.
- Non-positional: mono 0.5, stereo 0.707.
- Positional: by azimuth, compared with rodio's ear factors. Ahead −3.5 dB, behind −6.6 dB.
- **rodio 0.20's spatial source is mirrored.** It gives the ear *farther* from the source the larger factor:
  ((d_this − d_other)/gap + 1)/4 + 0.5. With the native host on, the listener's ears are swapped so interim sounds
  come from the right side. The default path (no native host) is unchanged; a decision for the main session.

### The buses: environment (reverb) network, eEQChain buses, owner one-shot buses; Class_Seams (2026-10-02, `skate_audio::bus`, `player::seams`)

**Why.**
- User: ambience and cars "don't sound like they're in the distance".
- User: the board put-down's "effects the original engine uses aren't recreated".
- Every voice used to mix dry into one bus.

The specs come from our reading of the TU3 code in two research passes: `audio-specs/aems-env-bus-spec.md` and
`audio-specs/aems-eqchain-buses-spec.md`. They supersede §6 of the voice-graph spec.

**Environment network** (`bus/env.rs`; boot `sub_826DD3A8` → `sub_8248EEE8`):
- *Input.* `[[0x830CFDEC]+52]` is never 0 in retail, so **every standard voice has Send A**: pre-gain, N → 1 mono,
  level = VOL × FXWET0 (property 5).
- *Graph.* EnvSendSub (199) splits into two sides A/B. Per side:
  - EnvSub (200): Gai0 → PeakingIir2, with the EQ frequency swept by the LFO task;
  - RvrbSub (201): pre-delay Del0 → **ReverbModel1** (six Moorer combs with prime delays from the space size, then
    one all-pass; T60 / size / brightness) → Gai0 → two rvrbfiltsub (HPF → LPF → Gai0 → Pan2D1 at 270° / 90°, LFE
    0.5);
  - two rvrbdelaysub echo taps (Del0 with feedback → HPF → LPF → Gai0 → Pan2D1). Their delay and pan are swept per
    block by the "LFO Task" (`sub_82490B60`).
  - Everything goes into SFX Master.
- *Presets.* 24 vault presets (`204CAC1FD77088B8`, `aud_reverb/reverbNN`, 44 values by record offset) applied as
  `sub_8248DD18` posts them.
- *Preset choice.* The district's `audio_reverb` region layer at the skater (default reverb01). A change loads the
  other side with weight 0 and crossfades the two weights linearly over 1 s of game time (`sub_824DE548`).
- *Emitters.* FXWET0 is the MixMap send word: out8 = −2600 mB + camera-distance roll-off 4 → 70 m. A near emitter
  gets a wet tail and echoes; a far one loses them. Its dry level has no distance roll-off. That is retail's
  distance cue. The zone ambience bed has no Send A.
- New DSP modules: `dsp/delay.rs` (whole-sample delay, ±0.99 feedback, 128-sample tap crossfade),
  `dsp/reverb.rs` (ReverbModel1) and `dsp/peaking.rs` (PeakingIir2, plus the DCl0 hard clip).

**eEQChain buses** (`bus/eqchain.rs`; `sub_82490CA0`, resolver `sub_82491108`, re-roll `sub_824916E8`, clear
`sub_82491180`):
- *Graph.* Eight buses (order 253): DCl0 → PI20 → PI20 → SFX Master. Index 8 = SFX Master itself.
- *Buses 0–4* (vault `AA801D9FC0ADBBBF`, records from the image table 0x8224DC78) re-roll all six EQ values on the
  first create-flagged use after each clear. Each value is b + (k·(a − b))·0.1, with k = r mod 11 from our instance
  of the title generator (no draw when a == b). Validated against retail: every non-default PEAK line of
  buses 0–2 sits on that grid (all_163809: 228/228, 207/207, 2013/2016).
- *Buses 5–7* take the shared jitter walk's values: keys `E17029CE…` and five more. These are channels of our
  existing `SFXObj_Jitter` table, now exported with their keys.
- *Clear.* Every second game frame, or every frame when frames are longer than 20 ms.
- *Who uses which bus:*
  - routing records 0–7 / 10–17 (+10 = may re-roll) on AEMS voices. GRINDS → bus 5, skid → 5, foot drag / wind /
    rattle → 7, as the programs' records say;
  - pop → owner bus at Contacts level(14), then bus 0;
  - touchdowns and the manual landing → owner bus at level(15), then bus 0;
  - landing impact → bus 1;
  - foot taps and scuffs → bus 1;
  - collision manager → SFX Master (its Collision SubMix bus, `sub_824D25E0`, is UNCERTAIN).

**Owner one-shot buses** (`sub_82488DD0`): Sub0 → Sen0 (env, at the owner's level when built) → Sen0 (eEQChain bus).
Modelled per voice: the panned output summed 6 → 1 × the level goes into the env input, and the voice's output goes
to the bus.

**SFX Master / output.** The trace has SFX Master's gain at 1 and LPF/HPF open, so it is an identity here; its DCl0
level is not traced. Retail's output-channel byte 0x8306705D is **always 6** (Dac setup `sub_82B20EF8`): retail
hands 6 channels to the console, and the stereo downmix is the platform's. The recomp host's is 0.4·(FL + SL +
0.5·C). We keep the title's own N = 2 table (0.707 / 0.5 / 0.5) as our stereo fold. That is the game's matrix,
though retail does not run it. No change.

**Class_Seams** (`player/seams.rs`; ctor `sub_824C1198`, create `sub_824C13D0`, process `sub_824C14C8`, trigger
`sub_824C1698`, crossing `sub_824C1CA0` / `sub_824C1BA8`, hit `sub_824C1DF8`, update `sub_824C1F18`):
- *Packets.* Four 20-word per-wheel packets, held for the component's life.
- *Material change.* A hit with the transition surface.
- *Grid mode.* The wheels' world positions are rotated by the pattern's angle and divided by its grid. One hit per
  axle per line crossing, after the minimum frames; the rear axle is skipped in manuals.
- *Distance mode* (slats): front then rear hits every `spacing` of ground speed × 100 × dt.
- *Update.* MixMap Cracks outputs.
- *Kept retail quirks:*
  - the cells start at 0x7FFFFFFF and the frame counter runs per axis, so the first frame at speed fires one hit per
    axle;
  - the turn word slews ±100 per packet write.
- *Engine inputs.* AudioState gets `wheel_position` (the wheel bodies' world positions). The engine already
  published the seam patterns (`WheelLineState.seam_patterns`). The state log gains `seam0`, `seam3`, `wheel_x`,
  `wheel_z` and `heading` for replays.
- *Tuning.* The pattern fields are exported with the seam wobbles. Seams_Bank joins the player banks and is decoded at
  start (first-trigger rule).

**Evidence and numbers.**
- *Dry identity.* With `E2E_BUSES=0` the 12 standard e2e renders are bit-identical to the previous stage's on 7.
  The other 5 differ by at most 1.2e-7 (relative 1.4e-7), only from the changed summation order through the bus
  inputs.
- *Wet.* Env + EQ on: the wet part is −6.7 / −6.9 dB under the dry on grinds (EQ bus 5 included), −9.7 / −9.9 dB on
  brake and slide, −22.6 dB on the ollie, and below −40 dB on plain rolling (the bed has no Send A; its graph-2 env
  send is not modelled yet).
- *Send A against retail* (local script `senda_bank.py` on 163809, per-voice send medians):

  | bank | ours (mean) | retail (median) |
  |---|---|---|
  | GRINDS | 0.0142, send/gain 0.16 | 0.0062 / 0.19 |
  | WHEEL_SKID | 0.0256 / 0.28 | 0.0160 / 0.51 |
  | FOOT_DRAG | 0.0037 / 0.13 | 0.0112 / 0.17 |
  | owner buses | 0.079 | 2590/32767 = 0.079 in the capture |
  | sense_of_speed | **0** | 0.0150 |
  | Skate_Collisions Splice voices | none | 0.0146, send/gain 0.35 |

  sense_of_speed: the programs post property 5 = 0 in our evaluator; the source of retail's value is not found
  (open). Skate_Collisions: retail's Splice voice graph has a Send A, its level rule is not recovered (open).
- *Seams through the real bank.* Sidewalk pattern at 10 / 20 / 30 km/h: voice gain median 0.045–0.049, p90
  0.054–0.063. square_8 at 20 km/h: 0.027 / 0.050. Retail Seams_Bank per voice (163809): non-zero p10 0.010 /
  median 0.074. First hit = later hits.
- *Cost.* Env network about 21 µs per block. All buses add about 105 µs per block at p50 (render p50 19 → 124 µs,
  budget 5,333). The eEQChain buses filter every block and are the main share: an optimisation-pass item.

**Dev install.** The local tool `stage_bus_tuning.py` adds `bus_tuning` and refreshes `player_tuning` (jitter keys,
seam pattern fields). Manifest backup: `audio_manifest.before-bus-stage.json`. Both keys are optional
and older builds ignore them. Setup writes the same keys (`audio_export.bus_tuning`).

**Not modelled / open.**
- The FlangeSub returns (routing 4096 / 8192 / 16384: GRINDS uses one).
- Reverb-zone emitters (type 5: zone fades and the pan rotation).
- The grain chain's graph-2 env send (level(13)).
- The Splice voices' own Send A and Collision SubMix.
- sense_of_speed's FXWET0.
- The reverb module's timer delay before a new preset is heard (applied at the next block here).
- PI20 not bit-exact (no replay vectors).
- The +150 Hz manual frequency shift.
- Rolling rattle, Class_rolling, footsteps, cloth, flips/treatment.
- Step-on (Skeleton+602/+603).

**Final stage of the plan: an optimisation pass with adversarial reviews** (optimisation
workflow). Measure first (AUDIO_TIMING in real play, e2e timing headless). Change one thing at a time and keep
behaviour identical: e2e renders bit-identical, or a documented tolerance. An independent reviewer tries to break
each change, looking for hidden output changes and fake speed-ups. Then re-measure (worst case and 1 % lows).
First round done 2026-10-03: see "Optimisation pass over the native runtime" below. Candidates: eEQChain buses that filter silence, per-block `Vec` allocations in the mixer, the wind/rattle and skid
post/release thrash, the remaining small game-thread hitches.

### Listening test 6 (20:06 build): seams on brick and in the University park (2026-10-02)

User: DownTown (Aletown) brick "played too often or out of sync"; in the University's big park "an odd noise while
riding … when going over seams … too much reverb added possibly". Logs `state_20261002_200832.tsv` /
`_201324.tsv` (the clock restarts at each map load: cut by rows, `scenarios.py --cut rA-B`), rendered headless.

**Surfaces.** The brick is tag 66 with seam pattern 13 (`mini_tile`: 0.08 m grid, angle 30°) and tag 5 / pattern 7;
the park is tag 41 (Wood_1) / pattern 9 (`irregular_large`, 0.7 m, gain 0.41).

**Cause 1 — materials gated by contact (fixed).** The audio record's wheel materials (`+620..+632`) and seam patterns
(`+636..+648`) come from the wheel lines (82C079E0: each wheel's 0.2 m ray), not from contact. Our `audio_state`
gated them by `in_contact`, so every contact flicker was a material change, and Class_Seams fires a *transition* hit
(AudioSurfaceMap word 9 + 5, a different sound) on every change.
- Retail (GREC, `tools/recomp-trace/grec_material.py`, 180430, 683 s rolling): wheel 0 keeps its material on 100 % of
  3-wheel frames and 96 % of 2-wheel frames; it changes 0.9 times a second.
- Ours (contact-gated): wheel 0 changed 5.0 / 8.9 / 6.6 times a second in the 20:08 / 20:13 / 19:29 logs, 19–27 changes
  a second over all wheels (the park's wheel mask changes 16–19 times a second).
- Fix: `skate_events::audio_state` takes both from the lines (still 143 / 0 for the riderless board). The state log has
  a new `lines` column; `e2e.rs` replays it (older logs: every line hits while a wheel is down; `E2E_CONTACT_MATERIALS=1`
  replays the old gating).
- Park windows: transition hits 243 → 80 (of 635 → 531 hits) in `mp_seams2`, 118 → 40 in `mp_seams`; brick 58 → 36.
  Scripted e2e: 11 of 12 renders bit-identical, `manual15` differs (the lifted truck keeps its material) within 0.1 dB.

**Cause 2 — interim seams and knocks still playing beside the native ones (fixed).** In native mode the interim
riding bed (`cues::BED_CUES`) still fired random `Seams_Bank` samples (about 0.45 a second, at random times: "out of
sync"), and `BED_RECORDS` fired the deck knocks 1112 / 1115 / 1119 and the scuffs `sk8_foley` 94 / 95 that the native
foot taps / scuffs play; the interim `CATCH` knock (1115, 0.22 s after a flip) doubled the native taps too. All
silent now when the native player / contacts run (`cues::bed_record_native`).

**Hit rate on brick: not above retail.** Ours on tag 66 / pattern 13 at ~8 m/s: 100–220 Class_Seams hits a second
(the 0.08 m grid; 116 a second over the brick window, 224 in the 30 km/h sweep), heard as about 11 seam voices a second — the program sees the
one-frame w7 pulse only at its 32 ms walks, as in retail. Retail on the same material (wheel-0 material 65 = tag 66,
180430 seconds 443 / 451 / 452): 27–40 Seams_Bank voices a second, per-voice gain median 0.088 (ours 0.07–0.09).

**Sample variety (resolved in Listening test 8: the missing `Start_up_Play_ctl` boot utility, no trace needed).** A sweep of the packet words through the real program
(`player_audio::tests::seam_samples_by_words`, `SEAM_SWEEP=word:value`) shows the program picks one sample per
(surface w10, speed w8, soft w11): surface 1 → sample 80 (72 at speed word 0, 88 at 10000), soft → 170. So at a steady
speed ours repeats one or two samples (80 / 32 on the brick). Retail on tag 66 plays 45, 83, 170, 80, 176, 34 … in
the same seconds — the soft-wheel set (170 / 176) and other surface blocks appear. Which words differ in retail is not
known: a recomp hook on the Class_Seams packet (`sub_824C1DF8` / `sub_824C1F18`, words 7–18 per wheel) would settle it.

**Reverb in the park: retail's preset, strong echoes.** The University's `audio_reverb` region gives preset
`407AFA1D6C7CEAD8` everywhere the user rode (DownTown: `68A9E6020DF2076D`). Retail used it too: in 163809 the env
EQ's sweep runs from 928.8 Hz = 1161 × (1 − 0.2) with gain 0.1 and Q 20, that preset's values. It has two echo taps
at 0.17 / 0.18 s, gain 1.0, feedback 0.2, with a ±20 % delay wobble, which makes short seam hits audibly slap back.
Rendered bus part on the park window: −16.1 dB re dry with it, −22.6 dB with reverb01, −21.1 dB with DownTown's.
The seams' Send A matches retail (send / gain 0.22, retail median 0.225). Our env network's wet level has not been
checked against retail *output* (no trace of the network's return); a capture of a seam hit in the park would.

### Listening test 7: rolling "busy", "continues into jumps", "rougher than it is" (2026-10-02)

User: rolling "can sound faster or like more is happening than actually is"; "sometimes while jumping the rolling on
ground sound continues"; "the materials sound rougher than they are for some things". Logs
`state_20261002_203746.tsv` / `_204047.tsv` (University, 0 malformed lines), plus a recomp trace-all session at the
same spot (`all_20261002_204336`, clean).

**Cause 1: interim rolling one-shots still playing beside the native layers (fixed, not yet in a test build).**
The interim bed table (`cues::BED_CUES`) still fired random `Rolling_Rattles` (0.31/s × bed rate, level 0.44) and
`PatchBank_Rolling_Surfaces` (0.14/s) samples under `SKATE_AEMS=1`. In 21 s of riding that is about 7.7 rattles and
3.5 surface one-shots. Retail posts the rattle only on a push and plays the surface layers as continuous textures
gated by the MixMap. These samples last 0.3–1.9 s, so one started just before takeoff rang on into the jump: about a
1 in 3 chance per jump for a rattle at this session's speeds. `skate_events.rs` now skips both entries whenever the
native rattle / rolling layers run (`PlayerAudio::rattle_on` / `rolling_on`). The native layers themselves go quiet
within 10 frames of takeoff: SkateBoard MixMap levels 1, 6, 7, 9 and 10 are 0 with no wheels down
(`skate-audio/tests/rolling_banks.rs::layers_go_quiet_after_takeoff`).

**Surface choice matches retail.** Retail's wheel-0 material (GREC `+620`) is 3 = tag 4 (concrete_rough) for all
32.5 s of four-wheel rolling, the same tag our engine reports (17.2 s of tag 4, 0.7 s of tag 64). Speeds match too
(retail median 11.0 m/s, p90 12.0; ours 9.4 / 12.1). So "rougher than it is" is not a wrong surface. The rendering
of `concrete_rough_hard` and the layering on top of it are being compared with the retail capture.

**Retail facts for the other layers.** The SenseOfSpeed rattle's gain is not gated on wheel contact (level 832 at 31 km/h
both on the ground and in the air), so it keeps playing in jumps and posts again as the speed crosses 30 km/h. That is
parity with retail. Retail keeps the riding speed during jumps (±5 km/h).

### Listening test 8: concrete "too rough", brick "firing too often, sounds faster" (2026-10-02, build 21:04)

User (build with the grain chain, board layers, seams taken from the wheel lines, interim cues off): rolling was
"much better", but "the same concrete still sounds too rough"; brick "sounds better but is still firing too much or
too often and sounds like you're going faster than you are". Logs `state_20261002_210721.tsv` (University, 22.8 s on
tag 4 at a mean 31 km/h) and `_210845.tsv` (University, then DownTown; 21.6 s on tag 66 / seam pattern 13 at a mean
19 km/h). Both logs are clean. Rendered headless: brick 15 / 21 / 32 km/h, concrete
on patterns 2 / 8 / 11.

**Cause: a missing boot utility froze the Seams program's sample choice (fixed).** We traced the Class_Seams program
op by op (new example `skate-audio/examples/program_trace.rs`). Every player's sample index is built as
table(surface w10) + table(speed w8) + soft wheels + one of three globals, `send_random_0_to_9a` / `9b` / `9c`. The
program calls the function `rnd_call` on a hit. Only `Common.abk` implements that function, through its class
`Start_up_Play_ctl`. Retail posts that class once at audio boot, after `c_emitter_utility` and before
`c_foley_utility` (retail's three boot POSTs; upstream PR #4's player does the same). On each call it runs three
RandomShuffle draws over 0–9 and writes them into the globals. We neither loaded nor posted it, so the globals stayed
at their defaults (7 / 0 / 0) and each (surface, speed, soft) word set always played sample 0 of its block of eight:
- brick played 80 and 32 every time; retail 180430 plays all of 32–39 and 80–87;
- concrete played 32, 104 and 112; retail 204336 plays all of 32–39, 104–111 and 112–119.

At 20–30 hits a second, the same transient repeated as a regular buzz, which reads as "too often", "faster" and
"rough".

**What matched already (no change):**
- *Speed word.* w8 = clamp01((state+208 − 0.5) × 0.08) × 10000, with +208 in m/s. Constants `0x8209975C` 0.5 and
  `0x8208EDA4` 0.08 come from `sub_824C1F18`; ours is identical. The speed table moves the block 72 → 80 → 88 with
  speed; retail plays 80–87 at 30 km/h, as ours does.
- *Seam hit gating.* We re-read `sub_824C1698` (per axle: one wheel, minimum frames since the partner wheel's hit,
  frame counter per axis), `sub_824C1CA0` (grid cell, surface-3 scale only for class 10) and `sub_824C1BA8`
  (rotation). They match the port.
- *Rate.* We compared at matched speed and seam pattern, looking up the pattern at retail's WPPOS position in our
  logs:

  | surface | retail | ours, after the fix |
  |---|---|---|
  | concrete, pattern 8, 30–40 km/h | 21 voices/s, 16 walks/s | 28 voices/s, 18 frames/s (36 km/h) |
  | concrete, pattern 8, 40–50 km/h | 28 voices/s | — |
  | brick (material 65), 30 km/h | 27–40 voices/s | 29–31 voices/s |
- *Per-voice level.* Ours is 0.045–0.053 on concrete against retail's 0.052–0.074 (by block), and 0.08–0.09 on brick.
  Ours is not louder than retail.
- *Concrete's other layers.* The spidercrack layer 5 only plays on seam pattern 1; these windows are on patterns
  2 / 8 / 11. Graph 3 (the high-speed copy) only starts above 46 km/h; these windows top out at 44.7 km/h.

**Fix:**
- `audio_export.AEMS_EXTRA_BANKS` now includes `Common.abk` (it is programs only, with no samples).
- `native.rs` loads it and posts `Start_up_Play_ctl` after `c_emitter_utility`, and keeps it across map unloads.
- The e2e test and the player tests do the same (`seams::UTILITY_BANK` / `UTILITY`).
- For an existing dev install: `stage_extra_banks.py --tag seams-utility Common`, which now also stages program-only
  utility banks.

**Verification:**
- Hits are unchanged (722 in brick21).
- Each eight-sample block is now used evenly. In brick21, 32–39 and 80–87 each get 2–10 starts.
- Whole-mix level: −0.6 to +0.05 dB.
- `E2E_SEAM_UTILITY=0` renders are byte-identical to the 21:04 renders.
- New test `player_audio::seam_hits_shuffle_their_samples`: 146 voices in 5 s on brick at 30 km/h, all 16 samples.

**Open:**
- On cells our logs mark as pattern 11 (sidewalk, 3 m grid), retail 204336 played 2 seam voices in 1.5 s of moving
  at 40 km/h; ours plays about 12 frames a second at 33 km/h. The sample is too small to call: most of retail's
  "pattern 11" seconds come from a stopped stretch with the speed still held. A recomp trace of `+636` (wheel 0's seam
  pattern) next to WPPOS would settle whether retail sees pattern 11 there.
- Our boot order is foley utility, then `c_emitter_utility`, then `Start_up_Play_ctl`. Retail's is emitter,
  Start_up, foley. The three share one RNG, so draw order differs from retail; behaviour does not.

### The board's other layers, the grain chain, tricks / treatment, the effect returns (2026-10-02, `player::{rolling, tricks, treatment}`, `grain::chain`, `bus::{flange, zones}`)

Specs (our reading of the TU3 code in research passes): `audio-specs/aems-board-layers-spec.md`,
`aems-grain-chain-spec.md`, `aems-tricks-treatment-spec.md`, `aems-bus-leftovers-spec.md`.

**SFXObj_SkateBoard's rolling layers** (`player/rolling.rs`):
- *Surface routing* (`sub_824C5CA8`, surface of a truck `sub_824C82A8`, member `sub_824C8370`): two trucks, the
  primary one reading wheel 0's material, the other wheel 3's; one sounding truck per distinct surface. A truck
  that leaves its surface hands the sound to the silent truck instead of stopping it (retail never stops the
  local player's last sounding truck; the MixMap's level(1) mutes it in the air). GREC 180430 agrees: neither
  truck running on 621 of 310k frames. The bed now binds and stops from these events (`GrainEvent`) and the
  routing writes SkateBoard inputs 0 / 6; `grain_bed.rs` keeps its old one-truck routing only when the native
  rolling layers are off.
- *Class_rolling* (constructor `sub_824C4C18`): the non-grain rolling surfaces 7, 8, 10, 11, 12, 13 post a
  per-surface patch (selectors 1, 2, 10, 12, 11, 9) instead of a grain — this replaces the interim stand-ins in
  `cues::grain_for` (tags 8 / 10 / 70 → aggregate, 37 / 67–69 → metal) on the native path. Rolling surface 0
  (tag 90) binds asphalt_rough_hard with the `default` collection's tuning (open question closed).
- *Held layers 0 and 3* (posted once at board create, `sub_824C9830` / `sub_824C9948`) and *layer 5* while wheel 0
  is on the spidercrack pattern (`sub_824C9F68` / `sub_824CA038`).
- *Rolling_Rattle_Class* (`sub_824C6198`, `sub_824B0248`, `sub_824C80C0`): re-posted on every push plant while the
  primary truck is on a grain surface (sample = 3 × surface code + speed band).
- *c_board_slide* (`sub_824CB3C8` / `sub_824B0670` / `sub_824CB4C0`): the loose board on its back / side while the
  rider bails or walks. The `up_dot` vectors are UNCERTAIN (deck up · world up stands in).
- Every Class_rolling post reaches all four banks bound to the class, so setup now exports
  `PatchBank_SpiderCracks`, `PatchBank_Objects` and `PatchBank_RocksBounce` (and `Treatments`).
- Levels through the real banks (per-start peaks, `tests/rolling_banks.rs`): Rolling_Rattles 0.34–0.37
  (retail p90 0.30–0.36, max 0.37); SpiderCracks max 0.355 (retail max 0.29–0.35); held layer slot 15 median 0.030,
  p90 0.046 (retail 0.007–0.010 / 0.044–0.121); Objects up to 0.20 (retail p90 0.03–0.22); board_scrapes max
  0.06–0.10 (retail p90 0.10–0.12). First rattle = later rattles. In the air every layer is silent within 10 frames
  (SkateBoard levels 1, 6, 7, 9, 10 are 0 without wheels).

**The grain bus chain** (`sub_824C8878`; `dsp/fss.rs`, `dsp/shelf.rs`, `grain/chain.rs`):
- *FrequencyShiftSsb* (`sub_82B22898`): two cascades of two allpass sections (fixed image constants at
  `0x82FCE2B0`) form an I/Q pair on the shared biquad kernel; out = I·cos φ − Q·sin φ with retail's polynomial
  sin / cos; at 0 Hz it is the I path (an allpass), never a bypass. Phase error of the Hilbert pair ±0.6° at
  100 Hz–1 kHz.
- *Values per frame* (`sub_824C9058`): A = (special ? +150 Hz : 0) + the push shift envelope + D × A's shift per slope;
  B = B's base shift (−10 Hz; wood −25, metal −20) + push shift + D × B's per slope. **This is the +150 Hz manual
  shift.**
- *Graph 3* (the high-speed copy): send 0 below 46 km/h → 3.0 at 70 km/h, DCl0 ±0.09, the wobble gain, HS20 5 kHz ×
  0.65. *Wobbles* (`sub_824CB078` / `sub_824CB180`) and the graph-1 level ramp, with the quirk that truck 1 never
  receives a wobble. *Graph 2*: the env send level(13)/32767 (pre-pan mono) into the reverb network, the
  FlangeSub send level(21)/(22) carried but not rendered (no bed → return route yet).
- Retail (`dsp_20261002_185826`, local script `chain_check.py`): the graph-3 send matches exactly (189
  posts), graph-1 / graph-3 gain = the level ramp within 1e-4, wobble depth 0.290 / 0.240 against the caps 0.30 /
  0.25, only truck 0's graph-3 gains change.
- e2e: **manual15 went from −4.0 dB to +0.1 dB against the PoC** (A × 0.65 and +150 Hz; centroid 652 → 763 Hz, PoC
  77x). Rolling scenarios unchanged (≤ 0.1 dB). Graph 3 adds about +5.7 dB at 60–85 km/h (not compared with a
  retail capture).

**Tricks and Class_Treatment** (`player/tricks.rs`, `player/treatment.rs`):
- *Class_Flips* (`sub_824CBFB8` → `sub_824AFAD8`) posts on the first air frame of a trick (not at the pop: retail
  median one frame after the jump-velocity write), held while airborne with the same audio trick, released at the
  landing; ids −1 / 35 / 36 post nothing. *cloth_trick* A as soon as the trick registers (~215 ms before the air),
  B with the scorable's second audio trick (`+352`, vault field `A2C5C22C5BE725F8`, now exported as
  `player_tuning.audio_tricks_2`) when the trick ends.
- *Class_Treatment* (`sub_824DD408` / `sub_824B0080` / `sub_824DD6F0`): posted once, streams the predicted time to
  landing (Air+184) and jump height; its landing streams 13 / 14 start ~0.37 s before touchdown.
- Levels through the real banks vs retail: flip whooshes 0.370 (retail 0.347–0.365), flips slot 5 0.133 (0.134),
  cloth_trick 0.18–0.24 (0.21–0.25), Treatments 13 / 14 0.0736 / 0.0184 (0.0746 / 0.0186). First flip = second.
- Treatments slots 16 / 17 (the long-air layer) play in the recomp too, at our level (section "Treatments slots
  16 / 17" below). The game-mode globals (G+96, X+1060, G+4) are free-skate defaults.
- The interim `cues::FLIP` is silent with the native Tricks.

**Effect returns and the other bus leftovers** (`bus/flange.rs`, `bus/zones.rs`, mixer Send B):
- *FlangeSub returns* (`sub_82490270`, presets `sub_824DDF58`, levels `sub_824DF220`): Del0 (≤ 15 ms) → PI20 → env
  send → Gai0 → Pn21 → SFX Master, the delay swept 0.2–3.8 ms at 0.5 Hz (A) / 0.2–0.8 ms at 0.25 Hz (B). Routing
  4096 → A: GRINDS; 8192 → A: WHEEL_SKID_BANK; 16384 → B: Flips. Send B = the post-gain mono at property 11; retail
  GRINDS 0.0499 / skid 0.0561 (= Rail out6 / SkateBoard out20, which our components post). On a grind the return
  is a centred copy 26 dB under the dry voice. Presets exported as `bus_tuning.flange`.
- *Send A corrections:* retail sense_of_speed Send A **is 0** (its updater writes w6 = property 5 = 0 every frame) and
  Splice voices have **no Send A** (graph `PITCH GAIN SEND6`). The 0.0150 / 0.0146 in "The buses" came from a trace
  script that attributed every line to an owner's last PLAY of the session (local script `senda_bank_fixed.py`
  is the corrected one). Nothing to port.
- *Collision SubMix* (`sub_824D25E0`): collision voices play mono into their submix with an env send at the
  category's Collision output (≈ 0.100 near the camera, as the trace) and the AudioSurfaceMap words 12–16 eEQChain bus
  (all 8 = SFX Master in the vault). On by default (`SKATE_AEMS_SUBMIX=0` = the old direct route).
- *SFXObj_Reverb's selector* (`sub_824DE548`, zone blend `sub_824DEEF0`) runs in retail's order (`env.update`): the
  first region preset fades in over 1 s from reverb01. The reverb-zone emitters (`.ems` attribute type 5) are not
  wired: one record on the disc (`1F94F2F815C00368` → preset `BEEFC8E3DE04FBAE`). *Superseded: 1,040 records, now wired
  — see "FootStep SubMix, the skeleton inputs, …".*
- Not applied: the global env-send scale (manager +104 = Reverb out4): it depends on Reverb.in5, whose writer is
  unknown (the trace says in5 = 0; our hosts write 32767, which would drop every env send 4 dB). *Superseded: the writer
  is `sub_824DF468` (by the preset number); applied — see "FootStep SubMix, the skeleton inputs, …".*

**Footsteps, clothing, hands on the deck** (`player/footsteps.rs`, `player/clothing.rs`, `player/step_on.rs`; spec
`audio-specs/aems-offboard-clothing-spec.md`):
- *SFXObj_OffBoard* (`sub_824E9270`, poster `sub_824E9FD8`, updater `sub_824EAEA8`): two held
  `playercharacter_footstep` packets (25 words) plus the foot-down Splice sounds (surface layers `sub_82493E60`,
  step layers `sub_82493690`), the walking voices (`sk8_foley` 62 / 63 / 64; Skate_Collisions 1121–1123 with the
  board in hand) and the jump take-off / apex voices. On the board the push plants and the brake count as feet
  down, so this also plays the push steps.
- *Clothing* (`sub_824DBB68`): `c_cloth_falls` at a bail, the push foley `sk8_foley` 73 / 74 (eEQChain bus 0),
  `c_body_slide`.
- *Hands on the deck*: Skeleton+602/+603 are the IK's **hand** targets (limbs 2 / 3) inside the padded deck box, not
  the feet. 1124 / 1125 (hand on, air / ground) and 1126 (hand off) play when the board is picked up or put down
  by hand (Contacts `sub_824B85B0`). The interim "1126 = foot on deck" was a wrong mapping and is silent natively.
- Caller 824D8164 (the 1017 `sk8_foley` 62/63/64 events in 164620) is `SFXObj_PedestrianSFX` — pedestrian footsteps,
  not the player.
- Levels vs retail per-voice medians: walking foley 62 / 63 0.387 / 0.244 (retail 0.387 / 0.244); step layer 82 equal
  per sample; board in hand 0.155–0.28 (0.17–0.34); metal surface 0.082 (0.082); push foley 0.257 (0.18–0.26);
  hand on 0.125–0.153 (0.13–0.16), hand off 0.062–0.070 (0.071–0.081); Foley_Cloth +7 dB (guessed inputs below).
- Engine inputs: feet down (OffBoard 306 / 307, Air 449 / 450, push flags, brake), the feet's surfaces
  (`processed.right/left_surface`), toe world speeds, footstep strength, the hand targets — published. APPROXIMATE:
  the feet's vertical speeds (toe world velocity y), the body speed (COM speed). Not published: the step code
  (Skeleton+144/+160), the limb speeds and the ragdoll body-part slide records (body slide stays silent). The
  FootStep SubMix chain (HPF → LPF → PI20 → env → pan per sound) is passed through but not rendered by the mixer.
- Setup exports per material `B1CF62EA632CF13F` (has footsteps), `37320DF00471F91A` / `9D6068AB16703650` (gains).
  `SKATE_AEMS_FOOTSTEPS=0` keeps the interim steps. The interim `STEP` / `RUN_STEP` / `PUSH` / `BOARD_DOWN` cues are
  silent with the native footsteps / contacts. The state log gains `foot_down`, `foot_tag_a/b`, `hands`, `strength`,
  `foot_vy_a/b` for replays.

**Retired interim cues (native path).** The native path no longer reaches `POP`, `POP_TAIL`, `LAND*`, `land_tier`,
`land_scale`, `LAND_CLOTH` (= the native scuffs `sk8_foley` 95), `POWERSLIDE*` (user-confirmed), nor `CATCH`, `FLIP`,
the interim bed's Rolling_Rattles / PatchBank_Rolling_Surfaces / Seams_Bank / sense_of_speed cues and its knocks /
scuffs, `STEP` / `RUN_STEP` / `PUSH` / `BOARD_DOWN` (native replacements, not user-confirmed). The tables stay for the default path (native off).

**Cost.** Render p50 87–117 µs → 114–178 µs per block over the e2e scenarios (the grain chain ≈ +27 µs per bound
truck, mostly `mul_add` library calls); game thread p50 32–38 µs → 35–38 µs per frame. `voice_load_probe 96`: p50
512–520 µs with or without the returns (budget 5,333).

**e2e against the PoC (all on).** Unchanged within 0.3 dB everywhere except manual
(−4.0 → +0.1 dB, the chain) and the slide (st101 segments +3.0 → +2.2, +0.8 → +0.8, −0.2 → +0.6 dB against the PoC: the skid's flange
return and the rolling layers). With every new part off (`E2E_ROLLING=0 … E2E_CHAIN=0 E2E_FLANGE=0`) 11 of 12 renders are
bit-identical to the previous stage; roll45 differs by the graph-1 wobble (−36 dB under the signal), which the
host now posts.


### Listening after the components stage (2026-10-02, builds 21:04 and 21:12)

User verdicts, verbatim:
- Rolling "is much better, it just has issues with certain surfaces":
  - University concrete (session `state_20261002_210721`, tag 4, about 31 km/h): "the same concrete still sounds too
    rough on our engine".
  - DownTown brick (`state_20261002_210845`, tag 66 / seam pattern 13, about 19 km/h): "the brick surface sounds better
    but is still firing too much or too often and sounds like your going faster than you are".
- Footsteps, clothing and hands on the deck (`state_20261002_212254`): "the footsteps sound great, cant tell on the
  clothing but nothing intrusive sounding. pickup / put down sounds were great."

Consequences:
- Footsteps and the hand pick-up / put-down are user-confirmed. The interim STEP / RUN_STEP / PUSH / BOARD_DOWN* cues
  join the confirmed retirements.
- Both surface reports are being measured against retail (recomp sessions `all_20261002_204336` at the same University
  spot and `all_20261002_180430` on brick). The lead is the Class_Seams packet words, especially whether our speed word
  has the scale retail's program expects: a wrong scale would pick the "fast" samples at low speed.
### FootStep SubMix, the skeleton inputs, the Reverb inputs, the reverb zones (2026-10-02, `bus::submix`, `skate_events`, `bus::env`, `emitters::reverb_zones`)

**The FootStep SubMix** (`sub_82494188`, spec `audio-specs/aems-offboard-clothing-spec.md` §2.5; `bus/submix.rs`):
- One mono graph per foot sound slot of `SFXObj_OffBoard` (two feet × four slots), built once:
  `Sub0 → HI20 → LI20 → PI20 → Sen0 (env bus) → Pn21 (1 → 6) → Sen0 (SFX Master)`.
- The slot's Splice voices play into Sub0. Their 6-channel final Send sums L, C, R, Ls, Rs into the
  mono input; LFE is dropped.
- At each start of the slot's sound, `sub_82494550` posts the slot's EQ record (HPF, LPF, PI20
  centre / gain / Q), the holder's env level and its azimuth.
- The graph keeps those values, its filter history, its send ramps and its panner matrix until the
  next start. `SpliceHost::set_submix` now reaches the mixer (`Output::Submix`, `Route::mono`).
- Off: `SKATE_AEMS_FOOTSTEP_SUBMIX=0` or `E2E_FOOTSTEP_SUBMIX=0` (the old direct route).
- UNCERTAIN: Sen0 #2's level (never posted, 1.0) and Pn21's law (class defaults); the release
  de-click of a voice goes to SFX Master as for the other buses.
- Effect: the step layers keep the slot's open EQ, so they come out as before. A replay of the
  user's walking (state log 21:22, rows 462–948) differs by 5e-8. The EQ records apply to the
  surface layers (metal surface 5, special surface 7, and materials with their own footstep record).

**The engine inputs the components were missing** (no `skate-core` change; read from what the engine
already keeps; `skate_events::audio_state`). Retail's Skeleton::FillPhysOut (`sub_82BE1AE8`) and the
record writer (`sub_827A1B78`) read back from the TU3 code:

| audio state | retail | ours |
|---|---|---|
| `+740` step code | Skeleton `+144` / `+160` = the physical pose translations of toe parts 15 / 19 (Skeleton 9024 / 9280) at the OffBoard 306 / 307 plants | `StepCode::update(flags_306_307, left / right surface, record.pose[15/19][3].y)` |
| `+296` / `+292` foot "vertical" speeds | \|y\| of Skeleton `+304` / `+320` = the **angular** velocity (body `+48`; `+32` is linear) of the assembly bodies of parts 20 / 16 (assembly `+76 + 96·part`) | \|angular_velocity.y\| of `skeleton.bodies()[20 / 16]` (was the toes' linear speed, a proxy) |
| `+328` body speed | \|Skeleton `+288`\| = the angular speed of part 23's body | \|`bodies()[23]` angular velocity\| (was \|COM v\|, a proxy) |
| `+672` limb speed | 0.25 · (572 + 568 + 564 + 560): \|record velocity − Skeleton16176 (physical COM velocity)\| of parts 8, 4, 21, 17 | the same from `skeleton.record.velocities` and `reckoning.vector_16` (was 0) |
| `+528..+548` slide speeds | the ragdoll regions' tangential speed (SkeletonCollision `+944 + 4i`, published by `sub_82BD60C8` into Collision `+80..`; 0 without a contact part) | `collision_feedback.regions[i].tangent_speed` (was 0: body slide never played) |
| `+560..+580` slide tags | the regions' surface tags (`+976 + 4i`; the per-frame PhysOut reset leaves 0 without a part) | `regions[i].material_flags` |
| `+593` | SkeletonCollision byte 4009 = the face point's contact this frame (specific point 1) | `collision_feedback.specific[1].current` |
| loose board `up_dot` | SkateboardReckoning `+80` (the effective deck basis' Y = the physical deck up) · Ground `+80` (the retained wheel-contact normal); deck contact Collision `+3475`, material Collision `+12` − 1 | `skate_events::deck_up`; contact and material read raw (the riderless-board gate of the record's wheel fields does not cover them) |

- Lengths use `f32::sqrt`. Retail uses x · the twice-refined reciprocal-sqrt estimate; the two
  differ by about 1 ulp.
- The state log appends `step`, `body`, `limb`, `slide`, `deck_up` and `deck_contact`.
  `scenarios.py --from-log` copies them and the e2e replay reads them when present. Older logs keep
  the old defaults.

**Reverb.in0..6 and the global env scale** (`sub_824DF468`, `sub_824DF390`; `bus::env::reverb_inputs`):
- *Who writes them.* `SFXObj_Reverb`'s update starts with `sub_824DF468`. It writes Reverb.in0..in6
  = 0, then 32767 into one input.
- *Which input.* It comes from the number of the preset on the side being faded to (record `+48` =
  the NN of reverbNN, jump table `0x824DF3D0`):

  | presets | input |
  |---|---|
  | 1–5 | in4 |
  | 6–8 | in0 |
  | 9, 13, 14, 17, 18 | in1 |
  | 10 | in2 |
  | 21 | in3 |
  | 11, 12, 15, 16, 22 | in5 |
  | 19, 20, 23, 24 | in6 |

- *What it changes.* In5 drives F213 (Reverb out4 −400 mB) and F141 (music). In1 drives the
  emitter / ambience ducks F21 / F40 / F41 and others.
- So our fixed in5 = 32767 was wrong. At reverb01 the MixMap now gives out4 = 32730 (scale 0.99887,
  as the retail trace showed), and inside a reverb11 zone 20603 (−4 dB).
- The scale is applied each frame after the tick (`EnvNetwork::scale_frame`).
- Off: `SKATE_AEMS_REVERB_INPUTS=0` or `E2E_REVERB_INPUTS=0` (fixed in5 = 32767, no scale).

**The reverb-zone emitters** (`emitters::reverb_zones` → `native::reverb_frame` → `EnvNetwork::update`):
- *The earlier count was wrong.* The previous stage counted "one record". The disc has **1,040**
  eVolumeType-5 records in 24 attributes, each naming a reverb preset (`99FD793BC30CF0FA`; reverb02…24):
  - `reverb_downtown`: 616 records;
  - `reverb_industrial`: 240;
  - `reverb_university`: 133;
  - the parks' `sfx_` files: 51.
- *Which files load.* The map database (`F4917ACACAFAF913` field `65FA976EF23A314E`) lists the files
  a map loads. The districts load `music_`, `sfx_`, `reverb_`, `speakers_` and `crowds_`; the parks
  load one file.
- *Per frame.* The zones holding the camera (sphere / ellipsoid with inner core, as for the sound
  emitters) are listed in the order they were reached. The retail selector blends and rotates the
  reverb toward them.
- *Setup.* The emitter export adds `reverb` (16 hex digits) to kind-5 records (`_value` now reads
  an untyped `Attrib::RefSpec` as class + key). Dev install: the local tool `stage_reverb_zones.py`,
  backup `audio_manifest.before-reverb-zone-stage.json`.
- Off: `SKATE_AEMS_REVERB_ZONES=0`.
- Test `emitters::a_downtown_reverb_zone_selects_its_preset_and_raises_reverb_in5`: in the core of a
  `1F94F2F815C00368` zone, reverb11 fades in and commits, in5 = 32767, and out4 32730 → 20603.

**Boot order.** Retail posts `c_emitter_utility` → `Start_up_Play_ctl` → `c_foley_utility`. They share
one random generator, so the native host now posts in that order (the foley utility used to come
first, from the optional banks).

**Treatments slots 16 / 17** (settled 2026-10-02: the recomp plays them too):
- *Problem.* Ours plays Treatments 16 / 17 quietly in every ollie. Our recomp sessions seemed to show
  they never play there, so `+240` (w8, the predicted time to landing) looked wrong around takeoff.
- *Evidence.* The user's TREAT session (`all_20261002_223306`, University: plain ollies, one ~2 s air
  off a ramp; hook on `sub_824DD6F0`, 57,323 lines, 0 malformed) logs Class_Treatment's inputs per
  rendered frame:

  | input | ground | first air tick | in the air | landing tick | after |
  |---|---|---|---|---|---|
  | `+240` predicted time to landing | 0 | the whole prediction (0.70–0.77 s ollies, 2.01 s ramp) | −1/60 per tick | still counting (0.019, can go below 0) | 0 |
  | `+236` air time | 0 | 1/60 | +1/60 per tick | still counting (0.767) | 0 |
  | `+260` jump height | 0 | 0.22 m | rises to 1.19–1.21 m (ollies) | held | 0 |
  | `+224` (G+4 byte) | 0 | 0 | 0 | 0 | 0 |

  Values change per 60 Hz physics tick; the recomp updates the packet about 345 times a second. Our
  audio state publishes the same shape: `+240` is KnownAir's Air+184 from the physics port.
- *Root cause.* A measurement error. Treatments 16 / 17 are byte-identical to sense_of_speed 3 / 4.
  `retail_voices.py` matches PLAY payloads against the disc and took the first archive hit, so the
  recomp's 16 / 17 voices were counted as sense_of_speed. Their sample addresses sit in the
  Treatments bank's memory (0x4B22…/0x4B23…, next to 13 / 14 at 0x4B21…). The real sense_of_speed
  3 / 4 voices are at 0x4AFF…/0x4B00…. Only 10 samples on the disc are shared across banks.
- *The recomp vs ours.* We replay the recomp's own per-frame inputs through our Class_Treatment, bank
  and evaluator (`tests/player_tricks.rs` `treatment_replay_of_the_recomp_capture`).
  - Voice counts:

    | slot | 13 | 14 | 15 | 16 | 17 |
    |---|---|---|---|---|---|
    | the recomp | 18 | 18 | 1 | 18 | 18 |
    | ours (replay) | 18 | 18 | 1 | 20 | 20 |

  - In the 15 airs of 0.6–1.0 s, both play 16 and 17 once per air. The recomp starts them a median
    87 ms after takeoff, ours a median 88 ms after (42–380 ms).
  - Peak gains: the recomp 0.0042–0.0287 (median 0.0149), ours 0.0054–0.0320 (median 0.0155, +0.3 dB).
- *Change.*
  - No change to 16 / 17 or `+240`.
  - `retail_voices.py` now resolves duplicate samples by sample address (`play_resolve_all.json`;
    `--no-disambiguate` gives the old attribution).
  - `+236` now counts through the landing tick, as the recomp's does. It is Air+176, the time in the
    air *state*, and the state is still 200..300 on that tick (`skate_events::air_time_236`). Before,
    it dropped to 0 a tick early. Switches: `SKATE_AEMS_AIR_TIME_STATE=0` / `E2E_AIR_TIME_STATE=0`.
  - The e2e scenarios now pass `to_land` / `jump_height` as `+240` / `+260`. Before they were 0, so
    the e2e renders had no Treatments voices at all. Switch: `E2E_AIR_WORDS=0`.
- *Files.*
  - `crates/skate-game/src/game_audio/skate_events.rs` (`air_time_236`, test
    `air_time_holds_through_the_landing_tick`)
  - `crates/skate-game/src/game_audio/e2e.rs`
  - `crates/skate-audio/tests/player_tricks.rs` (the TREAT replay, a real test, and the diagnostic
    `treatment_replays_the_recomp_capture`; the `_by_late_prediction` diagnostic is removed)
- *Verification.*
  - skate-audio: all tests pass. game_audio: 48 pass.
  - e2e on 13 scenarios, including a real-play cut with three ollies (`ollies_log`):
    - with both switches off, every render is bit-identical to the previous renders;
    - the `+236` hold alone is bit-identical too, even with Treatments playing (it also gives identical
      voices in the replay);
    - with the air words on, `ollie20` / `ollies_log` gain Treatments 13 / 14 at 0.0746 / 0.0186 (the
      recomp: 0.0746 / 0.0186) and 16 / 17 at 0.013–0.030 (+0.01 dB overall).

**Evidence.**
- skate-audio: 180 lib tests plus the data tests pass (new: submix ×2, Reverb inputs).
- game_audio: 47 tests pass (new: the zone test).
- Python: 27 tests pass (new: the reverb RefSpec).
- e2e with every new part off is bit-identical to the previous stage on
  11 of 12 scenarios. ollie20 differs in its random flip / cloth sample picks; that comes from the
  other changes made since 20:53, not from these parts, which consume no random numbers.
- All on against all off: ≤ 3.8e-5 peak difference and −0.00 dB on every scenario. That is the env
  scale's −0.01 dB on the wet path, plus the submix.
- Tools: `tools/audio-e2e/render_diff.py DIR_A DIR_B`, renders kept locally.

### Leftovers: Treatments 16 / 17 timing, sense_of_speed figures, FootStep SubMix build values, the zone check, the other `.ems` files, fallbacks (2026-10-02, overnight)

**Treatments 16 / 17: the two remaining differences are the recomp, not the mechanism.**
- *Problem.* On the ~2 s ramp air (session `all_20261002_223306`, 102.0 s) the recomp started 16 / 17 538 / 577 ms
  after takeoff at gains 0.051 / 0.026. Ours started them at once, at 0.10. In 2 of 7 airs under 0.5 s ours also
  played a 0.004–0.005 voice where the recomp played none.
- *Root cause, the late start.* The recomp's audio thread stalled. The trace has **no audio lines at all
  (PLAY / GAIN / SEND / MOD / XMA) from 101499 to 102500 ms**, while the game thread logged 1,724 GREC / TREAT lines.
  At 102500 about 40 voices start in the same millisecond. The second late start in the long session
  (`all_20261002_223613`, 443.4 s: 16 / 17 at 1113 ms, gain 0.005) follows a 1055 ms stall in the same way. That
  session has 11 stalls > 250 ms, most of them about 1.0 s.
  - Replaying the TREAT rows through our Class_Treatment with no audio rendered during that interval gives 17 / 16 at
    650–680 / 780–810 ms after the blip, gains 0.062 / 0.046. Without the stall: 70–130 / 160–260 ms, 0.075–0.080.
  - The 30 ms air blip before the takeoff plays no part: without the stall both renders start 16 / 17 in the
    second air.
- *The short airs.* Whether a short air opens 16 / 17 depends on where the program's 32 ms walks fall against the
  packet changes and on the shared random generator. Row-by-row replays at the recomp's cadence play none on the two
  airs in question. Over both sessions the rate is the same: airs under 0.5 s with a 16 / 17 voice, ours (60 Hz
  replay) 11 of 22, the recomp 10 of 22. In the long session's airs of 0.85–3.3 s both start 16 / 17 a median
  50–90 ms after takeoff at matching levels (3 s airs: 0.15–0.19 in both).
- *Change.* None to the component.
- *Tools.*
  - The local script `audio_gaps.py SESSION` lists audio stalls.
  - `recomp_airs.py SESSION CSV` lists the recomp's Treatments voices per air, from the `retail_voices.py --csv` dump.
  - Ignored tests in `tests/player_tricks.rs`: `treatment_airs_by_walk_phase` (row cadence, every walk phase, the ramp
    air with and without the stall) and `treatment_replays_the_recomp_capture` (`TREAT_SESSION=` for the long session).
  - **Check a session for audio stalls before reading timing out of it.**

**sense_of_speed recomp figures, redone with the shared-sample fix.** Local script `sos_figures.py`
(attributes each PLAY window's bank by sample address, as `retail_voices.py` now does; `--old` = first-match).

| session | sense_of_speed voices (old → fixed) | streams 3 / 4 (old → fixed) | Treatments 16 / 17 (hidden before) | Send A lines (non-zero) |
|---|---|---|---|---|
| 163809 | 645 → 624 | 125 / 124 → 114 / 114 | 11 / 10 | 282 (11, values 0.08–0.55) |
| 164620 | 1941 → 1827 | 299 / 303 → 244 / 244 | 55 / 59 | 801 (40) |
| 223306 | 269 → 233 | 43 / 43 → 25 / 25 | 18 / 18 | 72 (2) |
| 223613 | — → 1059 | — → 128 / 128 | 24 / 26 | 440 (9) |

- Send A stays 0 (FXWET0 = 0, as the updater writes). The few non-zero lines are module-address reuse by other
  graphs; with the fix there are fewer of them, not more. The old "retail median 0.015" was such an artefact.
- *Ours against the recomp.* Ignored test `sense_of_speed_replay_of_the_recomp_speeds`: the local board's GREC
  ground speed of 223613 per 60 Hz frame through SenseOfSpeed, the real bank, MixMap and evaluator. COM speed =
  ground speed, so the wind is approximate in the air.
  - Voice counts: 1068 for ours, 1059 for the recomp.
  - Streams 0 / 1 / 3 / 4: 124 / 215 / 130 / 130 for ours, 111 / 219 / 128 / 128 for the recomp.
  - 5–9: 24 in both. 10–14: 72 for ours, 61 for the recomp. 15 + 16: 343 for ours, 347 for the recomp.
  - Streams 3 / 4 peak gain p90: 0.054 / 0.050 for ours, 0.058 / 0.056 for the recomp.
  - **No difference to fix.** Before the fix the recomp seemed to play 3 / 4 about 20 % more often than our replay
    would have.

**FootStep SubMix: the two "class default" values, read** (`sub_82494188`, plug-in descriptors in the image):
- *Pn21.* The build copies Pn21's four constructor arguments from its descriptor (`0x82FD0F98`, 40-byte records):
  front 30 (1..90), side 110 (90..180), **normalisation 2** (0..2) and rear 150. It then overwrites argument 2 with
  0.0, the same as the voice open (`sub_824A3140` passes 30, 110, 0). So the law is 0, a gain of 1.0, as `Pan2D`
  already applies. Its parameters keep their defaults except the azimuth.
- *Sen0 #2.* It is only linked (0x7FF7FFF4 → SFX Master), never posted. Sen0's only parameter is `ATTRIBUTE_SETGAIN`
  "Gain", default 1.0.
  - The recomp's SEND lines confirm this: the eight FootStep SubMix graphs (2 feet × 4 slots; pattern `SEND1 SEND6`,
    local script `submix_sends.py` on 223306) log Sen0 #2 = 1.0 from the build on and never change it.
  - The same lines show Sen0 #1 at 1.0 from the build until the holder's level is posted (e.g. 0.059).
- *Change.* `bus/submix.rs` now starts Sen0 #1 at its class default 1.0, so the first posted level ramps from there
  (`bus::env::Level::running`). Sen0 #2 and the law were already 1.0.
- *Effect.* e2e renders (13 scenarios incl. the walking replay `walk8`) are bit-identical
  with the change and with it reverted, submix on and off: the ramp runs in the block before the step's voice
  starts.

**Reverb zones: the attribute check** (vfunc92 of the emitter manager, vtable `0x822FBDC8` +92 = `sub_824A2438`):
- It reads the attribute's reverb RefSpec (`99FD793BC30CF0FA`) and passes when its key is one of the **24 reverb
  preset keys in the image table `0x8302E298`**. That is exactly the set of the 24 exported `aud_reverb` presets.
- A missing field reads the default RefSpec (key 0), which fails.
- *Change.* `emitters::zone_records` keeps type-5 records without a known preset as disabled zones instead of
  dropping them: retail's query stops at the first type-5 node that fails. All 1,040 zone records on the disc
  pass, so nothing changes in play.

**The music, speakers and crowds `.ems` files.**
- The emitter system loads all of a map's files and dispatches by eVolumeType. Every caller of the type getter in
  TU3 checks only 1 (the looping-emitter pool, `sub_828E9ED8` / `sub_828EAD90` / `sub_828EB028` / `sub_828EB0F0`),
  4 (`sub_828EB410`) or 5 (`sub_82488278`).
- *Type 4 (`music_*.ems`; 3 records in `reverb_downtown` / `reverb_industrial`) = music zones.*
  - Their attributes reference class `E93ABF17955A5264` (RefSpec `442A158E27F2B5AD`): playlists with
    `ePlayListMode`, `eChannelMapPreset` and `IPodSongInfo` / `DJSongInfo` song lists.
  - `sub_824A2330` gates a node on the playlist record's `+4` = audio `+1608`. `sub_828EB410` keeps one winner
    (smallest distance) and `sub_824A2198` stores its playlist key, volume, the float `463B21BB46415616` and its
    distance / position in the emitter system.
  - This feeds the licensed-music player, which we do not have. **Not ported.**
- *Types 6 / 7 (`speakers_*`, `crowds_*`, and the Maloof / MegaPark `sfx_` files).*
  - They have no bank, and every record carries flags. The emitter system's flags mask is audio `+1032`, or 4 when
    that is 0 or 0x80000 (`sub_8248B258`).
  - Their nodes are never dispatched by type: speakers belong to the music / event PA, crowds to event crowds
    (`0B5950C0184014C2` = crowd type 3 / 4). This needs event gameplay we lack. **Not ported.**
- *Change.* `emitters::update` now reads the sound emitters (type 1, flags 0) from all of the map's files, as
  retail. Only the `sfx_` / `skateschool` files hold type 1, so the emitter list is unchanged.

**Native as the default with missing install data** (new data-gated test
`native::tests::missing_install_parts_fall_back_without_breaking`: copies of the dev manifest with parts removed,
data folders linked):

| case | result |
|---|---|
| no AEMS projects / no `emitter_utility` bank | runtime off, warning, measured tables |
| no MixMap | native on, no player components or bed (interim player cues and rolling loop) |
| no GRINDS (a core player bank) | native on, components off (interim cues) |
| no Treatments, or its file missing | native on, treatment off, the rest on |
| no `Common` (seam utility) / no bus tuning | native on, warning |
| no Splice trees | native on, contacts and footsteps off (interim cues) |

The per-frame host code (`native`, `player_audio`, `grain_bed`, `emitters`, `skate_events`) has no `unwrap` /
`expect` outside tests. Each interim cue is gated on its own part's flag.

**Files.**
- `crates/skate-audio/src/bus/submix.rs`, `bus/env.rs` (`Level::running`)
- `crates/skate-game/src/game_audio/emitters.rs` (zone check, all files for type 1)
- `crates/skate-game/src/game_audio/native.rs` (fallback test)
- `crates/skate-audio/tests/player_tricks.rs` (`render_frames`, `replay_treat_rows`, `treatment_airs_by_walk_phase`,
  `sense_of_speed_replay_of_the_recomp_speeds`)
- Local tools (`treat`, `sos` folders)

**Verification.**
- skate-audio: all tests pass (183 lib).
- game_audio: 50 pass.
- skate-game `--no-run` builds.
- Python: 27 tests pass.
- e2e: identical with every new part off, and with it on (above).

### Listening test 9: University water "overplaying itself, doubling or restarting" (2026-10-02, build 21:57)

**Report.** At (344.2, 68.03, −322.5), beside the dark water channel by the plaza trees: "It sounds like it is
overplaying itself and doubling or restarting? its hard to tell." Session `state_20261002_215843` (0 malformed),
`SKATE_AEMS=1`.

**What played.** The state log puts the user off board and still at (344.5, −322.5) from 29 s to 119 s. In that window
the only water is `.ems` record #27, `water_fountain` (13 × 7 × 61 m ellipsoid, patch 81), posted once and never
re-posted (03:59:08 → 04:01:00). Record #50, `fountains_waterlaps_left` (6 × 4 × 47 m, patch 347), was reached only
while walking along the channel (20.7–26.7 s, 46.1–47.2 s, 122.7–131.5 s). Its 4 m vertical semi-axis around y = 67.7
puts the listener (the camera) near its edge.

**Double-play candidates, all ruled out headless.**
- Interim water: with the native runtime the `PROFILES` relay is off for every bank in the install (log tag
  `(native)`). `water.rs` is gone. Splashes fire only on water entry, and there were none. No location set played
  water (only birds and jets).
- Two records: University's other fountain record (#46) is 109 m away. #27 and #50 overlap, and retail plays both
  together too (sessions `all_20261002_180430` and `214002` show both banks in the same seconds).
- Cross-talk between banks: every `c_emitter` post reaches every loaded `c_emitter` bank. With `water_fountain`,
  `fountains_waterlaps_left` and `trees_rustle` loaded and posts 81 and 347 live, only the matching bank plays
  (`emitter_scene_probe`).
- Re-posting: release ends the post's voices at once. A re-post starts one fresh relay, never a second one. Two
  voices at most per fountain post over 15 minutes.

**What the programs do** (`crates/skate-audio/examples/emitter_scene_probe.rs`, real banks and WAVs, redelivered every
60 Hz frame as `emitters.rs` does):
- `water_fountain` is a two-player relay of ten 1.6–2.5 s one-shots at pitch 0.82–0.94. Each player has its own op-8
  shuffle bag (two op 8 blocks, 316 and 540).
- The next piece opens when the current one has about 0.17 s of source time left: 0.19–0.26 s before it ends in wall
  time.
- Over 15 minutes that gives 14.4 starts per 30 s. The same piece plays twice in a row 11 % of the time (48 / 432),
  because the two bags are independent. Each bag avoids its own repeats (0 repeats one start apart).
- `fountains_waterlaps_left` is one 37.46 s loop (loop start frame 189, clean wrap). It restarts from the top on
  every post.

**Retail, earlier sessions.** `tools/recomp-trace/retail_relay.py` lists a bank's PLAYs with their gaps.
- `water_fountain`: retail starts every 1.73–2.53 s, about one piece length apart (gap / length 0.99–1.07 at
  the traced rate). That matches the relay above. No back-to-back repeat in the 5 retail transitions, which is too
  few to tell from 11 %.
- `fountains_waterlaps_left`: retail re-posts it on fresh players 2–14 s apart as the listener crosses its edge, the
  same restart-on-entry as ours.

**Retail at the spot (WATERRELAY, `all_20261002_222155`, clean).** The user walked to the channel and stood at
(346.0, −325.7) from about 61 s to 104 s. The fountain was posted once (c_emitter POST at 47.76 s, first piece 34 ms
later) and played 24 pieces. Run `retail_relay.py SESSION water_fountain [--from MS]`.
- **Steady stretch while standing (66.8–102.2 s), 18 starts:**
  - Each relay turn draws from its own bag: A = 6 7 1 4 5 3 0 8 2 9 | 0 7, B = 6 7 1 8 2 9 0 5 3 4 | 6 8 (the first
    three of each come from the approach stretch). Each is a complete pass of all ten pieces with no repeat. The
    first pick of the next pass is never the last pick of the pass before. This is op 8 exactly as ported (avoid
    flag at the wrap), with one bag per player, as in the bank (`emitter_scene_probe` prints both bags: range 10,
    identity set).
  - Back-to-back same piece: 1 of 17 (6 %; the 0 → 0 where bag A wraps). Ours is 11 % (48 / 432). With 17
    transitions, retail's count is consistent with 11 % (P(≤ 1) ≈ 0.43).
  - Cadence: gap / piece length p10 / p50 / p90 0.99 / 1.07 / 1.09, against ours 0.99 / 1.04 / 1.08. That is
    15.3 starts per 30 s against our 14.4. The two retail outliers (1.19 and 1.58) sit on a recomp stall: no PLAY
    and a 411 ms GREC gap at 76.4–77.6 s.
- **Approach stretch (47.8–64.2 s, listener walking in):** the first six pieces came in identical pairs (6 6 7 7
  1 1): both bags made the same first three picks. They were also slow (gap / length 1.10–1.88, no overlap). Ours
  doesn't do this. A fresh post draws both bags in its first walk and B's draw is never played (`PROBE_SHUFFLES=1`
  prints every draw). Two explanations failed: a shared bag (ruled out by the steady stretch), and a slower relay
  from the pitch word alone (w4 2600 slows ours but keeps the bags apart). The session has no MOD (pitch) lines, so
  this is parked. It doubles **more** than ours, so it isn't what the user heard while standing.
- `fountains_waterlaps_left`: two brief activations while the listener settled (PLAY 57.78 s and 62.60 s, each
  voice's player reused for other sounds within 1.1 s / 1.6 s), then silent while standing. That is the same
  restart-on-entry and edge behaviour as ours. One difference: the second start came 0.57 s after its post (62.03 s)
  instead of ~20 ms, probably retail reloading the bank (it unloads banks no node uses; we keep them until the map
  changes).

**Verdict.** No doubling bug, and no code change. In the standing case the user reported, our relay matches retail
in mechanism (per-player op-8 bags, wrap rule), repeat rate and cadence. The "restarting" is authored:
- each piece's attack lands over the previous piece's tail;
- now and then the same splash plays again about 0.2 s before it ends;
- while walking, the channel loop starts over at each re-entry.

Open (parked):
- The approach-stretch pairing (both bags making the same picks), which needs an op-8 draw hook or MOD lines to
  settle.
- The re-post bank-reload delay.

Side finding: the user's session logs
`OFFBOARD_GROUND_RESULT_NONFINITE` every frame while the user stood there (physics, off-board ground job). It did not
interrupt the emitter and is not audio, but it needs its own look.

**Files.** New: `crates/skate-audio/examples/emitter_scene_probe.rs` (scene probe; prints the op-8 bags and, with
`PROBE_SHUFFLES=1`, every draw); local tool `tools/recomp-trace/retail_relay.py` (`--from/--to`, bags
by relay turn).

### Listening test 9 (seams): brick and University concrete; the console's 30 fps seam cadence (2026-10-02, build 21:34)

(Numbered with the water report above: both come from the same round of listening.)

**Problem.** The 21:34 build loads `Start_up_Play_ctl`. With it, the user said of both surfaces: "both sound better but
still need work". Earlier reports, verbatim:
- University concrete: "the same concrete still sounds too rough on our engine".
- DownTown brick: "still firing too much or too often and sounds like your going faster than you are".

Sessions (all clean, 0 malformed lines):
- Aletown brick: ours `state_20261002_213757`, the recomp `all_20261002_214002`.
- University at the PCU Library: ours `state_20261002_214224`, the recomp `all_20261002_214346`.
- A later recomp session with new seam hooks: `all_20261002_223306`.
  - SEAMPAT logs `+636`/`+648`/`+620`/`+208`, the frame time and the four wheel positions on every Class_Seams process
    call.
  - SEAMHIT logs every hit.

Local tools:
- `pairs.py`, `tagrate.py`, `agg.py`, `blocks.py`, `window_compare.py`, `hit_prominence.py`;
- `seampat.py` and `seamhit_match.py` for the hook session;
- `recomp_fps.py`.

Program probe: `skate-audio/examples/seam_pulse_probe.rs`. It drives the real Seams program and samples with per-block
w7 sequences or with a host model.

**Mechanism (asm).** `sub_82485190`, the audio manager, runs once per rendered frame: process (vtable +16), then the
MixMap tick, then update (+20).
- On every call, Class_Seams' process (`sub_824C14C8`) clears w7, then fires a hit when a wheel's grid cell changed.
- The program walks every 32 ms and sees only the latest w7.
- So a hit's pulse lasts one rendered frame, and that length decides how many hits the program sees and which of its two
  players start.

Every hit starts a "base" block (samples 32–47, the same on every surface) and a surface block (80–87 on brick, 104–119
on concrete). The share of base-block voices works as a fingerprint of the pulse length:

| pulse length | base-block share |
|---|---|
| one 60 Hz tick | 2–32 % |
| 3–5 ms | 45–57 % |
| 33 ms | every hit is seen |

**The recomp is not the console.** The recomp renders uncapped. In 223306 it calls the process every 2.9 ms (frame-time
field median 2.5 ms). The speed changes on 18 % of calls (the 60 Hz physics step); the wheel positions change on 37 %
(the rendered pose, between the two rates). So the recomp's pulses last about 3 ms. On the shipped game (about 30 fps)
they last one 33.3 ms frame.

Measured in the recomp:
- Base-block share: 41–50 % in all sessions.
- Rates: 25–36 voices/s on brick (tag 66), 21–30 on concrete (tag 4, pattern 8).
- Pattern 11 (sidewalk), **settled.** The recomp reads `+636` = 11 there (SEAMPAT). It fires 15–21 hits a second at
  20–40 km/h, but plays only about 0.5 voices per hit: 7.5 voices/s in 223306, about 3 in 214346. The sparse sidewalk
  comes from its short pulse, not from the pattern.
- Our cell computation matches retail's. From the logged positions, 400 of 520 grid hits coincide with a cell change of
  that wheel on the same call.

**Change: the console cadence at any frame rate.** User decision, verbatim: "match the shipped game, use the 30 fps
cadence however lets scale up and account for the higher framerates that the rust engine is going to be running at".
- **Virtual calls.** `player::seams::Seams::frame` runs on every rendered frame (`PlayerAudio::seam_frame`, from
  `native::mixmap_frame`). It makes virtual process calls on a fixed 30 Hz grid of real time.
- **Wheel positions.** Each virtual call reads the wheel positions at its own time, interpolated along the rendered
  trajectory: the physics samples interpolated by `Time<Fixed>::overstep_fraction`, as the presentation does. So it sees
  the same crossings the console samples.
- **The pulse.** A hit's w7 is redelivered at once and cleared on the audio clock after 6 or 7 blocks (6.25 on average,
  one console frame), via `SeamCommand::RedeliverAt` and the new `Runtime::redeliver_at`.
- **The update half** (levels, the turn word's per-write slew) also runs once per console frame.
- **The 60 Hz tick** only creates the packets and writes Cracks.in0 (`process_tick`), latched from any hit since the
  last tick.
- **Below 30 fps,** several virtual calls run in one frame.
- **Default and off switch.** On by default. `SKATE_AEMS_SEAM_PULSE=0` restores the old path: the whole process per
  60 Hz tick, at the physics positions.

**Proof of frame-rate independence.** E2E `E2E_FPS=f` runs the game's host at f fps. Over all straight rolling above
10 km/h in both logs, voices per second and base-block share per tag:

| tag | 30 fps family (30 / 29.7 / 30.3 / 20) | 59–370 fps (59, 60, 61, 140, 144, 150, 240, 360, 365, 370) |
|---|---|---|
| 3 | 16.9–20.6, 50–59 % | 17.5–21.0, 47–60 % |
| 5 | 32.9–37.6, 10–30 % | 28.0–41.0, 7–32 % |
| 66 brick | 18.4–20.7, 10–15 % | 16.6–21.0, 3–22 % |
| 4 concrete | 17.4–20.1, 20–26 % | 17.7–22.7, 19–38 % |

- Grid hits are the same at every frame rate: 2121–2141 (brick log) and 1189–1207 (University log).
- Per-voice gain is the same at every frame rate.
- The 30 fps spread is phase noise in the program's shuffle, and every higher frame rate falls inside it.
- Unit test `seams::tests::console_cadence_is_frame_rate_independent`:
  - hits within 2 % from 30 to 365 fps (5 % at 20 fps);
  - every pulse lasts 6 or 7 blocks, with a mean of 6.25.
- For comparison, our old 60 Hz host (the 21:34 build) gave tag 66 at 25.8 voices/s with a 25 % base share, and tag 4
  at 26.0 voices/s with 20 %.

**Against the recomp (a high-fps reference, not the target).**

| | ours, console cadence | the recomp |
|---|---|---|
| brick | ~19 voices/s, base ~14 % | 25–36 voices/s, 46–50 % |
| concrete | ~19 voices/s, base ~23 % | 21–30 voices/s, 41–49 % |
| sidewalk | 14–32 voices/s (every hit is seen) | 3–7.5 voices/s (short pulses) |

A per-call model run at the recomp's own rate matched the recomp within about 20 % on rate and share, so the mechanism
reproduces both machines. No console capture exists to check the 30 fps numbers directly.

**Other comparisons (no change).**
- Grain bed (the sounding truck's player A), the recomp vs ours:

  | window | gain | position |
  |---|---|---|
  | concrete, 43 km/h | 0.237 vs 0.245 | 0.78 vs 0.78 |
  | brick | 0.208 vs 0.196 | 0.494 vs 0.494 |
- Rattles: about one per push in both.
- Spidercracks and Class_rolling: none in these windows.
- Octave spectrum above 125 Hz: within about 2 dB. The recomp's capture also holds ambience bass.

**Open.**
- **Grain pitch.** The recomp's GREC pitch is about 0.97 (0.966–0.991 by band). Ours is 0.995 in e2e, where the
  listener is synthetic and gives no Doppler. SkateBoard pitch(3) carries the MixMap's Doppler term, so in game the
  camera decides it. Not yet measured in our game.
- **Hit prominence.** A hit's high-frequency rise over the rolling median is 2.8–6.0 dB in the recomp's capture and
  5.5–11 dB in ours. Voice gain matches, so the cause is downstream: pitch, bus or masking. *Re-measured in "The MixMap
  on the console cadence; seam-hit prominence re-measured": the recomp figures were taken ~110 ms before the hits.*
- **Other per-frame rules.** On the console the whole audio manager is per rendered frame at 30 fps. That covers the
  MixMap tick (dt 1/30), the eEQChain jitter every second frame (15 Hz on the console, 30 Hz in ours), the Doppler slew
  per evaluation, and other components' one-frame pulses. Only Class_Seams follows the console cadence so far. These are
  listed for the host owner. *Done for the MixMap, the Jitter and the eEQChain clear (next section); the eEQChain clear
  is 30 Hz on the console, not 15 Hz.*
- **Landed latch.** Wheel 0's landed latch stays per 60 Hz tick. *Correct: the conditioners run per physics step (next
  section).*

**Validation.**
- skate-audio: 181 lib tests and the data tests pass.
- `game_audio::`: 47 tests pass.
- The `--no-run` build and `cargo check` pass.
- Default e2e renders (one call per row, the old path) are byte-identical to the 21:34-era renders of both logs.

**Files.**
- Changed:
  - `skate-audio/src/player/seams.rs` (`frame`, `process_tick`, `expire`, `RedeliverAt`, the Cracks latch);
  - `skate-audio/src/runtime.rs` (`redeliver_at`);
  - `skate-game/src/game_audio/{native,player_audio,e2e}.rs`.
- New: `skate-audio/examples/seam_pulse_probe.rs`.
- E2E switches: `E2E_FPS`, `E2E_CALLS`, and `E2E_SUBSTEPS` (diagnostic: the whole host N times per row).

### The MixMap on the console cadence; seam-hit prominence re-measured (2026-10-03, overnight)

**Problem.** Listening test 9 left two items open. First, the per-frame host rules other than Class_Seams (the MixMap
tick, the eEQChain jitter, the Doppler slew, wheel 0's landed latch) still ran on our 60 Hz step instead of the shipped
console's cadence. The user's decision for that: "match the shipped game, use the 30 fps cadence however lets scale up
and account for the higher framerates". Second, the seam hits measured more prominent above the rolling bed in ours
than in the recomp's capture (5.5–11 dB vs 2.8–6 dB high-frequency rise), and ours showed more high-frequency onsets.

**Mechanism (asm, `sub_82485190`, the audio manager; reference reading only).** The manager runs once per rendered
frame with that frame's dt and splits its work into two halves.
- Half 1: `sub_82491180` (the eEQChain clear and jitter post), the listener (`sub_8248CC08`), then every state
  manager's process (vtable +16). That reaches each SFX object's process slot +36, which includes `SFXObj_Jitter`'s
  walk step (`sub_824EF378`, vtable `0x822FC248` +36).
- Half 2: the MixMap tick (`sub_8294BAE8`, the halves' summed dt), then every update (vtable +20).
- When dt is above 0.02 s (`0x822F8DE8`), both halves run on every call. At or below it they alternate, each with the
  sum of the last two dts.
- So the console at ~30 fps evaluates the MixMap, steps the Jitter and clears the eEQChain buses **once per 1/30 s**,
  with dt ≈ 1/30. It would be the same at 60 fps, with the halves alternating at 30 Hz each.
- The eEQChain clear is in half 1, so it runs on **every** console frame (30 Hz). The "15 Hz" in the previous section was
  wrong. Our old host's rule, every second 60 Hz frame, was already 30 Hz.
- Envelopes use dt. The Doppler slew `D −= trunc((target − D)·(−0.2))` and the jitter walk are per evaluation, so at our
  old 60 Hz both ran twice as fast as on the console.
- The recomp renders uncapped (~345 fps), so its halves alternate at ~170 Hz: not the console's rate.

**Change.**
- `skate_audio::mixmap::cadence::Cadence` picks the 60 Hz host steps that carry a console evaluation: every second step
  on the fixed grid of real time the 60 Hz steps already use. `native::mixmap_frame` keeps the inputs, the components'
  process and update per 60 Hz step. On the steps that carry an evaluation it also:
  - clears the eEQChain buses, before the inputs, as half 1 does;
  - steps the Jitter walk once (`PlayerAudio::jitter_steps`);
  - ticks the MixMap with dt = `CONSOLE_DT` (1/30).
- Below 30 fps one real frame can carry two evaluations; both run.
- **Held inputs.** Retail's writers run once per evaluation, so a one-frame flag lasts a whole console frame and the tick
  always sees it. Ours write per 60 Hz step, so a flag set on the step between two evaluations would be cleared before
  the tick. The new `MixMap::hold_input` makes the next tick see the largest value written since the last one; the
  stored input keeps the last write. It is used for the six flag inputs (`native::hold_flag_inputs`):
  - Contacts 1 / 6 (landing swell, landing material);
  - Rail 1 (grind ended);
  - SkateBoard 0 / 4 (surface change, push plant);
  - Cracks 0 (seam hit).

  Every other input the host writes is a level.
- On by default. `SKATE_AEMS_MIX_CONSOLE=0` restores the old host: an evaluation per 60 Hz step with dt 1/60, the Jitter
  per step and the eEQChain clear every second step. In e2e the cadence is on with `E2E_FPS` (the game's host) and off in
  the per-row renders; `E2E_MIX_CONSOLE=0/1` forces it.
- e2e fix: below 60 fps one call spans several rows, but the listener's velocity (Doppler) and the bed's frame took one
  row's dt. They now take the call's dt, as the game passes the real frame time. This affects only `E2E_FPS` < 60
  renders; the 30 fps renders in "Listening test 9 (seams)" had this error.

**Wheel 0's landed latch: no change.** The latch is the conditioner `sub_82772FD8` (called from `sub_82772748`). That is a
physics-side PhysOut conditioner (vtable `0x82310A78`, listed among physics jobs at `0x82348E98`), not an SFX object.
The recomp shows the conditioners' outputs change at the physics rate, not per rendered frame. In session 223306 the air
time `+236` (TREAT) changes on 860 of 5,094 in-air calls (17 %), always by 1/60 s (0.0167 / 0.0166). Its "more than 5
frames in the air" are physics steps on the console too, so our per-60 Hz-step latch (`Seams::process_tick`,
`contacts`, `rolling`) already matches.

**Proof (e2e, both user logs: full1 = brick 213757, full3 = University 214224; local script
`fps_band.py`).**
- Identity with the new parts off: the default per-row renders, and `E2E_FPS=60 E2E_MIX_CONSOLE=0`, are byte-identical
  (f32 and voices) to a pre-change build of the same tree (a snapshot with this change reverted).
- Frame rate, with the console cadence on:

  | fps | full1 grain A gain p50 / p90 | full1 A pitch p50 | full3 A pitch p50 |
  |---|---|---|---|
  | 29.7 / 30 / 30.3 | 0.2204 / 0.2205 / 0.2019, p90 0.2491 / 0.2491 / 0.2488 | 0.9907 / 0.9907 / 0.9895 | 1.1069 |
  | 20 | 0.2192 / 0.2491 | 0.9907 | 1.1069 |
  | 60 / 144 / 240 / 365 | 0.2012 / 0.2488 (all four identical) | 0.9912 | 1.1074 |

  - The MixMap-driven quantities are identical from 60 to 365 fps.
  - Between 60 and 365 fps only the seams' shuffle phase moves: 18.3–20.0 seam starts/s, against 18.0–19.0 at 29.7–30.3.
  - The 60+ fps values sit in the 30 fps band or within 0.0005 of it.
- The remaining 30-vs-60 fps difference in per-second levels was already there with the old cadence: median
  |Δ| 0.61 / 0.24 dB old vs 0.42 / 0.18 dB new. It comes from the components' process running once per real frame below
  60 fps (one state sample per two physics steps), not from the MixMap.
- New cadence vs old at 60 fps: per-second level median |Δ| 0.27 / 0.08 dB, p90 0.96 / 0.41 dB. The visible change is
  the Doppler: grain A pitch p10 0.839 → 0.820, because the slew now runs at the console's 30 Hz.
- Unit tests: `mixmap::cadence::tests::evaluations_follow_the_steps_not_the_frames`,
  `mixmap::tests::a_held_flag_written_between_ticks_reaches_the_next_tick_once`.

**Seam-hit prominence, re-measured (no change made).** Local tools: `windows.py`, `perhit.py`,
`lag.py` (event-triggered average), `jitter.py`.
- **The old comparison was misaligned.** `hit_prominence.py` took the recomp's high-frequency peak 0–40 ms after the
  logged PLAY time.
  - An event-triggered average of the capture's >2 kHz envelope around ~500 hits per session puts the hits **~110 ms
    after** the logged time in every session (214002, 214346, 223306, 223613).
  - Isolated strong hits spread from 75 to 160 ms (interquartile), so the capture's alignment jitters by about ±45 ms.
  - Ours peak at +40 ms (the voice log is per 60 Hz frame).
  - So the recomp's "2.8–6 dB" was mostly the bed before the hit.
- **Aligned comparison.** Event-triggered peak over the local median, by band (four recomp sessions vs our two renders):

  | band | the recomp | ours |
  |---|---|---|
  | 0.5–1 kHz | 2.3–3.7 dB | 5.5–5.8 dB |
  | 1–2 kHz | 3.4–5.0 dB | 7.0–7.5 dB |
  | 2–4 kHz | 5.0–6.4 dB | 8.3–8.6 dB |
  | 4–8 kHz | 6.4–7.7 dB | 8.8–10.0 dB |
  | 8–16 kHz | 7.3–8.3 dB | 11.5–13.2 dB |

  The recomp's figures are blurred by its alignment jitter. Adding Gaussian jitter of 30–60 ms to our hit times moves
  ours by at most about 1 dB (the hits are dense), so a gap of about 2–3 dB below 2 kHz and 3–5 dB above 8 kHz remains.
- **Mechanism checks, all equal or not the cause:**
  - **Pitch.** Seam voice pitch p10/50/90: the recomp 0.98 / 1.10 / 1.18 (163809 MOD); ours 0.99 / 1.08 / 1.16.
  - **Record levels.** Seam voice gain over the bed's gain A while rolling at 20–50 km/h: the recomp −9.0 to −13.1 dB,
    ours −11.1 to −13.4 dB. Ours is not louder at the record level.
  - **Bus.** The seams post to eEQChain 8 = the default bus input (no EQ), like the bed's graph 2.
  - **Voices per hit.** The same distribution (about 60 % single, 25 % pairs; identical samples per hit alike).
  - **Fold.** 99 % of our render's energy is in C (the board ahead of the camera), so the capture's fold weights don't
    change the comparison.
  - **Bed spectrum.** Away from hits, relative to its 0.5–1 kHz band, the bed is within about 1.5 dB per octave up to
    16 kHz.
  - **Low-pass.** The recomp's seam voices often play low-passed: 54 % of their starts set the LPF below 24 kHz (down
    to 8,990 Hz). The Seams program maps w15 = |turn·1000| (state +204, slewed ±100 per write) to the voice LPF:
    w15 ≤ 200 → 24,971 Hz, 300 → 20,177, 600 → 15,382, 1000 → 8,990 (`program_trace`). Ours does the same. Our rolling
    |turn| is distributed like the recomp's (both p75 0, >0.25 on 4–18 % vs 13–31 % of frames), so on straight rolling
    the filter is open in both.
  - **Grain start fades.** The fade request's curve is 1.0 (`0x8231A844` in `sub_828EC2F8`) → GaF0's square-root
    curve, as ported. The window picks are bit-exact with the PoC vectors.
  - **Onsets.** Our high-frequency onsets coincide with grain starts no more often than chance: 45 % and 41 % within
    ±25 ms, with ~9 grain starts/s across both trucks' players, where chance is ~45 %.
- The remaining 2–5 dB has no mechanism found. Candidates: what else the capture holds around the board (ambience at
  6–17 dB below the bed in high frequencies, other skaters, the recomp's 25–36 seam voices/s raising the local median)
  and the bed's post-record chain. No level change: retail parity needs a mechanism, not a fitted weight.

**Files.**
- New: `skate-audio/src/mixmap/cadence.rs`.
- Changed:
  - `skate-audio/src/mixmap/{mod.rs, tests.rs}` (`hold_input`, tick split into hold + `evaluate`);
  - `skate-game/src/game_audio/native.rs` (`mix_console_requested`, `hold_flag_inputs`, the cadence in
    `mixmap_frame`);
  - `skate-game/src/game_audio/player_audio.rs` (`jitter_steps`);
  - `skate-game/src/game_audio/e2e.rs` (`E2E_MIX_CONSOLE`, the call dt fix).

**Verification.** skate-audio: 183 lib tests and the data tests pass; `game_audio::` 50 pass; the `--no-run` build
passes; the e2e identity and frame-rate band are above.

**Open.**
- The components' own per-call rules still run per 60 Hz step. Retail runs every process / update on the 30 Hz cadence
  too, but its conditioners hold pulses over several physics steps for that. Examples: Seams' turn-word slew
  (already 30 Hz), the bed's per-frame steps (`grain_bed::update` runs per rendered frame, dt-scaled), and frame
  counters in the components. Moving those is a separate, wider change; it touches components other work streams own.
- The remaining seam-prominence gap (above). A console capture, or a recomp capture with a time-exact voice log, would
  settle it.

### World sound sources: traffic, pedestrians, speech (2026-10-03, `skate_audio::world`, `game_audio/world_sources.rs`)

**Problem.** The traces had already found retail's ped voices, ped footsteps, traffic engines, horns, alarms and skids
(todo `audio-world-npc.md`). The engine has no living world (no peds, no traffic), so none of them could play. The
user asked for the foundation: the retail sound objects ported and ready, so that a future ped or vehicle system
only has to publish state.

**Root cause.** These sounds are not emitters. Each comes from a per-object SFX owner on its own MixMap slot:
- Traffic slot 4: 4 instances. TrafficEngine, TrafficSkids, TrafficHorn.
- Pedestrian slot 5: 15 instances. PedestrianSpeech, PedestrianSFX, PedBodyFall, Tazer.

Each owner is driven by its game object's state, so the owners need that state before they can sound.

**Evidence** (TU3 recompilation, reference only; spec notes `audio-specs/world-traffic-audio.md`, `world-ped-audio.md`,
`world-speech.md`):
- **TrafficEngine** (`sub_824D6110` / `sub_824D6478`): an RPM model.
  - In each "gear" (4 × 7 m/s) the target rises linearly from 0 to the record's max at the gear's top.
  - A ±8 RPM wobble is added and the result is clamped to [idle, max].
  - The RPM follows at 2000 RPM/s, rises at 1000 and falls at 4000. A fall holds the shift word w17 = 1 for 3 updates.
  - The levels are split front / rear by the car's heading against the camera.
  - The `TRAFFIC_CAR` packet's w14 is the engine patch. The record's patch 1 becomes 7 or 8 for a third each, and patch 3
    becomes 6 for half.
  - Retail loads all eight engine banks at once (recomp READs at 8.8 s). **Each bank's program destroys its
    instance unless w14 is its own number**, so every post sounds in exactly one bank. The recomp agrees: the starts of
    session `all_20261002_164620` are single-bank.
- **TrafficHorn / car alarm / TrafficSkids:**
  - Horn: posts `TRAFFIC_HORN` with a random variant 0..8. The kind is the vehicle's horn state 1–5, and a record with
    patch 0 never honks.
  - Car alarm: horn state 6 posts `c_car_alarm` (variant 0..3).
  - Skids: post `TRAFFIC_SKID`, which plays on the vehicle's skid flag.
- **PedestrianSFX:**
  - Two held `livingword_footstep` packets (one per foot), with the program stepping on the foot-down word.
  - A `sk8_foley` Splice step per plant: walk / jog / run 62 / 63 / 64 by the ped's speed, above 2.5 and 7.5 m/s.
  - The packet's shoe class (w14) picks the sample set, and **class 1 is silent**.
- **Speech:**
  - The state graphs send a speech value on state entry (`SendSpeechEvent`). PedestrianSpeech hands it to the speech
    manager on change (values 7 / 8 remapped).
  - The clips stream from `livingworldspeech.big`: 3,011 clips × 1–48 takes, mono XMA2 at 36 kHz. Each take's offset and
    SNR header are in the nested `livingworldsth.big`.
  - Which reaction plays which event was measured in the recomp: warn → 501, slam nearby → 104, collision nearby → 205 /
    204 / 202, trick nearby → 101, flee → 108, greet → 1901 / 806 / 807.

**Change.**
- `crates/skate-audio/src/world/` (new, engine independent), with the player components' process / update split:
  - `traffic.rs`: `Engine`, `Horn`, `Skids`, `Vehicle`, `EngineRecord`;
  - `peds.rs`: `PedSfx`, `PedSpeech`, `PedFootstepTuning`;
  - `speech.rs`: clip index, reaction cues, line choice, mixer slots;
  - `owners.rs`: instance pools and 3DObjPos blocks, through the existing `player::objpos`;
  - `keys.rs`: the slot 4 / 5 controller keys.
- `game_audio/world_sources.rs` (new): the `WorldOwners` resource a ped / vehicle system fills each frame, and a system
  after `native::mixmap_frame`. It assigns instances, updates and processes the owners, applies the posts, and loads the
  world banks on the first published frame.
  - **Inert by default.** With no owners it returns at once, so play is unchanged. `SKATE_AEMS_WORLD=0` turns it off.
  - Shared files touched: one module line and one plugin line in `game_audio/mod.rs`, and one optional manifest field
    (`world_tuning`) with its accessor in `library.rs`.
- Setup: `tools/asset_pipeline/world_audio.py`.
  - The 13 world banks join the decoded set.
  - The manifest gets `world_tuning` (the `aud_traffic_engine` records and the ped footstep fields).
  - `speech/livingworld.json` holds the clip index (2.3 MB).
  - Decoding the speech is opt-in with `SKATE_SETUP_SPEECH=1`: about 2.4 GB of PCM for the free-roam events.
  - Dev install tools: `stage_extra_banks.py --tag world …` and the local tool `stage_world_audio.py`.

**Verification.**
- `crates/skate-audio/tests/world_sources.rs`, headless through the real MixMap and banks:
  - A car of patch 1 at 12 m/s passing 8 m away:
    - only `C01_family01` sounds;
    - the engine pitch shows the Doppler shift: 4182 approaching, 4007 receding (4096 = 1.0);
    - the front layer peaks at the closest approach and is gone beyond ~45 m;
    - the RPM settles at 12·4000/14.
  - Patches 0–9 through all eight banks: patch n sounds only in `C0n`; 2 and 9 are silent.
  - A ped walks past and plants its feet: an AEMS step and a `sk8_foley` step per plant.
    - The sample groups our evaluation produces for shoe classes 2–5 cover **4,798 of retail's 4,949**
      `fstep_livingworld` starts (session 164620).
  - A bumped business man (voice 59) resolves to a `501` warn line of his own voice; the take renders for its 5.49 s.
- Unit tests:
  - skate-audio `world::` (14): the RPM model, the patch override, the front / rear split, horn / alarm / skid
    words, footstep packets, speech value remap, clip names, line choice;
  - game_audio:: `world_sources` (2);
  - Python `test_world_audio` (3).

**Open questions.**
- **Speech:**
  - ~~the speech manager's `.evt` rules~~ and ~~the `_n` / `_f` lines~~: ported, see "World speech: the speech
    manager's rules" below;
  - which PedestrianSpeech output sets the speech level.
- **Assignment and positions:**
  - retail's instance assignment (ours: nearest N);
  - which points the traffic position blocks 4.1–4.3 follow (ours: all three at the body).
- **Fields not traced:** vehicle `+112` / `+144`, `TrafficCarPhysics.in0`, the ped model → shoe class / voice.
- **Not ported:**
  - PedBodyFall;
  - Tazer, which needs AEMS op 38;
  - ~~NPC skaters' boards~~: foundation ported, see "NPC skaters' board sounds" below;
  - event PA / crowds / music zones, which need events and a music player.

#### World speech: the speech manager's rules (2026-10-03, overnight; `world::speech_manager`, `world::speech_rules`)

**Problem.** The line and take choice was a uniform placeholder, and the `_n` / `_f` lines and the vault tuning were open.

**Root cause.** Retail's choice runs in two layers.
- The game's speech manager maps a speech value to an event and gates it with the vault tuning.
- A generic speech library then picks the line and its takes from the `.evt` table and the clips' `.hdr` headers.
- Neither layer was read.

**Evidence** (TU3 recompilation, reference only; `audio-specs/world-speech.md`):
- **Value → event** (`sub_824AB6C8`): a switch over the speech value.
  - A `rand()` coin picks between 203 / 205, 603 / 607 and 108 / 109.
  - Values 1 / 3 play radio lines for security guards. For bums, `rand() % 3` picks 206, 207 or 1901.
- **Gate** (`sub_824ABA18`, `sub_824A8C78`, `sub_824A75F0`): the vault class `Hash_9C1F48F5D637E275`, with
  `SPCHType_1_EventID` naming the living-world event. Its `Sk8::Audio::tSpeechTuning` holds:
  - per-speaker timers: the same event after `+24` s, any line after `+8` s;
  - a not-follow list (no line within t s after event e);
  - a `rand() % 1000 × 0.1` probability against `+20`;
  - the player's speed window in km/h (`+32` / `+36`);
  - excluded challenge types;
  - zombie mode (`+60`). The timers start at 0 and restart when a line starts (`sub_824A90B8`).
- **Request words** (`sub_824D9908`, `sub_824ABD90`): ped type bit, voice variant bit, the near / far flag, the
  conversation partner's type, zombie 1 / 2. Which words an event reads is per event.
- **Line** (`sub_82973CB8`):
  - the event's probability;
  - the records in a weighted random order: weight = 4^(b >> 5) × (b & 31) of the record's first byte;
  - the first record that passes its own probability and whose field masks share a bit with the request words (0 = any).
- **Takes** (`sub_82972660`, `sub_82974220`, `sub_82973408`):
  - a record's clips play in sequence;
  - per clip, the takes not in its history ring (`.hdr` +8, usually the take count) are the candidates; when every
    take is in the ring, the one played longest ago is the only candidate;
  - a random candidate index, redrawn while it is among the last min(n/2, 10) picks of that clip in a 32-entry ring;
  - the chosen take goes into the clip's history when the line starts.
- **Near / far.** The flag is 1 when the ped's `+148` exceeds `+156`. The `.evt` itself shows that 1 means far:
  `101_51_GenPos_Grn1_far`, `104_51_grn1_Slam_far`, and all 366 `_f` / `_n` records of 101 / 104 / 202 / 203 / 501
  split that way.
- **The recomp agrees** (local tool `speech_takes.py`, 10 sessions, reads in audio stalls
  left out):
  - The take of each speech READ follows from its offset. All 57 clips played twice or more follow the history rule,
    including one full cycle (`806_82_torm1_Int_c14_tour`: 0, 1, 0). A uniform pick would have repeated a take in
    about 13.6 of them.
  - All 15 multi-clip lines are one record's clip list in order (radio chirp / line / chirp; phone greet / silence /
    call).

**Change.**
- `crates/skate-audio/src/world/speech_rules.rs` (new):
  - the `.evt` parser (`EventTable::parse`, the same layout for all four speech banks);
  - `Library::start`: the record order, the field match, the clip sequence, the history and the recent ring;
  - the generator: the add-with-carry with the image seed, which retail shares with the grain player.
- `crates/skate-audio/src/world/speech_manager.rs` (new):
  - `event_for_value`, `request_block` / `request_words`;
  - `EventTuning` and `SpeechManager::gate` / `request`;
  - `speaker_bits`: voice id → type and variant bits from the records.
- `speech.rs`:
  - `SpeechIndex::set_ids` / `clip_by_id` / `picks_to_lines`;
  - `choose` is now documented as a fallback for hosts without the rules.
- `world/mod.rs`: two module lines.
- Setup (`world_audio.py`), with no change to `audio_export.py`:
  - `speech_index` now also exports each clip's `.hdr` id and history and the parsed `.evt` (`rules`);
  - `world_tuning` gains `speech_tuning` (per bank and event).
- The game host is unchanged and still inert.

**Not ported** (fails closed, or unused by living-world data):
- the per-channel request queue (16 slots, 8 streams, timeout = event +2). The recomp plays several living-world lines
  at once;
- the interrupt rules (`sub_824A73F0`);
- external conditions and clip parameters;
- the main-cast path for peds without a living-world speaker (`+116 == 0`).

**Verification.**
- skate-audio unit tests `world::speech_rules` (6) and `world::speech_manager` (4).
- `tests/world_speech.rs` (4) on the dev install's export:
  - near / far: 366 records agree, 0 disagree;
  - voice ids map to their type and variant bits;
  - a bumped business man gets `501_59_busm1_Warn_f` far and `_n` near, and is refused inside the 15 s repeat. His 3
    takes play once each, then repeat in the same order;
  - a guard's radio line is chirp / line / chirp with two different chirp takes.
- Python `test_world_audio` (6).

**Open.**
- Which ped values `+148` / `+156` are.
- The speech level and pan (which speech-manager output drives the stream voice).
- The time unit of the queue timeout.

#### NPC skaters' board sounds (2026-10-03, overnight; `world::skaters`, `game_audio/npc_skaters.rs`)

**Problem.** The AI skaters (4–14 around the player in the user's sessions) had no board sounds, and which MixMap slot
retail uses for them was open: the Player slot has only 2 instances in free skate.

**Root cause.** Retail does not give every AI skater sounds. `CSTATEMGR_Player` owns exactly two records, one per
Player-slot instance. The local player holds the first. The second goes to **one** NPC skater: the first in the skater
list that comes within 30 m of the camera while the record is free. It runs every player component for that skater.

**Evidence** (TU3 recompilation, reference only; spec `audio-specs/world-npc-skater-audio.md`):
- **The records.**
  - The manager's init `sub_824F1F40` creates two `CSTATE_Player` records (creator `sub_824F8D60`; +68 skater index,
    +72 local byte).
  - Its update `sub_824F1FB0` walks the skater list (544-byte entries at `*(0x83083C38)+0x2F070`). It keeps a skater's
    record (matched by id) or asks for a new one.
  - NPC skaters are considered only while `sub_824F8EF8` holds: distance to `*(0x830CFDD4)` (the camera) < 30.0
    (`0x820D4924`).
  - The create `sub_824F2238` hands out the first inactive record. Only the local skater may evict one.
  - The record's update `sub_824F8E18` drops an NPC at 30 m or more, or when it leaves the list.
- **Every component runs for that skater.** The audio-state bridge `sub_824B0DA8` reads the skater entry at the record's
  index. The components' local tests (`[record+72]`) pick the non-local branches the port already has: no wind /
  rattle, one routing pass, no held layers, the collision local byte clear. Class_Seams gates only on the record's
  active byte (`+52`), so it runs for the NPC too.
- **The soft-wheel word** (`sub_824B23C8`). For a non-local skater it returns 1 when the local player's record `+84` is 0.
  The bridge writes `+84` with the local player's own `+684`. So **an NPC board plays the soft-wheel samples exactly
  when the local player's wheels are hard**.
- **PlayerPhysics in13** (`sub_824B19C8`) for a non-local skater: |its COM velocity − the local player's|, capped at 35,
  slewed by 100 /s, × 32767 / 35. The vault class is `0xC1831BDB6CB1B1EA`.
- **The recomp:**
  - The CONTACT hook's second Contacts object (`0x40C710A0`) fires in bursts that match a nearby AI board's speed and
    landing. Session `all_20261002_164620`: 326–329 s = board `468B18F0`, 25 m away, 10–11 m/s; the local player is still.
    The earlier note that both contact objects were the player's was wrong.
  - The user's board is hard. Soft grain members play 0.6–4.9 voices/s around those bursts, against 0.3–0.9/s over
    the session. Their GAIN × SEND p50 / p90: `concrete_smooth_soft` 0.026 / 0.172, `concrete_aggregate_soft` 0.012 /
    0.090, `asphalt_rough_soft` 0.002 / 0.106. The local's `concrete_smooth_hard` reads 0.238 / 0.737.
  - Tool: the local script `instance1_voices.py SESSION`.
- **The non-local collision helper `sub_824BF5F8`** is speech, not board audio. On a body impact during a bail it finds
  the skater's `CSTATE_SkaterSpeech` record and posts message 8206 / 115 (a bail grunt).

**Change.**
- `crates/skate-audio/src/world/skaters.rs` (new):
  - `Slots`: the record assignment above;
  - `component_state`: an NPC's `AudioState`, with `local` = false and `soft_wheels` = the inverse of the local
    player's;
  - `NpcSkater`: the local player's component objects for instance g. It writes PlayerPhysics, the two 3DObjPos blocks,
    Contacts, Rail and OffBoard inputs at g. It runs the rolling routing, rattle, seams, grind with its on / off
    sounds, skid, squeaks, foot drag, sense of speed and the Splice contacts. It returns the same `Command`s and hands
    its collision messages to the shared collision manager. No component code is forked.
- Shared code, each change identical for the local player (`AudioState::local` is always true there):
  - `player/inputs.rs`: `Physics` and `Contacts` get an `instance`; `Physics::write_against` gives the non-local in13;
    `write_rail_at` and `write_off_board_at` are added.
  - `player/seams.rs`: an `instance` for the Cracks key. The `!local` early returns are gone; retail has none.
  - `inputs.rs` in12, `seams.rs` w11, `components.rs` skid w8 and `contacts.rs` tier: post `s.soft_wheels`
    (`sub_824B23C8`'s value) instead of `local && soft_wheels`.
- `game_audio/npc_skaters.rs` (new): the `NpcSkaters` resource an AI-skater system fills each frame, in list order, and
  a system after `native::mixmap_frame`, like `world_sources.rs`. It uses the local player's tuning and loaded banks.
  - **Inert by default:** with nothing published it returns at once.
  - `SKATE_AEMS_NPC_SKATERS=0` turns it off.
  - `PlayerAudio::post_collisions` takes the NPC's collision messages.
  - Shared lines: one module line and one plugin line in `game_audio/mod.rs`.

**Verification.**
- skate-audio `world::skaters` unit tests (3): the assignment (30 m in / out, no eviction, list order, NaN, leaving the
  list), the soft inverse, and in13's slew and cap with the instance's blocks.
- `game_audio::npc_skaters::tests::npc_skater_rolls_and_grinds_past_the_listener` (data-gated). An NPC rolls past 8 m in
  front of the camera at 7 m/s, ollies onto a metal rail, grinds and lands, through the real banks, MixMap, Splice and
  buses at the 30 Hz evaluation cadence:
  - claimed at 30.0 m, released at 30.06 m; nothing opens a second after the release;
  - Seams_Bank voices peak at 0.046–0.065 near the camera and 0.017 at 22–30 m;
  - GRINDS 0.087–0.094 at 8 m, Skate_Metal on / off 0.10–0.12, the landing (Skate_Collisions) 0.205;
  - the seam packets carry the soft word 1 with hard local wheels and 0 with soft ones.
- Identity: `e2e_bench.sh` before / after the change (13 scenarios and 4 real sessions, row and fps300) → every output
  hash identical (`compare_runs.sh npc-pre npc-post`). skate-audio tests and `game_audio::` (60) pass.

**Open.**
- The NPC's granular rolling bed. The runtime has one `GrainBed`, driven from `skateboard(0)`; the routing's grain binds
  are collected (`NpcSkater::routed`) and dropped by the host. Most surfaces' rolling needs that second bed.
- Wheels, tricks, treatment, footsteps and clothing for the NPC instance (per instance in retail).
- What an inactive instance's inputs hold. We deactivate the 3-D blocks and release the packets.
- The manager's gate (a game-state word == 5 and `sys+560` == 1) and the AI manager's list order.
- The NPC's speech (`sub_824BF5F8`, the SkaterSpeech manager `sub_824F7D10`) belongs to the speech stage.

### Optimisation pass over the native runtime (2026-10-03, overnight)

**Problem.** The final stage of the plan: make the native audio cheaper without changing a single output bit (user:
"optimise only with proof of identical behaviour"). The audio thread renders each 256-frame block while it holds the
runtime lock. The game thread waits for that lock several times per frame (`game_lock_wait`), so render time turns
straight into frame-time jitter at 300+ fps.

**Measured first.**
- *Real play* (`SKATE_AUDIO_TIMING=1`, sessions 21:37, 21:58, 22:47):
  - `render_block` averages 263–292 µs, with a per-second maximum of ~470 µs median and 680–830 µs p90.
  - The game thread takes the lock 490–1,170 times a second (~3.5 per frame). In most seconds one of those waits
    is about one render long: `game_lock_wait` per-second maximum median 276–332 µs.
  - `mixmap_frame` averages 21–37 µs per frame (lock waits included); the MixMap tick itself is ~45 µs at 30 Hz.
  - `emitters` has 3–36 ms spikes, a few per session (below).
- *Headless.* The new bench (`tools/audio-bench/e2e_bench.sh`) runs the 13 e2e scenarios and four
  whole user sessions (state logs 21:37:57, 21:42:24, 21:58:43, 22:47:47; 24,264 rows), in `row` and `E2E_FPS=300`
  modes, and hashes every `.f32` render and voice log. Render per block on the real sessions: p50 210 µs, p99
  340–377 µs.
- *Where it goes* (temporary per-stage timers, real play, µs per block): eEQChain buses 95–104, grain bed 70–77 (its
  FrequencyShiftSsb 59), env network 22–25, voices 8–20, evaluator 5–7.
- *Root cause.* Every `f32::mul_add` without the `fma` target feature is a call into the CRT's `fmaf`. A
  256-sample biquad channel costs 1.4 µs that way and 0.43 µs with hardware FMA; an FSS `sin` costs 19 ns against
  ~1 ns. The eEQChain filters 8 buses × 6 channels × 2 peaking EQs every block, mostly on silence. The FSS evaluates
  512 polynomial `sin` / `cos` per chain per block.

**Changes (each bit-identical by construction and by test).**
1. `dsp::biquad::kernel_block` (used by `Iir2`, `PeakingIir2`, `HighShelfIir2` and the FSS allpasses). The kernel is a
   pure function of (coefficients, history, input), and its arithmetic depends on the position only through `n % 8`.
   - *Settled filter:* on an 8-periodic block (silent or constant), run one group of 8. If the history comes back
     bit-identical, the remaining groups repeat it exactly. Otherwise the rest runs from the state after the group,
     so nothing is computed twice.
   - *Silent tail:* on silent input, after the history's inputs reach +0, each feed-forward form equals the 1e-18
     bias exactly. This is checked at run time with the kernel's own expressions, so only the two feedback FMAs run.
2. `PeakingIir2::process`: a silent channel whose history is bit-identical to an earlier silent channel's copies that
   channel's output and history (jittered buses 5–7 idle with six equal histories).
3. `FrequencyShift::process`: a lane whose value is bit-identical to its previous group's (a 0 Hz shift, 40–45 % of
   chain blocks) reuses its `sin` / `cos`.
4. `dsp::wrap` instead of `%` in the reverb combs and the delay lines. It equals `%` for every input, overflowed
   ones included.
5. No allocations on the audio thread in steady state (was ~104 per block). The voice / bus / submix plane lists are
   stack arrays (`each_mut` / `each_ref`), the pan's speaker weights are an array, and the grain pick's candidate
   list and recent-window list keep their buffers. RNG draw order is unchanged: an explicit loop in
   `eqchain::resolve`.

**Proof.**
- *e2e byte identity:* `compare_runs.sh base final` → IDENTICAL for scen-row, scen-fps300, real-row and
  real-fps300 (every `.f32` and every voice log). The same against the original sources rebuilt for `E2E_FPS=30`
  and `E2E_FPS=1000` (frame-rate extremes).
- *Test-only references, bit for bit:*
  - `dsp::biquad::kernel_block_matches_the_plain_kernel`: every coefficient set, including NaN / ∞ / zero ones and
    the FSS allpasses; lengths 0–264; silent, −0.0, constant, 8-periodic and noise blocks; fresh, noisy, settled and
    non-finite histories; 3,000-block decays. It also asserts that both shortcuts ran.
  - `dsp::peaking::silent_shortcuts_match_the_plain_kernel`: the old process as reference, with bus-like 6-channel
    traffic, jitter, re-rolls, bypass and a NaN frequency.
  - `dsp::fss::repeated_lanes_reuse_the_same_trig`: the old process as reference, with 0 / ±150 Hz, tiny, huge and
    NaN shifts.
  - `dsp::tests::wrap_is_the_remainder`.
- *No allocations:* `tests/render_alloc.rs` (a counting allocator around 300 steady-state blocks of a full scene)
  asserts 0.
- *Suites:* skate-audio all pass; `game_audio::` 52 / 52; `--no-run` builds.

**Speed (two runs each, same machine, µs per render block).**

| Set | before p50 / p90 / p99 | after p50 / p90 / p99 |
|---|---|---|
| Real sessions, row | 210 / 250–260 / 340–377 | 131 / 167 / 233 |
| Real sessions, 300 fps | 211–214 / 250–293 / 347–417 | 128 / 166 / 231 |
| Scenarios, row | 120–122 / 210 / 281–288 | 84 / 140 / 193–198 |

Total render time on the real sessions is down 39 % (17.1 s → 10.4 s per 75,826 blocks). The spread between runs
is a few µs at p50 / p99. The maxima (10–38 ms) move between runs and blocks in both builds: OS preemption on a
shared machine, not code. The game-thread side is unchanged (p50 30–35 µs per tick call). The in-game lock wait
should shrink with the render; check `game_lock_wait` / `render_block` in the next `SKATE_AUDIO_TIMING` session.

**Rejected or left for the user.**
- *Hardware FMA* (`-C target-feature=+fma`, measured in a separate build): render p50
  130 → 52 µs, p99 233 → 130 µs. Every e2e output is byte-identical: the fused result is unique, and 100 M random
  `fmaf` cases agree too. It would raise the CPU baseline to FMA3 (Intel Haswell 2013 / AMD Piledriver 2012) for
  the whole game. **Done as runtime dispatch** (user decision "option 1"): see "Hardware FMA dispatch" below.
- *A safe software fma* (f64 product and sum, f32 midpoint check, library fallback): exact, but no faster (the
  kernel is latency-bound). Dropped.
- *`a * b + c` instead of `mul_add`:* not bit-identical. Never.
- *Skipping silent grain chains / FSS trig on idle chains:* their bias-level output (~1e-18) reaches the master
  bit-for-bit, so it is not skippable. Dropped.
- *Merging the game thread's lock sections:* it changes which intermediate states a render can see. Not provably
  identical. Dropped.
- *Emitter bank loads* (the 3–36 ms `emitters` spikes): `Native::ensure_bank` read and decoded a world emitter
  bank's WAVs on the game thread when its first emitter started. That cost 2.6–5.4 ms warm per bank (3.8–8.7 MB of
  f32), more from a cold disk. **Done in a follow-up:** a distance prefetch on a worker thread, see "Emitter bank
  prefetch" below.

**Files.**
- `crates/skate-audio/src/dsp/{biquad,peaking,fss,shelf,reverb,delay,pan,mod}.rs`, `bus/{eqchain,submix}.rs`,
  `grain/player.rs`, `mixer.rs`.
- New test `crates/skate-audio/tests/render_alloc.rs`.
- `crates/skate-game/src/game_audio/e2e.rs`: with `E2E_TIMING=1` it also writes `<name>.ours.timing.tsv`.
- Bench tools: `tools/audio-bench/`.

**Open.** ~~The FMA decision~~ (done: "Hardware FMA dispatch" below). MixMap tick (~45 µs at 30 Hz, game thread) not examined in depth. The emitter-bank
prefetch is done (below).

#### Emitter bank prefetch (2026-10-03, follow-up)

**Problem.** The `emitters` system took 3–36 ms on a few frames per session (`AUDIO_TIMING`, 22:47 session:
`emitters=4129/23us`). When a world emitter of a bank started for the first time, `Native::ensure_bank` read the
bank's `.abk` and decoded every WAV to planar f32 on the game thread. That costs 2.6–5.4 ms per bank with a warm disk
cache and more from a cold disk.

**Retail.** No question about retail timing arises: the change never delays or defers a start. Every start happens on
the same frame as before, and `Runtime::load_bank` runs at the same point in `ensure_bank`. Only reading, parsing and
decoding move to another thread, and their result is the same data.

**Memory decides the shape.** Decoded, a map's emitter banks (`tools/audio-bench/
emitter_bank_memory.py`) take:
- University: 41 banks, 161 MiB;
- DownTown: 44 banks, 210 MiB;
- Industrial: 51 banks, 249 MiB;
- each park: 0–7 MiB.

Prefetching a whole district would cost too much. Instead the prefetch follows the listener:
- A bank is queued when the listener comes within `AHEAD` = 60 m of the bounding sphere (largest extent) of one of its
  emitters, nearest first.
- An unused prefetched bank is dropped when the listener is more than `EVICT` = 90 m from all of its emitters.
- Loaded banks stay until the map changes, as before.

The simulation over the users' sessions (`prefetch_memory_sim.py`, board position as the listener) gives this extra
resident memory (prefetched, not yet loaded):

| Session | Map | Loaded banks (as before) | Extra, peak |
|---|---|---|---|
| state 21:37:57 | DownTown | 4 banks, 26.8 MiB | 29.7 MiB (the spawn ring) |
| state 21:58:43 | University | 3 banks, 17.8 MiB | 20.1 MiB |
| state 22:47:47 | University | 6 banks, 28.6 MiB | 19.3 MiB |

The other settings tried: 40 / 70 m gave a peak of 11–30 MiB, and 100 / 140 m gave 27–43 MiB.

**Change.**
- `library.rs`: a new `BankSource` (`Library::bank_source`) holds the root, `.abk` path and WAV paths, detached from the
  library. `BankSource::load` reads, parses (`Bank::parse`) and decodes (`decode_wavs`, which `Library::bank_pcm` uses
  too) with the same error strings `ensure_bank` had.
- New `game_audio/native/prefetch.rs`: one worker thread (`audio-bank-prefetch`, spawned on the first request) runs
  `BankSource::load` for the queued banks. `Prefetch::take` at the start returns:
  - done: the data;
  - being decoded: waits for it, which is never longer than decoding it on the game thread;
  - queued, failed, panicked or unknown: None. The worker then skips a queued bank, and `ensure_bank` loads on the
    game thread as before, with the same errors and warnings.
- Dropped or cleared banks' data is handed to the worker to free. Freeing it on the game thread cost up to 0.5 ms
  per `prefetch_near` call in the test below.
- `native.rs`: `ensure_bank` takes from the prefetch or else calls `library.bank_source(stem)?.load()?`, then runs
  `load_bank` as before. `unload_map_banks` also clears the prefetch.
- `emitters.rs`: on map load, each native emitter gets the index of its bank and a status per bank
  (idle / requested / loaded). Each frame, `prefetch_near` takes the distance to each bank's nearest emitter, queues
  and drops banks, and touches no runtime, random or node state.

**Proof.**
- *e2e byte identity:* `compare_runs.sh pf_base pf_new` → IDENTICAL for scen-row, scen-fps300, real-row and
  real-fps300. Both builds come from one snapshot of the tree, differing only in the four files
  above. The e2e harness doesn't run world emitters, but it loads every player bank through the refactored
  `bank_pcm`. The later move of frees to the worker only touches `prefetch.rs`, which the harness does not run. The
  unit tests below were rerun on the final code.
- *Identical data:* `native::prefetch::tests::prefetched_banks_are_identical_and_not_decoded_at_the_start` covers the
  66 emitter banks of DownTown and University. For each, the worker's bank equals `BankSource::load` on the game thread
  (Debug-equal), and its PCM equals both that load and `Library::bank_pcm` bit for bit (`to_bits`).
- *No decode on the game thread:* a thread-local WAV-decode counter (test builds only) stays at 0 across
  `ensure_bank` for every prefetched bank. The same banks loaded without the prefetch decode all their WAVs there.
- *Runtime untouched:* `emitters::tests::the_prefetch_follows_the_listener_and_touches_no_runtime_state` moves the
  listener past all of DownTown's emitters (3,520 frames). It checks four things:
  - requested = within `AHEAD`, minus the loaded banks;
  - nothing is kept beyond `EVICT`;
  - the starts take prefetched banks with 0 decodes;
  - the evaluator's random state and block count are unchanged by the prefetch.
- *Map change:* `unload_map_banks` empties the prefetch and unloads the map's banks. The next start loads again (same
  test).
- *Fallbacks:* `unknown_failed_and_dropped_banks_fall_back_to_the_game_thread`. A missing `.abk` gives the old error
  text, there is one slot per bank, and drop and clear work.

**Speed (headless, 66 banks, warm cache, 5 runs).**
- `ensure_bank` on the game thread (the old path): median 2.2–2.4 ms, max 5.7–16 ms.
- Prefetched: median 0.001 ms, max 0.009–0.018 ms.
- `prefetch_near` over DownTown's 176 emitters / 44 banks (3,520 calls):
  - mean 0.44 µs;
  - the first call 52–61 µs (it starts the worker thread);
  - the rest at most 14–48 µs (queuing requests).
- The e2e render and frame timings of both builds are the same within run-to-run noise. The e2e harness doesn't run
  this code.

**Adversarial review (checklist, what was tried).**
- *Purity / hidden state:* `BankSource::load` reads only the files, and `Bank::parse` and `wav_pcm` are pure
  functions of the bytes. The library's clip caches (`loaded` / `failed`) are not involved; `bank_pcm` never used
  them. Nothing on the worker touches the runtime, the MixMap, the emitters' xorshift or the evaluator RNG. The test
  checks the evaluator RNG and block count before and after 3,520 `prefetch_near` calls.
- *Order:* the runtime sees the same sequence of `load_bank` / `post` / `redeliver` / `release` calls. Bank ids are
  handed out at `load_bank`, which has not moved, so they are unchanged. The order in which banks are requested
  doesn't matter: the data waits outside the runtime.
- *Races:* `take` against a worker that is about to start on the same bank. The slot's mutex decides which comes
  first: either the worker marks it Running first and `take` waits, or `take` marks it Skip first and the worker skips
  it. A bank dropped while Running finishes on the worker and is freed there with the slot. No deadlock: `take` never
  holds the runtime lock, and the worker never takes it.
- *Failures:* missing `.abk`, missing WAVs (None slots, same as before), a panicking load (`catch_unwind` → the game
  thread loads it and panics as it always did), no worker thread (spawn failure or the channel closed) → game-thread
  loads.
- *Map change / reload:* `unload_map_banks` clears the prefetch. The emitter state rebuilds its bank table on every
  map identity change (same map, new generation included).
- *Teleport / spawn inside a reach:* the start finds its bank queued and loads it on the game thread, exactly as
  before. This is not a regression, only no gain.
- *Other `ensure_bank` callers* (boot banks, `world_sources`): they take a prefetched bank when one is there (same
  data), otherwise they load as before.
- *Is the speed-up real?* The cost did not move to the game thread: `prefetch_near` runs at mean 0.44 µs. The first
  version freed evicted banks on the game thread (max 0.5 ms per call), which the review caught; frees now run on the
  worker.

**Residual spikes.**
- An emitter that is already in reach on the first frame of a map, or right after a teleport, still loads on the game
  thread (its bank is still queued). That frame is part of the map load.
- The world-source banks (`world_sources.rs`, traffic / peds) loaded on the game thread when the first vehicle or ped
  was published. They now use the same prefetch once the living world sets `WorldOwners::expected` (see
  "Session-review leftovers").
- Map change still frees the loaded banks on the game thread (`unload_bank`, as before).
- A file changed on disk between the prefetch and the start would differ. That only happens if setup runs while the
  game is running.

**Real play still to check.** Look at the `emitters` per-second maximum in the next `SKATE_AUDIO_TIMING=1` session.

#### Hardware FMA dispatch (2026-10-03, follow-up; user decision "option 1")

**Problem.** Without the `fma` target feature every `f32::mul_add` is a call into the C runtime's `fmaf`. That was
the main render cost left after the optimisation pass (above). `-C target-feature=+fma` halved the render with
byte-identical output, but it would raise the CPU baseline for the whole game. The user's decision: one exe that picks
hardware FMA or the plain code at start-up, with `skate-audio` staying `#![forbid(unsafe_code)]`.

**Which loops.** Only the loops that call `mul_add` per sample: the Direct Form I biquad kernel and its silent-tail
feedback loop (`dsp::biquad`; used by the high/low-pass, PeakingIir2, the shelf and the FSS allpasses), the
FrequencyShiftSsb oscillator with its polynomial `sin` / `cos` (`dsp::fss`), and the linear resampler
(`dsp::resample`, every voice). Reverb, delay, pan, gain, send and the grain player have no per-sample `mul_add`. Their
`mul_add`s are once per block or per parameter change: coefficients, envelopes, phase wrap. Evidence: the dispatched
build reaches the whole-crate `+fma` build's render time (p50 ~54 µs vs 52 µs), so nothing else is left to gain.

**Change.**
- New crate `crates/skate-audio-fma` (no dependencies):
  - `body.rs` holds each loop once (`#[inline(always)]`, moved verbatim from skate-audio).
  - The `kernels!` macro wraps every body twice: `plain::name` with the normal features, and `fma::name` with
    `#[target_feature(enable = "fma")]`, which implies AVX and SSE4.1. Same source.
  - `Path` picks one. A `Path` naming the FMA copy only comes from `Path::fma()`, after
    `is_x86_feature_detected!` confirmed fma, avx, sse4.2, sse4.1, ssse3 and sse3. That check is the whole safety
    argument for the single `unsafe` call (in the macro, with a `// SAFETY:` comment; clippy
    `undocumented_unsafe_blocks` is on).
  - The process-wide choice is cached in an `AtomicU8` (`active()`; `init()` chooses, `force()` is for tests).
- `SKATE_AUDIO_FMA=0` forces the plain copy. `1` (or unset) takes FMA only when the CPU has it.
- `skate-audio`: `biquad::kernel` / `kernel_silent_tail`, `fss::{sin, cos}`, `FrequencyShift::process` and
  `Resampler::render` call the crate. `Coefficients`, `BIAS`, `INV_TWO_PI` and `WEIGHT` come from it (same paths,
  same values). `dsp::{init_fma, force_fma, fma_path, FmaPath, FmaChoice}` are re-exported. Still
  `#![forbid(unsafe_code)]`.
- Game: `game_audio/native.rs` logs `AUDIO_DSP path=fma|plain cpu_fma=… SKATE_AUDIO_FMA=…` once, at native audio start
  and before the first render.

**Proof.**
- *e2e byte identity:* 13 scenarios + 4 whole user sessions, row and `E2E_FPS=300`, `compare_runs.sh`. Three trees:
  (a) the tree before the change, from a snapshot; (b) the new tree with `SKATE_AUDIO_FMA=0`;
  (c) the new tree with FMA. All IDENTICAL: every `.f32` render and every voice log, a-b, a-c and b-c, in 9 runs
  (`fma_base{,2,3}`, `fma_plain{,2,3}`, `fma_on{,2,3}`).
- *Both copies called directly* (`skate-audio-fma/src/tests.rs`):
  - biquad and feedback: 4,000 random rounds with wild coefficients / histories / inputs (NaN, ±∞, denormals, ±0,
    random bits) at lengths 0–1000; plus 60 stable filters × 400 blocks of finite input, compared exactly.
  - `sin` / `cos`: 4 M points of the oscillator range, 4 M random bit patterns and the edge values.
  - `fss_mix`: 3,000 rounds. `resample`: 3,000 rounds; step 0, 1×, 4× and random.
- *Whole runtime on both paths* (`skate-audio/tests/fma_paths.rs`, own binary): the `render_alloc` scene (24 voices
  on all buses, eEQChain, flange, env reverb, grain truck) for 1,500 blocks. Finite: every bit identical. With a
  NaN / −NaN-payload / ∞ / denormal in a voice's PCM, or a NaN eEQChain gain (NaN biquad coefficients): NaNs in the
  same places, every non-NaN bit identical, the same voice counts.
- *Suites:* skate-audio 245 + skate-audio-fma 6 pass, with `SKATE_AUDIO_FMA=0` and with auto. `game_audio::` 60
  pass. `--no-run` builds. Both link configs build: dev `cargo build -p skate-game --bin skate3rust --release
  --locked`, and release-style `+crt-static --target x86_64-pc-windows-msvc --no-default-features` (in
  its own target folder).

**The one difference: a NaN's sign / payload.** When several operands of one operation are NaN, x86 returns the
first source operand's NaN, and which operand is first is the compiler's choice per instruction. The CRT's
`fmaf(a, b, c)` keeps its order; an inlined `vfmadd` may be commuted. In the direct tests this happened 544 times in
the wild-input biquad rounds (NaN coefficients plus NaN histories), and never with one NaN source. ∞ − ∞ gives x86's
default −NaN on both copies. (LLVM's constant folder makes +NaN instead: the tests use `black_box`.) Why it can't
change an observable output:
- Every bit-level comparison of DSP state is a memo of a pure function on the same path. Examples: the settled-filter
  repeat (history bits before / after a group), the twin-channel copy (equal history bits), the silent-tail check,
  `periodic8` / `silent` on the input, and the FSS trig reuse (lane bits). When a memo fires, its result is what the
  plain kernel gives on that path (tests `kernel_block_matches_the_plain_kernel` with NaN histories and
  coefficients, `silent_shortcuts_match_the_plain_kernel`). A different payload can only change *whether* the memo
  fires, never a value.
- NaN is absorbing in these loops: in the IIR feedback, the sums and the gains. No sample value feeds control flow:
  voice lifetimes, the evaluator, grain picks and bus routing depend on parameters, positions and the RNG. So the
  voice logs cannot see it. Casts (`as i32`), comparisons, `max` / `clamp` and `is_nan` ignore payloads.
- The output goes to rodio as f32. A NaN there is already a broken sample either way.
- In the whole-runtime NaN scenes, 0 of 4.4 M NaN output samples differed in bits: the bus sums re-pick the NaN.
- In practice NaN does not occur: 0 NaN / ∞ in the 287 M samples of the kept `base` renders (13 scenarios + 4
  sessions, both modes).

**Adversarial review** (optimisation checklist):
- *Purity:* the bodies are the old code, moved. The only new state is the cached path, which only selects a copy.
- *Signed zero / NaN / denormals:* tested directly. Only NaN payloads differ (above). MXCSR is the same for both copies.
  `round_ties_even` becomes `vroundss` in the FMA copy and is exact in both.
- *Position dependence:* `kernel_block` still passes `samples[8..]` from a group boundary. The `n % 8` association
  is inside the moved loop.
- *Block sizes:* 0, 1, 2, 7, 8, 9, 15, 16, 24, 255, 256, 264, 1000.
- *State across blocks:* 400-block decays, and 1,500 runtime blocks with moving cutoffs and azimuths.
- *Frame-rate extremes:* e2e row and 300 fps. Host code is unchanged apart from the start-up log line.
- *Integer identities:* the resampler's 16.16 stepping is moved verbatim.
- *Allocation / RNG order:* no new draws. `init()` reads the environment once, at audio start; a lazy first use
  outside the game only happens in tests and the e2e harness (`render_alloc` still asserts 0 after its warm-up).
- *Soundness:* the FMA copy can only run through a `Path` that `Path::fma()` returned after the CPU check. A race
  between two first calls stores the same answer. Without OS AVX state support, `is_x86_feature_detected!("fma")`
  is false.
- *Real speed-up:* 3 base runs and 2 clean plain / FMA runs. p99 improves with p50. The game thread is unchanged.

**Speed** (µs per 256-frame render block, real-session and scenario replays, headless; clean runs only: run 2 of plain / FMA was disturbed by other load on the machine, and run 1 of plain's scenario sets overlapped the release-style build):

| Set | before (base, 3 runs) p50 / p90 / p99 | plain (clean runs) p50 / p99 | FMA (runs 1, 3) p50 / p90 / p99 |
|---|---|---|---|
| Real sessions, row | 129 / 174–177 / 286–316 | 128–137 / 327–339 | 54–58 / 87–109 / 158–209 |
| Real sessions, 300 fps | 130–140 / 178–215 / 312–392 | 128–130 / 280–355 | 55–59 / 88–102 / 153–178 |
| Scenarios, row | 86–91 / 142–166 / 228–278 | 84–85 / 228–253 | 38–40 / 67–73 / 117–141 |

The total render time on the real sessions (row) drops from 10.4 s to 4.5–5.2 s per 75,826 blocks (−50 to −56 %). The plain
path matches the old code. The game-thread side is unchanged.

**Open.**
- In-game check: an `AUDIO_DSP path=fma` line, and `render_block` / `game_lock_wait` in a `SKATE_AUDIO_TIMING=1`
  session.
- A CPU without FMA runs the plain copy, the old code. The tests skip the comparison there, so it is untested on
  such hardware here.
- To rerun the proof:
  - in a snapshot of the old tree (own `CARGO_TARGET_DIR`): `bash tools/audio-bench/e2e_bench.sh fma_base row fps300`;
  - `SKATE_AUDIO_FMA=0 bash tools/audio-bench/e2e_bench.sh fma_plain row fps300` (own `CARGO_TARGET_DIR`),
    then the same without the variable as `fma_on`;
  - `compare_runs.sh fma_base fma_plain` / `fma_on`;
  - `cargo test -p skate-audio-fma -p skate-audio --release --locked` (with and without `SKATE_AUDIO_FMA=0`).
- The extra build folders and the snapshot are local and can be deleted once accepted.

**Files.** New: `crates/skate-audio-fma/{Cargo.toml, src/lib.rs, src/body.rs, src/tests.rs}`,
`crates/skate-audio/tests/fma_paths.rs`. Changed: `Cargo.toml` (workspace member), `Cargo.lock` (the path crate, no
external dependencies), `crates/skate-audio/Cargo.toml`, `crates/skate-audio/src/dsp/{mod,biquad,fss,resample}.rs`,
`crates/skate-game/src/game_audio/native.rs`.

### Session-review fixes: push plant timing, the push foot's plant / lift, the body poster, grind on / off sounds (2026-10-03, overnight)

Four ranked items of the review of all clean sessions (2026-10-03, analysis only; items #1, #2, #4, #5) and the e2e harness
gaps (#10). "The recomp" = skate3recomp run locally: its program logic is evidence, its frame rate and stalls are not
retail. Recomp figures below are from the nine clean sessions 163809 … 223613, stall windows excluded
(local script `rlib.py`).

**Problem.**
- Every push sound but the stroke foley played ~550 ms early, at the stroke instead of the foot plant, and the rattle
  ran at a third of the recomp's rate.
- The Contacts' push-foot plant / lift sounds (sk8_foley 84 / 85 …) never played.
- Bails had a few body impacts where the recomp has a dense layer of skin / denim / arm / leg / head / torso hits, plus
  the interim `bail_hit` / `bail_soft..hard` cues on top in native mode.
- Grinds had no on / off contact sounds (Skate_Collisions 1135–1175, Skate_Metal 525–564).

**Root causes.**
- *Push plant (#1).* The audio-state bridge `sub_824B0DA8` sets `+335` on State55's rise (the foot plant), and `+333` /
  `+334` while it holds. The host fed the animation's push contact (`flags2468` bit 27), which fires near the stroke.
  `grain_bed`'s push count used the same edge.
- *Plant / lift (#2).* `sub_824B8218` calls `sub_824BBB28` after the process. It was not ported.
- *Body poster (#4).* `sub_824BC188` was not ported. It needs the regions' impacts (audio state `+496..+516`), which the
  host did not compute. A setup export bug also lost the head and torso materials (below).
- *Grind on / off (#5).* The Rail poster `sub_824C28B0` calls `sub_824C3FC8` after each GRINDS post and
  `sub_824C4138` at the release. `components::Grind` skipped both.

**Evidence and changes.**

*#1 Push plant.*
- `skate_events::push_plant`: `push_planted` = State55, `push_trigger` = its rise (`+333 || +334` / `+335`).
  `cues.riding.pushes` counts the same edge.
- The retail split (`+333` = State55 && !`+338` && State56, `+334` = State55 && `+338`) waits for `+338` to be mapped.
  The recomp's GREC logs `+338` (the third byte of its `+336` word) if someone wants to settle it.
- State log: new `plant` column (State55). The `push` column keeps its old meaning (the animation's push edge).
  `e2e.rs` prefers `plant` when present.
- Older logs have no State55. `E2E_PLANT_FROM_FEET=1` rebuilds it from their `foot_down` bits on ground states without
  the brake (State55 && (State57 || State56)) — a proof aid only.
- Off: `SKATE_AEMS_PUSH_PLANT=0` / `E2E_PUSH_PLANT=0`.

| per push (the recomp: 137 pushes / 917 riding s; ours: 4 user logs, 32 pushes / 236 riding s) | the recomp | ours before | ours after (feet proxy) |
|---|---|---|---|
| plant foley sk8_foley 74, lag after the stroke | +548 ms | +0 ms | +550 ms (p10/p90 450/600) |
| rattle, lag after the stroke | +558–660 ms | +33–50 ms | +600 ms (p10/p90 433/650) |
| rattle per riding minute (`rattle_attr.py`, level > 0.05) | 31.3 (24.3 "other", 6.4 push) | 9.4 (0 other, 9.1 push) | 37.1 (26.7 other, 9.4 push) |
| rattle level p50 | 0.326 / 0.355 | 0.325 | 0.326 |

The plant now lands with the footstep. Most rattles come from State55 rises with no stroke nearby, in the recomp and
in ours. The remaining rate gap is +19 % (the foot-down proxy, not the game's own State55).

*#2 The push foot's plant / lift* (`player::contacts::plant_lift`, update `sub_824BF268`).
- Trigger: `+333 || +334` rising while `+123` is clear → plant (`sub_824BB8D8`); falling → lift (`sub_824BBA00`).
- Sound: `sk8_foley` by wheel 0's material kind. The kind is AudioSurfaceMap word 5 (`sub_82494EB8`; 0 for no
  material).
  - Plant ids [84, 88, 92, 80, 76], lift ids [85, 89, 93, 81, 77] (Contacts `default` fields in the code docs).
  - Kinds above 4 post id 0.
- Routing: eEQChain bus 1 (class `42AFE160E647167C` field `748CBC9727A5347F`).
- Block: [0, 1, 0, dt, 1, 1] at the start, then [level(6), pitch(1), raw(0), dt, 0, 1].
- Off: `SKATE_AEMS_PLANT_LIFT=0` / `E2E_PLANT_LIFT=0`.

| | the recomp (`plantlift.py`) | ours after (feet proxy) |
|---|---|---|
| plant on pushes | 130 / 137 (95 %), +548 ms (498 / 682), level p50 / p90 0.220 / 0.463 | 29 / 32 (91 %), +550 ms (450 / 617), 0.222 / 0.490 |
| lift on pushes | 130 / 137, +664 ms (580 / 748), level 0.155 / 0.423 | 28 / 32, +683 ms (583 / 717), 0.213 / 0.493 |
| ids | 84 > 88 > 92 (materials 1–5) | 84 / 88 / 92 by kind |

*#4 Body poster* (`player::contacts::body`, messages into the collision manager).
- Gate: runs unless the rider is bailing with `+677` set.
- Per region 0..5 with an impact and no cooldown (15 frames, vault `6DD85F43C1B6E6AA`):
  - the region material (head 97, torso 98, arms 100, legs 99) against the touched surface (`+560` tag − 1), each by
    its impact band;
  - the band-2 second message;
  - the cloth contact (denim 109 / skin 107 / denim 108) at tier 0;
  - the pad contact (110 head / torso, the face 112, 111 limbs) by `sub_824BCCF8`'s thresholds (class
    `6EBA5BCD3E38A98A`: 0.75 / 0.7 / 0.65, face 0.05 / 0.3; highs 1.25 / 0.55).
- Region impacts (`skate_events::region_impacts`, from `sub_82BD60C8`): clamp01(max(0.001, Δv(part) · region normal ×
  part mass × 10)).
  - Δv = SkeletonState4048 `velocity_changes`.
  - Mass = SkeletonState4560 = the bone-box volumes `animation_masses.part_weights`.
  - 10 = physics_collision `default` +164, field `1430BD50F0A33475`.
  - The conditioner `sub_82773298` keeps the max of the last 4 frames.
- **Export fix:** five AudioSurface records are keyed by name in the converted collections: head, torso, water,
  drum_pylon, crumpled_paper. The band holder's `default` is keyed by name too.
  - Materials 77 / 87 / 92 / 97 / 98 exported as missing, and every RefSpec to the `default` bands resolved nothing.
  - Before the fix, an impact on those materials read tier 2 with an empty window.
  - `audio_export.collision_tuning` now aliases name keys by hash64. Head = ids 1032 / 952 / 953; torso = 951 / 949 /
    950 — the ids the recomp plays in bails (below).
- Not ported, and why:
  - The first-hit torso message `sub_824BCEB0`: gated by an unidentified global at `*(0x82083C38) + 0x2FCB4`.
  - The Hall of Meat layer: global byte `+859`.
  - The non-local `sub_824BF5F8`.
  - `sub_824BDA90`: game events, not sounds.
  - `sub_824E3BE0` is not the player's ragdoll. It is a physics-prop SFX object: its own record from `[data+128]`,
    materials `+360` / `+160`, caller `sub_824E2790`. It belongs to the world-sources stage.
- The interim bail cues are silent while the body poster runs (`PlayerAudio::body_impacts_on`).
- Off: `SKATE_AEMS_BODY_IMPACTS=0` / `E2E_BODY_IMPACTS=0`.
- **The recomp, per bail** (38 bails, `fixes/bail_splc.py`):
  - 137 collision-manager voices.
  - Ids 955 25.4, 954 20.7, 956 19.2, 947 13.0, 958 8.7 (concrete), 948 6.0, 949–953 (torso / head) 14, 1031 / 1030 /
    1032 (tier 2) 5.
- **Ours.** The user's logs carry no region records yet, so no real bail can be replayed. A synthetic bail
  (`fixes/e2e_bail_on`, region impacts set by hand) plays 31 collision voices instead of 3: torso, legs, denim and
  skin, per-voice peaks 0.10–0.25. The recomp's mean is ≈ 0.15 (Σg² 4.3 over 185 voices). This shows the chain, not
  the retail density. That needs a state log with the new `rimp*` / `rtag*` columns.

*#5 Grind on / off* (`components::Grind::sounds` / `update_sounds`, `sub_824C42A8`).
- At each Class_grind post: slot 0, slot 1 for family 0, and the updater's family change, which uses the new family's
  layer.
- Sound: the grind surface's on id (`sub_824C35D0`): Skate_Metal on a metal surface (bool `02BAC36BCC8A30DE`), else
  Skate_Collisions, one id per layer. At the release, the off id (`sub_824C37D8`).
- Routing: through eEQChain bus 0 (field `D1A87641CCB98787`), mono, env send = Rail level(5).
- Level = ((10000 − speed word) × 0.0001 × (B − A) + A) × layer factor × Rail level(1). A / B = 0.1 / 1.25 on, 0.1 /
  1.0 off.
- Pitch = the same lerp (0.8 / 1.0) × pitch(2).
- Data: exported per grind surface (`player_tuning.grind[*].on / off / metal`, `grind_contact_eq`).
- Off: `SKATE_AEMS_GRIND_ONOFF=0` / `E2E_GRIND_ONOFF=0`.

| per grind | the recomp (17 grinds, `fixes/evpost.py`) | ours before | ours after (7 grinds) |
|---|---|---|---|
| on sound | 17 / 17, lag 0, level p50 / p90 0.290 / 0.454 | 0 / 7 | 7 / 7, lag 0, 0.259 / 0.383 |
| off sound | 17 / 17, lag 0, 0.204 / 0.285 | 0 / 7 | 7 / 7, first audible +50 ms, 0.245 / 0.313 |

*#10 Harness.*
- New state-log columns: `plant`, `stroke`, `deck_spin` / `spin_x` / `spin_y`, `bail`, `bail_end`, `held`,
  `offboard_air`, `footplant`, `revert`, `soft`, `face`, and `rimp0..5` / `rslide0..5` / `rtag0..5`.
- `e2e.rs` reads them when a log has `plant`, and then also sets `on_foot` = state 500. Older logs render as before.
- `audio_trick` was already resolved from the scorable inside `PlayerAudio` (e2e included); only the Script's literal
  read −1.
- `scenarios.py --from-log` copies the new columns and the skeleton columns it used to drop (step, body, limb, slide,
  deck_up, deck_contact).

**Files.**
- `crates/skate-game/src/game_audio/`: `skate_events.rs`, `state_log.rs`, `e2e.rs`, `player_audio.rs`, `library.rs`.
- `crates/skate-audio/src/player/`: `state.rs` (`body_impact`), `contacts.rs`, `components.rs`, `tuning.rs`
  (`GrindContact`, `grind_contact_eq`).
- `tools/asset_pipeline/audio_export.py`, `test_audio_formats.py`.
- Dev install: `stage_player_tuning.py` re-run. Backup: `audio_manifest.before-review-fixes-stage.json`.

**Verification.**
- skate-audio tests: 204 + integration, all pass.
- `cargo test -p skate-game --release --bin skate3rust --locked -- game_audio::`: 57 pass.
- Python `test_audio_formats` / `test_world_audio`: 31 pass.
- e2e of the four user logs (213757, 214224, 215843, 224747; `E2E_FPS=60`) with every new part off: **byte-identical**
  (f32 and voices) to a build of the tree without this pass (a snapshot, old manifest). The same holds with the
  restaged manifest, so the export fix changes nothing in these logs.

**Open.**
- A user state log with the new columns (1 min of pushing; one or two bails). It checks the game's own State55 rate
  against the recomp's 27 plants / riding minute (the feet proxy gives 29.5) and the bail density.
- ~~The lift's level is +2.7 dB over the recomp's (0.213 vs 0.155).~~ A measurement artefact; per id ours matches
  (see "Session-review leftovers").
- `+338`'s mapping.
- The first-hit torso message's global: the gate is read; its writer is still not found (see "Session-review
  leftovers").
- Needs an in-game listening check.

**Addendum: the user's session 084712 (2026-10-03, build 04:20, all fixes on).** University,
real-play state log `state_20261003_084712.tsv` (0 malformed, new columns). The user: "sounded SO MUCH BETTER, I WOULD
CALL IT ALMOST OR EXACTLY RETAIL"; the footsteps while running / walking "sounded amazing too". Local tools:
`plants.py`, `regions.py`, `bails_cmp.py`.
- *State55 plants: one per stroke, as in the recomp.* The lower rate is riding style (slow riding, walking between
  bails; confirmed by the user), not a missing plant.

  | | ours (084712) | the recomp (9 sessions, stalls excluded) |
  |---|---|---|
  | strokes (State56 rise) / riding min | 6.3 (4) | 8.8 (135) |
  | plants (State55 rise) / riding min | 14.1 (9) | 27.0 (412 plant posts) |
  | plants per stroke | 2.25 | 3.05 |
  | strokes followed by a plant within 1.2 s | 4 / 4, lag p50 500 ms (400–517) | 116 / 135, lag +548 ms |
  | plants with no stroke in the 1.2 s before | 5 (≈ 1 s apart, a foot rhythm) | 296 |

  Riding = states 100–299 with wheels, plus air: 38 s here. The brief's 28 s / 19 per minute used a narrower riding set.
- *Bails (2): the state-log side.*
  - Bail 1 at 18.25 s comes from off-board air (501 → 300, 6.5 m/s). Bail 2 at 36.23 s comes from 500 → 300 at
    7.6 m/s, after an air.
  - Region impacts are low. Bail 1: max 0.086 (region 1), the others ≤ 0.02. Bail 2: regions 0 / 4 / 5 reach 0.145 /
    0.202 / 0.206, regions 2 / 3 0.03 / 0.06.
  - Most contact frames sit near the 0.001 floor. All the touched surfaces are tag 4.
  - Contact frames > 0.001 per bail: 84 and 104.
- *The recomp, per bail, same window (−0.5…+2.5 s, voices > 0.002; 38 bails):*
  - Skate_Collisions: 207.7 voices, Σg² 4.68, peak p50 0.61.
  - Skate_Metal: on 16 / 38 bails, 6.5 voices, Σg² 0.40.
  - Bodyslide: on 33 / 38, 1.8 voices, Σg² 0.006.
  - Collision-manager posts: mean 137, p50 133.
  - Of these, ids 968 / 969 (7.9 + 3.5 per bail) are world / ped collisions, not the rider's body.
- *Ours, rendered:* done after the restart; results in "Bail density: ours rendered" below. The original note:
  the headless e2e build (own target folder) was stopped mid-compile for a PC restart. To finish it:
  - The scenario is already cut: `real_084712.tsv` (`scenarios.py --from-log … --cut
    r0-999999`).
  - Render: `E2E_DIR=<absolute scenario folder> E2E_FPS=60 cargo test -q -p
    skate-game --release --bin skate3rust --locked -- --ignored --exact game_audio::e2e::e2e_render --nocapture`.
  - Compare: the local script `bails_cmp.py - <scenario folder>:real_084712`. It reports
    voices per bank, Σg², records → ids and regions per bail.
  - Watch-point: with impacts ≤ 0.2 the bands may never reach tier 2, which the recomp plays at about 5 per bail
    (ids 1030–1032). If ours lacks them, compare `skate_events::region_impacts` (Δv · normal · mass × 10) against the
    recomp before changing the poster.

#### Bail density: ours rendered, the impact scale checked (2026-10-03, analysis only; no code change)

Session 084712 rendered headless through the e2e path (own target folder, `E2E_FPS=60`; `E2E_DIR` must be an
**absolute** path, a relative one fails with "path not found" because the test runs from the crate folder). The
state log has 0 malformed lines. `bails_cmp.py` took the time label from a missing `ms` column; it now uses row / 60.

| per bail, window −0.5…+2.5 s | the recomp (38 bails) | ours bail 1 (18.2 s) | ours bail 2 (36.2 s) |
|---|---|---|---|
| Skate_Collisions voices | 207.7 | 46 | 45 |
| Skate_Collisions Σg² | 4.68 | 1.89 | 1.53 |
| Skate_Collisions peak | p50 0.61 | 0.72 | 0.72 |
| three banks (Collisions + Metal + Bodyslide) Σg² | ≈ 5.1 | 1.95 | 1.84 |
| collision-manager posts | 137 (p50 133) | ≈ 30 (estimate: voices / 1.5, the recomp's voices per post) | ≈ 30 |
| tier-2 body ids 1030 / 1031 / 1032 (+ torso 951) | 1.2 / 2.5 / 1.3 (+ 1.8) | 0 | 0 |

**The gap is confirmed, and it is wider than tier 2.** Ours plays no tier-1 or tier-2 body id at all except the
head's 953 (3 in bail 2). The recomp's posts by material and tier (`fixes/bail_splc.py`, nine sessions):

| region material (bands: tier 1 > b8, tier 2 > b0; floor b12) | tier 0 | tier 1 | tier 2 | ours (both bails, voices) |
|---|---|---|---|---|
| head 97 (0.05 / 0.375; 0.002) | 952: 2.9 | 953: 3.1 | 1032: 1.3 | 952: 3, 953: 3 |
| torso 98 (0.1 / 0.25; 0.002) | 949: 3.9 | 950: 3.2 | 951: 1.8 | 949: 7 |
| legs 99 (0.1 / 0.35; 0.002) | 947: 13.0 | 948: 6.0 | 1031: 2.5 | 947: 9 |
| arms 100 (0.2 / 0.65; 0.005) | 956: 19.2 | 957: < 0.6 | 1030: 1.2 | 956: 6 |
| skin 107 / denim 108–109 (cloth, tier 0) | 954: 20.7 / 955: 25.4 | | | 954: 7 / 955: 25 |
| concrete 3 (1.0 / 1.85; 0.12) | 958: 8.7 | 1047: none | 991: none | 958: 0 |

In the recomp 40–60 % of the head / torso / leg posts are tier 1 or 2, so its region impacts are often above 0.1 and
several times per bail above 0.25–0.65. Ours never exceeds 0.21 in the whole log (bail 1 max 0.086).

**What scale retail expects: [0, 1], the scale we already have.** The thresholds are not in an AEMS program. The poster
picks the id by the collision material's impact bands (the AudioSurface records, `impact_band`), and the AEMS program
only plays the id. The body bands sit inside [0, 1] (floors 0.002–0.005, tier 2 at 0.25–0.65). The concrete record's
tier 1 / 2 (> 1.0 / > 1.85) cannot be reached by a clamped region impact, and the recomp never posts 1047 / 991 in a
bail, while 958 (> 0.12) plays 8.7 times. So retail's impacts are clamped to [0, 1] too. Re-read of `sub_82BD60C8`'s
region loop (0x82BD68F0…0x82BD69D4) matches `skate_events::region_impacts` term by term:
- part = SkeletonCollision `+1200 + 4i` (−1 = none);
- Δv = SkeletonState `+4048 + 16·part`;
- the region normal is `+1008 + 16i`;
- mass = SkeletonState `+4560 + 4·part`;
- × config `+164` (10);
- floor 0x82063A48 (0.001), then clamp to [0, 1];
- written to Collision `+80 + 4i`, slide speed to `+112 + 4i`.

`+4560` is the raw bone-box product that `82BEBAA8` normalises (`SkeletonAnimationMasses::part_weights`). Same
indices, same constants. **So the poster and the impact formula are not the gap. The input is:** ours has small
per-step velocity changes of the ragdoll parts along the contact normal. Rough sizes (full-extent bone boxes ≈ 0.003
arm … 0.015 torso m³): tier 2 needs Δv·n per 1/60 s step of about 1.7 m/s (torso) to 4–5 m/s (head, legs). Ours peaks
near 2 m/s, though the regions touch the ground for 1–2 s per bail (58–114 contact frames, sliding at 3–7 m/s). In
our ragdoll the parts land and slide smoothly. Retail's give sharp per-step stops and bounces.

**Not changed:** the mechanism on the audio side is already retail. Scaling the impacts or the bands would be a guessed
weighting. The cause is in the physics (the ragdoll's contact response / part velocities during a bail), not in
`contacts::body`.

Caveats:
- Two bails only (from off-board air at 6.5 m/s, and from 500 → 300 at 7.6 m/s after an air). The recomp's 38 are of
  mixed heights.
- The recomp's post count is inflated by its frame rate. It runs the poster at ~345 fps, so the 15-frame body cooldown
  is ≈ 43 ms there, against 0.5 s on the 30 fps console.

**Open (needs a decision or data):**
1. Measure retail's impacts directly: a recomp hook logging Collision `+80..+100` (or the audio state `+496..+516`)
   and the touched parts' SkeletonState `+4048` Δv per physics step during bails. This would run alongside the
   planned `PLAY_TRACE_AUDIOX.bat bail` session (the user's session). It would say whether retail's per-step Δv is
   really 2–3× ours, and whether that comes from the contact solve or from the bail drives.
   *Hook built 2026-10-03 (recomp `research-hooks`, category `audiox`; session pending).* Kinds `BAILSTEP` / `BAILREG`
   log per pass of `sub_82BD60C8`, only during the local player's bails. For each of the **8** regions with a part they
   log the impact the game wrote (Collision `+80 + 4i`; the loop runs 8 times, so `+80..+108`), |Δv·n|, the signed
   normal velocity before and after, |Δv|, the normal, the mass, the slide speed and the tag. They also log the update
   interval. The local block comes from the skater-entry update `sub_827A1B78`: entry `+152` bit 31, X =
   `[[character + 1808] + 1800]`. Found on the way: the SkeletonState velocities are finite differences × 60
   (`sub_82BEBD28`, constant `0x822F860C` = 60; `+3216` position, `+3632` velocity, `+4048` Δv), so Δv is per update and
   assumes 1/60 s. Summary: `tools/recomp-trace/bail_impacts.py <trace>`.
   *First session (the user's `audiox_bail_20261003_094336`, 3 min, 0 malformed lines): no BAIL\* lines at all.* Cause:
   the hook accepted only entries inside the global table `*(0x83083C38) + 0x2F070`, but neither caller of
   `sub_827A1B78` passes a table entry. The per-skater loop `sub_827A11B0` (return address `0x827A13FC`) passes a
   stack array (its `r1 + 1064 − 152`, stride 544), and `sub_827A1030` (return `0x827A10A0`) a single zeroed stack entry
   with r6 = 1. So the "local entry" test never held, BAILLOCAL never logged and no pass matched the local X. Entry
   `+152` bit 31 is the r6 argument (in the loop: the skater's controller test). Fixed in the recomp (uncommitted): the
   hook accepts only the loop's call (`lr == 0x827A13FC`) with bit 31 set, and a bail is active when the entry's bits
   or `+676` / `+677` of a GREC-local audio state are set. A rate-limited `BAILCAND` line (7 fields) shows the lookup's
   raw inputs (first 32 calls; each new X of the output pass), so a failed lookup is visible in the next trace.
   *What the session shows without the BAIL\* lines* (local script `bail_session_summary.py`):
   - **Two GREC owners pass the local test** in this session (board objects `40C33020` from the start, `40C34020` from
     22.9 s), each once per frame. FIRSTHIT kept one set of last values, so the +676 / +677 bytes "alternated" 1 / 0
     every call: one owner was bailing and the other was not. `40C34020` bailed once (154.7 s) with B+16 still 0, so it
     is not the local skater. `40C33020` is. FIRSTHIT now keeps its last values per state. ~~Earlier sessions had one
     owner.~~ (Wrong: 15 of 18 sessions had the second owner, an NPC skater; the hooks now gate on local72 — see "The
     per-player recomp hooks logged an NPC skater too" below.) GREC / GRECX / TREAT / SEAMPAT readers should filter by owner (GREC's published local state alternates
     too, so the TREAT / SEAMPAT local-mask bit 2 is unreliable when a second owner exists).
   - **10 local bails**, 3.6–7.9 s from +676 rising to clearing. +677 (end) rises 1.4–4.0 s in and clears ≈ 1.0 s before
     +676 clears.
   - **B+16 rises in the same frame as +676** in all 10 bails, and clears 33–34 ms before +677 clears. So the first-hit
     byte is "bail in progress" for the whole bail, not a single first contact. Its rising edge is the bail start.
   - **B+24 is not 0.0000.** It resets to 0 at the bail start and then rises monotonically in steps about 17 ms apart
     (8–41 steps per bail) to a final value of 0.42–1.0 (clamped at 1.0 in 3 bails). It reaches that value 0.5–4.0 s in
     and holds it until the next bail. It is an accumulated bail strength, not a per-hit value.
   - **Body posts per bail** (SPLC, window bail start −0.5 s … +676 clear; ids by tier as in the table above):

     | bail (s) | posts | tier 0 / 1 / 2 | 1030 / 1031 / 1032 | 951 | 958 / 1047 / 991 | B+24 final |
     |---|---|---|---|---|---|---|
     | 39.8 | 149 | 106 / 27 / 16 | 2 / 7 / 4 | 3 | 4 / 2 / 0 | 0.68 |
     | 81.2 | 67 | 55 / 8 / 4 | 1 / 0 / 2 | 1 | 8 / 1 / 0 | 0.42 |
     | 91.9 | 49 | 35 / 9 / 5 | 0 / 2 / 2 | 0 | 2 / 3 / 1 | 0.56 |
     | 101.8 | 108 | 74 / 17 / 17 | 5 / 6 / 4 | 1 | 11 / 3 / 1 | 1.00 |
     | 121.1 | 45 | 29 / 10 / 6 | 1 / 3 / 0 | 2 | 0 / 0 / 0 | 1.00 |
     | 136.0 | 61 | 38 / 13 / 10 | 1 / 3 / 2 | 0 | 3 / 4 / 4 | 0.96 |
     | 144.4 | 67 | 46 / 13 / 8 | 0 / 6 / 1 | 1 | 2 / 0 / 0 | 0.64 |
     | 157.4 | 46 | 32 / 9 / 5 | 0 / 2 / 2 | 1 | 0 / 0 / 0 | 0.55 |
     | 168.7 | 43 | 27 / 7 / 9 | 6 / 0 / 2 | 0 | 1 / 4 / 1 | 0.82 |
     | 184.2 | 79 | 52 / 16 / 11 | 2 / 4 / 2 | 1 | 3 / 6 / 2 | 1.00 |

     Mean per bail: 71 posts; 1030 / 1031 / 1032 = 1.8 / 3.3 / 2.1 (all 74 posts of those ids fall in the 10 bails);
     head 952 / 953 = 2.4 / 3.4, torso 949 / 950 / 951 = 2.1 / 2.1 / 1.0, legs 947 / 948 = 7.1 / 4.3, arms 956 / 957 =
     8.8 / 0.8, skin 954 10.8, denim 955 14.8, concrete 958 3.4. **Concrete tier 1 / 2 (1047 / 991) do occur here: 2.3 /
     0.9 per bail**, unlike the nine earlier sessions, so some impacts against that surface went above 1.0 / 1.85. Since
     the region impacts are clamped to [0, 1], those posts come from a different impact value than the clamped region
     impact. Open: which (e.g. the deck or another surface's bands).
   *Measured with the fixed hook (2026-10-03, scripted background runs, 0 malformed lines).* Script
   `bail_ledge.txt`: the Super-Ultra Mega-Park spawn → off the board → a session marker → per attempt
   go to the marker, run off the drop and use the bail input. Runs `bailrun_t2_100040` (2 bails), `bailrun_ok_100226` (4 full
   bails), `bailrun_ok2_100514` (6 bails, cut ≈ 2.2 s in by the get-up press; the script now waits 6 s) and
   `bailrun_ok3_101354` (5 local bails of 6 attempts with the COLLPOST build; one window was an NPC skater's bail and is
   flagged by `bail_impacts.py`). No bail had an update > 20 ms, so no recomp hitch fell inside a bail. At spawn
   BAILLOCAL logs once per load with `how` 1 and entry index 0 (one skater in the loop). Summary
   `bail_impacts.py <trace>`:
   - **The impact formula is confirmed line by line.** In 4763 / 4763 BAILREG lines, the impact the game wrote equals
     clamp01(max(0.001, |Δv·n| × mass × 10)) within 0.002, with Δv = SS `+4048`, n = SC `+1008`, mass = SS `+4560` and
     cfg `+164` = 10.
   - **Update interval.** The ragdoll step runs at 60 Hz in the recomp too: step p50 16.6 ms (p10–p90 15.9–17.4) and
     the SS `+5220` dt is always 0.01667. So retail's per-step Δv and ours (1/60 s) compare directly, despite the
     recomp's ~345 fps render.
   - **|Δv·n| per step, the recomp vs ours.** The recomp, all bails of the ok runs, per region: p90 0.01–1.7 m/s, **p99 1.8–13
     m/s, max 7.6–20 m/s** (e.g. ok run: regions 3 / 5 / 6 / 7 p99 5.6 / 6.4 / 4.2 / 3.9, max 17 / 9.9 / 14.8 / 20).
     Ours peaks near 2 m/s. The tier-2 needs (≈ 1.7 m/s torso, 4–5 m/s head / legs at our masses) are reached
     several times per bail in the recomp. The largest steps are contact stops: in the top 10 % per region, the
     into-surface v_old·n (−2 … −20 m/s) mostly goes to near 0 or reverses in one step ("stop" share 25–100 %). So the
     gap is the physics: retail's parts hit the ground fast and stop in one step, while ours land and slide.
   - **Masses (SS `+4560`, the recomp)** by part: 1 0.0052, 3 / 7 0.00034, 4 / 8 0.0017, 5 0.0031, 9 0.0031, 6 / 10 0.0015,
     13 0.0058, 15 / 19 0.00031, 16 / 20 0.0010, 17 / 21 0.0051, 18 / 22 0.0067, 23 0.0098. They are smaller than
     the ≈ 0.003–0.015 bone-box estimate above, so they are worth checking against ours (`part_weights`) too.
   - The bail / end bits of the stack entry match +676 / +677 (BAILSTEP fields). With the GREC-state gate, a bail of the
     second owner `40C34020` also opens the window for the local X (one 0.6 s stretch with bits 0 / 0 in run ok2). Filter
     those passes by the bits.
   - Posts per bail in these runs (higher drop; window bail start −0.5 s … +676 clear): ok run 84 / 399 / 324 / 144,
     tier 2 = 7 / 42 / 30 / 10 (legs 1031 4 / 17 / 20 / 8 lead). Every full bail has tier-2 posts.
2. Then a physics-side investigation of the ragdoll's contact Δv (outside the audio port).
3. Frame-rate dependence (separate from the gap): `Contacts::body` decrements its 15-frame cooldown and the 4-frame
   region max per rendered frame. At 60 fps that is 0.25 s / 67 ms; on the console it is 0.5 s / 133 ms. Under the
   console-cadence rule both should run on the 30 fps cadence. That change would lower our post count further (up
   to 2× at 60 fps). Not done here: it is not the asked gap, and it changes the output.

### Session-review leftovers: the lift level, the grind-off lag, world-bank prefetch, the first-hit gate (2026-10-03, overnight)

Four open items from "Session-review fixes". "The recomp" = skate3recomp run locally: its program logic is evidence,
but its frame rate and stalls are not retail. Recomp figures come from the nine clean sessions 163809 … 223613, with
stall windows excluded. Tools: local scripts.

**1. The push foot's lift, "+2.7 dB" (0.213 vs 0.155): no mechanism difference, so no code change.**
- *The level path is the same.* `sub_824BB8D8` (plant) and `sub_824BBA00` (lift) are the same function apart from
  their slots (`+84` / `+80` and `+92` / `+88`). Both:
  - post `sk8_foley` with the id of wheel 0's material kind;
  - start with the block [0, 1, 0, dt, 1, 1];
  - get their level from the one updater `sub_824BF268`: level(6) × 1/32767, pitch(1), raw(0), dt, 0, 1.
- Contacts level(6) is MixMap E27 (`-600 + B[Player.2] + B[Player.0] + C[Player.8]`). It has no plant- or lift-specific
  duck: F9, the duck on Contacts.in6, feeds out3, not out6. So a lift plays at the plant's level(6). The only per-sound
  differences are the splice records: lift id 85 is record 31 (gains up to 2.83); 89 is record 35 (members 0.35–0.40).
- *The gap came from the measurement.* The review's figure took the first `sk8_foley` voice within 60 ms of each push
  edge, from any record (the clothing foley plays there too). It also pooled the sessions' different material mixes.
  `left/plantlift_ids.py` matches every post to the voices of its own record's samples instead:

  | id (kind) | the recomp: posts, level p50 / p90 | ours (`fixes/e2e_feet`): posts, level p50 / p90 |
  |---|---|---|
  | plant 84 (0) | 472, 0.176 / 0.491 | 147, 0.196 / 0.479 |
  | plant 88 (1) | 123, 0.247 / 0.303 | 18, 0.235 / 0.283 |
  | plant 92 (2) | 44, 0.221 / 0.342 | 0 |
  | lift 85 (0) | 473, **0.201** / 0.491 | 111, **0.204** / 0.494 |
  | lift 89 (1) | 121, 0.068 / 0.082 | 12, 0.080 / 0.083 |
  | lift 93 (2) | 46, 0.160 / 0.205 | 0 |

- Per id, ours and the recomp agree within about 1 dB (−0.1 dB for lift 85). Pooled, the recomp's lift p50 is 0.169
  against our 0.193, because its sessions have more kind-1 surfaces: 19 % of lifts against our 10 %, and kind 1's lift
  record is quiet. The 0.155 also included other voices.
- `+338` doesn't enter the level. It only splits `+333` / `+334`, and the trigger reads their OR.

**2. Grind off "first audible +50 ms": an authored delay plus a measurement artefact, so no code change.**
- *Post timing is the same.* Ours posts the off sound (`sub_824C4138`) at the release, on the frame the grind ends.
  The recomp posts it 0 ms after the grind end too (`fixes/evpost.py`).
- *The updater is the same.* `sub_824C42A8`, read again: per slot, the on and off sound get gain = their level ×
  level(1)/32767, pitch = their pitch × pitch(2)/4096, raw(0), dt, 0, 1, and the env send level(5); a sound that
  ended is released. `Grind::update_sounds` matches this word for word. It runs in the same frame's update, so the
  console cadence plays no part.
- *The delay is in the data.* The off records' members carry authored delays. For example, Skate_Collisions record 835
  has group 2 at 11 ms (+ up to 25 ms random) and group 1 at 50 ms (+ up to 50 ms). The splice player honours them.
- The recomp's PLAY lines after each off post (`left/grind_off_delay.py`, sample-matched) put the first audible member
  at p10 / p50 / p90 = 4 / 14 / 47 ms. Its delayed groups play late as authored: d57 → 64–72 ms, d64 → 80–101 ms,
  d162 → 176–190 ms.
- Ours: the first member comes 0–1 frames (0–17 ms) after the post in all 7 grinds, and the 50 ms groups come 3–6
  frames later.
- `fixes/grind_cmp.py` took the first run in its list, which is in completion order, not the earliest. Fixed to take
  the earliest: the off lag p50 is 0 ms (7 / 7).

**3. World-source banks through the prefetch** (`game_audio/world_sources.rs`, `native/prefetch.rs`).
- *Problem.* The traffic and ped banks (`TRAFFIC_BANKS`, `PED_BANKS`; 10 of 13 in this install) were read and decoded
  on the game thread in `ensure_bank` when the first vehicle or ped was published. That is one frame with 10 bank
  decodes.
- *Change.*
  - `WorldOwners::expected` (new): the living-world system sets it as soon as it knows it will publish owners on this
    map.
  - While it is set, `prefetch_world_banks` queues every world bank that is neither loaded nor requested on the
    prefetch worker. When it clears, the requested banks that no owner loaded are dropped.
  - `Prefetch` counts its clears (`clears()`). A map change (`unload_map_banks`) clears the prefetch and unloads the
    world banks, so the host asks again. `contains` is no longer test-only.
- *Identity.* The rules are the same as for the emitter prefetch:
  - `ensure_bank` still runs `load_bank` at the first owner, with the same calls in the same order;
  - only reading and decoding move;
  - a queued, failed or missing bank loads on the game thread as before, with the same errors.
- *In play the host stays inert.* No system sets `expected` or publishes owners, so `frame` returns at its first line,
  as before. The e2e harness doesn't run world sources.
- Off: `SKATE_AEMS_WORLD_PREFETCH=0` (game-thread loads at the first owner, as before). `SKATE_AEMS_WORLD=0` turns off
  both.
- *Test* (`world_sources::tests::world_banks_are_prefetched_when_expected_and_load_without_decoding`, data-gated):
  - nothing is requested until the world is expected;
  - then every installed world bank is requested once (missing ones are listed as unavailable);
  - the worker's bank and PCM equal `BankSource::load` bit for bit;
  - the requests leave the evaluator RNG and the block count unchanged;
  - `ensure_bank` for all of them decodes 0 WAVs on the calling thread;
  - loaded banks are not requested again;
  - `unload_map_banks` makes the host request them again;
  - "not expected" drops them;
  - without a prefetch the bank decodes on the game thread as before.
  - The prefetch fallback test also checks the clear counter.

**4. The first-hit torso message's gate (`sub_824BCEB0`): read further, but the writer is still not found.**
- `sub_824BC188` sets Contacts input 7 to 0 on every frame. With the byte B+16 set, it also sets Contacts input 8 to
  clamp(B+24 × 32767 (`0x821747FC`), 0, 32767), else 0. Here B = `*(*(0x83083C38) + 0x2FCB4)` (`lis -31992` =
  0x8308; an earlier version of this note wrote 0x8208).
- The object's `+420` latches B+16's rising edge (`+421` = last frame's value). `sub_824BCEB0` then:
  - pulses Contacts input 7 = 32767;
  - for a region (loop index `r17`) other than 1 and a region tier (`r20`) ≥ 1, posts the torso material 98
    against no material (143) through `sub_82496C58` (level) and `sub_82486EF0`;
  - clears `+420`.
- Contacts.in7 triggers F7 in Player (+400 mB, hold 14, release 29). That feeds the **Collision** slot's C0: the bail's
  collision layer is 4 dB louder for about half a second after the first hit. Contacts.in8 has no reader in the MixMap.
- B+16 is the same flag that PlayerPhysics.in11 (`sub_824B19C8`) and the `hall_of_meat_slo_mo` companion's w11
  (`sub_824DD6F0`, B+24 × 1000) read. `sub_824C0AB8` and `sub_824BFA48` (Contacts state) read it too.
- B+16 is a large sub-object (≥ 12.5 KB), reset by `sub_827AB6E0` from `sub_8279E800`. The `sub_827AE9C8` /
  `sub_827AEF08` / `sub_827AF4E0` / `sub_827AF748` / `sub_827F1DD0` family copies it into locals.
- No direct store to B+16 byte 0 or B+24 was found in the 15 functions that load B, or in the same class's methods
  (`sub_827A*` / `sub_827B*` `stb …,0(r3)` / `stfs …,8(r3)`).
- Next: a recomp hook that logs B+16 (byte) and B+24 (float) per frame during bails and Hall of Meat runs. A
  write-watch on that address would name the writer.
- *Hook added (2026-10-03):* `FIRSTHIT` in the recomp's research hooks (category `audiox`, sampled from the board
  update hook, local player): B, the B+16 byte, the B+24 float and the audio state's bail / end bytes (`+676` / `+677`),
  logged when any of them changes plus once a second. It gives the timing and strength but not the writer (the hook
  framework has no write-watch). A short bail session (4–5 different bails in the same area) is queued. Until then the first-hit message and the Contacts.in7 pulse stay
  unported (our Contacts.in7 / in8 stay 0, as with B+16 clear).
- *Measured 2026-10-03* (the user's session 094336 and the scripted bail runs; details under "Bail density … Open 1"):
  - B+16 rises in the same frame as the audio state's +676 at every bail start. It clears 33–34 ms before +677 clears,
    so it means "bail in progress".
  - B+24 resets to 0 at the bail start and accumulates in ~17 ms steps to 0.42–1.0 (clamped).
  - The first-hit torso post (COLLPOST from `sub_824BCEB0`, return `0x824BCFC8`) fires **once per bail**: torso 98
    against 143, tiers 1 / 1, 0.16–0.28 s after the bail start (1.77 s once). It happens for an NPC skater's bail too
    (local72 0).
  - Port: on the rising edge of our bail flag, latch; at the first region contact with region ≠ 1 and tier ≥ 1, post
    98 / 143 once and pulse Contacts.in7. In8 = B+24 × 32767 needs the accumulated strength, whose writer is still
    unknown.

**Files.**
- `crates/skate-game/src/game_audio/world_sources.rs`, `game_audio/native/prefetch.rs`.
- Tools: local scripts `splc_records.py`, `plantlift_ids.py`, `grind_off_delay.py`; `fixes/grind_cmp.py` (earliest
  voice).

**Verification.**
- skate-audio tests;
- `cargo test -p skate-game --release --bin skate3rust --locked -- game_audio::`;
- `--no-run`.

Results: skate-audio 228 pass; game_audio 58 pass (new world test included, run on the install's data); `--no-run`
builds. e2e doesn't run the changed code: `world_sources` is not in the harness, and
`prefetch.rs` only gained a counter and a public `contains`.

## Credits

The retail measurements in this document were taken with **[skate3recomp](https://github.com/mchughalex/skate3recomp)**
by @mchughalex, a native static recompilation of Skate 3. It is built on the
[rexglue SDK](https://github.com/rexglue/rexglue-sdk) and credits [Xenia](https://github.com/xenia-project/xenia)'s
research. We ran it locally with trace hooks and scripted teleports; it has no licence, so nothing from it is
included here. Upstream PRs #4 / #1 by @andrewnakas (research notes) were also a key reference.
Setup decodes the disc's EA-XMA audio with [vgmstream](https://github.com/vgmstream/vgmstream) (pinned
r2117, hash-checked).

Research for the native audio runtime (specs in progress, 2026-10-02) also draws on:
- [dbalatoni13/nfsmw](https://github.com/dbalatoni13/nfsmw) (CC0): a matching decompilation of Need for
  Speed: Most Wanted's EA audio library. Official AEMS opcode and bank-header names and state layouts.
- [BurnoutDecomp/BP-Decomp_Workflow](https://github.com/BurnoutDecomp/BP-Decomp_Workflow) and
  [BurnoutDecomp/b5-decomp](https://github.com/BurnoutDecomp/b5-decomp): RenderWare Audio plug-in
  identities, the SndPlayer1 and filter/resampler details, opcode names. b5-decomp has no licence, so it
  is reference only.
- XNA Math / [DirectXMath](https://github.com/microsoft/DirectXMath) (Microsoft, MIT License): the
  `XMVectorSin` / `XMVectorCos` polynomials the FrequencyShiftSsb oscillator uses (`skate-audio-fma`); the
  coefficients are read from the shipped image.
- [mitsevox/tw2004](https://github.com/mitsevox/tw2004) (CC0): Tiger Woods 07 RenderWare Audio function
  listings.
No code from these is included.

The native runtime (`crates/skate-audio`) is our own code written from our specs. Its oracle was the PoC
worktree of upstream PR #4 by @andrewnakas (no licence; run locally as a black box: our probes drive its
evaluator and replay-verified DSP kernels; two local fixes to it, never committed). Its facts (offsets,
opcode numbers, constants) come from our reading of skate3recomp (no licence, reference only) and the disc
banks; official names from dbalatoni13/nfsmw (CC0).

The MixMap port and the granular rolling bed (2026-10-02) rest on upstream PR #4 / #1 by @andrewnakas
(no licence, reference only): they found that the bed is a granular player and recovered the MixMap's
function map, stage order and input writers, the GrainPlayer object, pick and scheduler, the board
records, the bus chains, the vault field roles and the instance counts. Their MixMap port and grain
functions, run locally in the PoC worktree, are our golden oracle (MixMap 87,000 cells, pick / position
vectors); the order of retail's single-precision operations in the Bézier, the speed scales and the
`id 9 = 0 for the local player` rule were read from their documentation. EA's record names (MixCtl,
3DMixCtl, EvtMixCtl, SubMixCh, MasterMixCh, Preset) come from
[BurnoutDecomp/b5-decomp](https://github.com/BurnoutDecomp/b5-decomp)'s NFSMix headers (no licence,
reference only). Spot checks and addresses: skate3recomp (no licence, reference only). Data: the user's
own Skate 3 disc and TU3 image (`MixMapSK8.mxb`, `grains.big`, `skatercollections.vlt`; never
committed), decoded with [vgmstream](https://github.com/vgmstream/vgmstream). No code from any of these
is included.

The player inputs and components (2026-10-02) are our own code from our reading of the retail functions
in the skate3recomp generated code (no licence, reference only; addresses in the source docs) and the
user's vault. Upstream PR #4's `player-audio-retail-drivers.md` and `mixmap/inputs.rs` notes by
@andrewnakas (no licence, reference only) gave the function map, the packet word tables, the Jitter /
Contacts / 3DObjPos writer descriptions and the retail wind and grain capture tables we validate
against. The retail level tables come from our local recomp trace sessions
(recomp session `20261001_211347` — deleted 2026-10-02, re-verify on clean data — and
`all_20261002_163809`). No code from any of these is included.

The Splice player, the board contacts, skid and squeaks (2026-10-02) are our own code from our reading of the retail
functions in the skate3recomp generated code (no licence, reference only; addresses in the docs) and the user's vault
(`skatercollections.vlt`). Upstream PR #4's driver notes by @andrewnakas (no licence, reference only) gave the
Contacts / skid / squeak function map and word tables we checked against the code; their PoC, run locally as a black
box through our own headless probe, is the end-to-end oracle. No code from either is included.

The collision manager, the wheel spin, the foot taps / scuffs (2026-10-02) are our own code from our reading of the
retail functions in the skate3recomp generated code (no licence, reference only; addresses in the module docs) and
the user's vault (field layouts from the disc's own schema, `skaterschema.vlt`). The bed validation data (GREC hook)
was recorded with skate3recomp by @mchughalex, run locally by the user. Upstream PR #4's driver notes and
`collision_states.rs` docs by @andrewnakas (no licence, reference only) gave the manager's map (router, ten slots,
message layout, the material table at `0x8302D6E8`) that we re-read and corrected in the code (the update reads the
category output's level; materials ≥ 143 take pitch output 1; the impact bands come from the `+24` RefSpec). No code from
either is included.

The environment network, the eEQChain buses, the owner one-shot buses and Class_Seams (2026-10-02) are our own code
from our reading of the retail functions in the skate3recomp generated code (no licence, reference only; addresses in
the specs `audio-specs/aems-env-bus-spec.md`, `audio-specs/aems-eqchain-buses-spec.md` and the module docs), the
TU3 image and the user's vault; validated against the user's own recomp traces (skate3recomp by @mchughalex, run
locally). BurnoutDecomp/b5-decomp's reconstructed ReverbModel1 and Delay (no licence) were read only to orient; the
DSP follows the TU3 asm. Upstream PR #4's notes by @andrewnakas (no licence, reference only) named the eEQChain
holder fields, the owner bus builder and the Class_Seams function map. No code from any of these is included.

## Open questions

- World sound sources (2026-10-03): the speech manager's `.evt` rules, take choice and level; retail's SFX instance
  assignment; the traffic 3DObjPos binding; ped model → shoe class / voice; PedBodyFall, Tazer (op 38), NPC skaters' grain bed / wheels / foley (foundation in "NPC skaters' board sounds"),
  event PA / crowds / music zones ("World sound sources").
- Main bail body-impact sound (Bodyslide is only a layer); landing weight layer level; metal grind pick
  (54–56) by ear; powerslide, wheel spin, push sound, fountain bank, caveman jump-on — not yet confirmed.
- Surface → grain now follows retail (2026-10-02); still open: the `Class_rolling` surfaces (interim stand-ins), tag 90, and what selects the soft-wheel grains.
- Retail plays rolling granularly; the default path plays banded loops, `SKATE_AEMS=1` plays the native
  granular bed (player B silent until turn/slope inputs exist; FSS, graph 3, wobble not modelled).
- MixMap inputs: VU (output meter) and the menu/NIS/HOM/Challenge flags stay 0; the bed's gain A matches
  retail's level(1) p90 with Jitter wired, the lower retail medians need a straight-roll trace at turn 0
  (see "Player inputs and the first native player components").
- Native player components still to port: seams, rattle, Class_rolling, cloth, body slide, footsteps,
  flips/treatment, the body-impact and step-on posters (need skeleton region contacts / Skeleton+602/+603 from the
  engine). Native since 2026-10-02: skid, squeaks, the Contacts pops / landings / touchdowns, the collision manager
  (grind start, landing pair, deck impacts), foot taps / scuffs, the wheel spin. Music.in3 is a game-mode flag
  (0 in free skate), so the ollie voice and the landing impact stay gated as in retail.
- The turn input (`Turn` animation attribute) is almost binary in our engine (29 % of riding frames at 1.0 vs
  retail 16 %; 7 % in between vs 25 %): carving layers trigger too easily (user, 18:02 build). Animation side.
- The interim layer still bypasses the native host's 6→2 fold (~3–6 dB above native voices of the same per-voice
  level); the send buses (distance cue) are not modelled.
- User listening check 2026-10-02 (native build 16:59): rolling "too loud" and "not in sync", grinds "not the
  same sound" — see "Board contacts, skid and squeaks; the end-to-end PoC comparison" for what the measurements
  show; the PoC reference had a one-off louder/harsher seam sound on one launch (first-play decode).
- Zone ambience from `.ems` emitters (beds per map are a name-based guess); manual wheel spin
  (`Whls_spins_Man_1`); music.
- ~615 MB of PCM (mostly ambience): a compressed format would need an encoder at setup.
- Retail's own sample choices could be measured from a recompiled build that logs which bank data the
  game decodes (local research only, not part of this change).
- `AUDIO_*` logging is verbose for a release build.
- Native runtime: the unmodelled buses and sends (the MixMap's emitter send word w2 now has a value but no
  bus to go to), loop time-left, `c_emitter` w0, and the gain ramp's last ulp (see "Native AEMS runtime:
  implementation" and "MixMap and the granular rolling bed").
- Buses (2026-10-02): the FlangeSub effect returns (GRINDS routes to one), reverb-zone emitters (type 5), the grain
  chain's graph-2 env send, the Splice voices' own Send A (retail Skate_Collisions median 0.0146) and Collision SubMix,
  sense_of_speed's FXWET0 (retail median 0.015, ours 0), the reverb module's timer delay; SFX Master's DCl0 level.
- The riderless-board gate (states 300 / 500–502 report no wheels) rests on retail's measured record; the writer of
  record +152 bits 20–22 is not located.
- rodio 0.20's spatial panning is mirrored (the default path still uses it; native mode swaps the ears).
- 19:38 air tricks #1 / #3: the audible in-air voice is retail's wheel spin-down; a retail ollie at ~30 km/h would
  settle whether ours sounds like it.
- Port stage 2026-10-02 (rolling layers, chain, tricks, footsteps, returns), still open: the FlangeSub send of the grain chain
  (no bed → return route); the bool-class word `0x11A631878B239355`; the seam pattern retail sees on University's sidewalk
  cells (pattern 11, "Listening test 8"). The seam sample variety is fixed: it was the missing `Start_up_Play_ctl` boot
  utility ("Listening test 8"). Closed 2026-10-02 (stage "FootStep SubMix, the skeleton inputs, …"): the FootStep SubMix, the
  step code, limb / body speeds, ragdoll slide records, the loose-board vectors, Reverb.in0..6 + the env scale, the reverb
  zones (1,040 records, not one), the boot order.
- Treatments 16 / 17: settled, including the ramp air's late start (a 1 s audio stall in the recomp) and the short
  airs (same rate as the recomp). sense_of_speed figures redone with the shared-sample fix: no difference
  ("Leftovers: Treatments 16 / 17 timing, …").
- FootStep SubMix: Sen0 #2 = 1.0 and the Pn21 law 0 are read, not defaults. Left: a released voice's de-click tail
  skips the submix's filters (as for every bus).
- Reverb zones: vfunc92 is ported (preset ∈ the 24 presets). Not ported: the music zones (type 4, playlists for the
  licensed-music player we lack) and the speakers / crowds records (types 6 / 7, event PA and crowds; flags mask
  audio +1032, 4 by default). The value of audio +1032 in free skate is not measured.
- The Send A table in "The buses" is superseded for sense_of_speed (0, as retail) and the Splice voices (none):
  local script `senda_bank_fixed.py`.

### Native runtime is the default (2026-10-02)

(Superseded 2026-10-03: the tables and these opt-outs are removed; see "Interim cue tables retired; the water
splash is native".)

- The native AEMS runtime is now the default. `settings/audio.json` `"interim": true` or `SKATE_AEMS=0` selects
  the interim measured tables; any other `SKATE_AEMS` value, or none, selects native.
- The old `native` setting key is ignored. Installs saved `"native": false` while the tables were the default, and
  honouring it would keep those players on the tables.
- The tables stay in the code for now.
- Verification: `game_audio::` tests, including the new
  `native_is_the_default_even_with_an_old_saved_native_false`; muted smoke runs (`--mute`) on StartPark and University
  with the staged build: native runtime and every component on, render ready, no panics, errors or non-finite physics.

### Seam-hit prominence: closed (2026-10-03)

The remaining 2–5 dB gap between our seam hits and the recomp's capture (after the 110 ms alignment fix) has no
mechanism. Pitch, per-voice level, bus, voices per hit and the bed spectrum all match. The user closed it as a
likely recomp artefact: the recomp's frame rate is uncapped, its audio thread stalls, and its capture includes
ambience and the other seam voices. The user hears the seams as "WAY BETTER". No levels were changed.

### Interim cue tables retired; the water splash is native (2026-10-03, `player::footsteps::Splash`)

The user, after the listening check of build 04:20: "go ahead and remove the interim cues", then, on hearing some were
still in use: "oh.. if they are still being used then we aren't done." Decisions: splash "port it natively first";
the riding bed: remove it, then check it against the recomp; installs without the data: require it (no fallback).

**Listening (user, 2026-10-03, bin\ 10:06, session `state_20261003_101640`, 0 malformed):** "The sounds are SO GOOD.
I think that is basically retail, if there is a difference i couldn't tell. Splashes sounded correct. Riding sounded
right, jumping sounded right, falling sounded right. Cracks on bricks sounded correct at both higher speed and lower
speed. Manual sounded amazing."

**1. The water splash, from retail's mechanism (not the carried-over sound).** `SFXObj_OffBoard`'s process ends with
`sub_824EBB58`; its update with `sub_824EBE78`. Read from the TU3 recompilation (reference only):
- Inputs. The bridge `sub_824B0DA8` copies record `+172` bits 30 / 29 / 28 into audio state `+811` / `+812` / `+813`.
  The conditioner `sub_827A1B78` writes them:
  - `+811` in water = the current state's byte `+81`. In our engine that is Wipeout300's `special_surface_81`
    (its water contact), already published into `state_flags`.
  - `+812` under the surface = `+811` and the state's surface height `+32` (`surface_height_32`) above the Y of any
    of the PhysOut Skeleton points `+128` / `+112` / `+32`. Skeleton::FillPhysOut (`sub_82BE1AE8`) writes them: part
    15's and part 19's pose matrices applied to a per-part local point (Skeleton `+2560` / `+2816`, translation at
    `+48`), and part 1's pose translation (Skeleton `+8128`).
  - `+813` the board in water = Collision `+16` = 12. That is the board's surface vote `82C08818`, which forces 12
    when a board contact is water: `physics::board_surface`, the value respawn already reads.
- Poster (every frame, record active; no local or on-foot gate). On `+811`'s rise the time in water `+480` starts at
  0, then counts dt; on its fall the latches `+477` / `+478` clear. On `+812` once per stay (`+478`), unless the
  game global `+224` is set (0 in free skate), the entry sound `+484` is stopped and a new one starts. The ids are
  vault fields of the AudioSurface-class record `water` (`923CCB46EF5BF5BA` / `B2BD1F28DDE601B0`):
  - Skate_Collisions **1187** (`35D3B06292CDA10B`) on the water contact's first frame;
  - **1197** (`C17485220849574D`) once the time in water has reached 0.001 s (`9CD13431903E3719`), i.e. from the
    next frame on;
  - `+813` once per rise (`+492`): **1198** (`BA81E93AE985D1C7`) into `+488`.
- Sound path: the collision Splice object `sub_82497F48` into SFX Master, start block [0, 1, 0, 0, 1, 1]. The update
  `sub_82498140` sends [OffBoard level(13) (entry) / level(16) (board) / 32767, pitch(14) / 4096, raw(0) × 360/65535,
  dt, 0, 1] and the env send level(15) / 32767 (as the grind on / off sounds: the last update's env level is latched
  at each start). A sound that ended is released.
- Cross-check: 1197 is the container of **record 869**, which is exactly the sound the user found by ear (476–478,
  with 169 at 75 % and one of 171 / 172). 1187 → record 868 (170–172 + 169); 1198 → record 870 (473–475, the set the
  user had rejected for small body falls: retail plays it for the board). The 2026-10-01 measured retail splash
  (475 + 478 + 172 + 169) is 1197 + 1198.
- Port: `skate_audio::player::footsteps::Splash` (state `in_water` / `under_water` / `board_in_water`, set in
  `skate_events::audio_state`; the ids and threshold in `FootstepTuning`). Logged in game as
  `AUDIO_EVENT splash native Skate_Collisions:<id>`.
- **UNCERTAIN:** the engine does not publish the per-part local points at Skeleton `+2560` / `+2816`, so the toes'
  pose translations stand in for `+128` / `+112`. That can only shift the entry frame by the local offset (a few
  cm of depth).
- Through the real MixMap (test `the_water_splash_plays_through_the_real_mixmap`): OffBoard level 13 = 14585
  (≈ 0.45), 16 = 14401, env 15 = 862, pitch 14 = 4086.

**2. The riding bed removed, checked against the recomp.** Under native the random bed still played Skate_Collisions
947–968 and sk8_foley 62 / 73 / 74 / 84 / 85 / 88 / 89 as interim voices. All of that is gone (below). Riding = GREC
/ state-log context roll + carve, audible voices (> 0.002), first-voice levels
(local scripts `bed_vs_recomp.py`, `riding_posters.py`):

| per riding second | the recomp (6 sessions, 682 s) | ours native (5 user sessions, 229 s) | the removed bed (expected, same frames) | ours before (native + bed) |
|---|---|---|---|---|
| Skate_Collisions voices | 8.95 | 2.80 | 5.78 | 8.58 |
| Skate_Collisions Σg² | 0.424 | 0.218 | 0.120 | 0.338 |
| sk8_foley voices | 5.93 | 2.54 | 1.90 | 4.44 |
| sk8_foley Σg² | 0.229 | 0.103 | 0.079 | 0.182 |

(Recomp: all_20261002_180430, 223306, 223613, 214346, 214002, 222155. Ours: the e2e renders of 213757, 214224,
215843, 224747 (row) and 084712 (`E2E_FPS=60`). The native renders are byte-identical before and after this change.)

So without the bed ours is −2.9 dB (Skate_Collisions) and −3.5 dB (sk8_foley) below the recomp while riding. By
poster (the recomp, riding):
- **Skate_Collisions:** the collision manager (`sub_824D1F68` ← `sub_824D2318`) plays 3.3 posts/s while riding.
  Most are the body materials **955 / 954 / 956 / 947** (denim / skin / arm / leg, 1.7/s, p50 ≈ 0.1); the rest are
  969 / 958 / 968 / 1038 (≈ 0.6/s, mostly quiet). Ours: the body poster (`sub_824BC188`, ported) runs in every state,
  but our skeleton publishes almost no body-region contacts while riding (4 of 1,770 riding rows in 084712, impacts ≤
  0.011). The manager is shared, though: NPC skaters' Contacts and physics props (`sub_824E3BE0`) post into it too,
  and the session review already put 969 / 958 / 968 down to world objects. The SPLC caller chain stops at the
  manager's update, and no hook records the messages (`sub_82486EF0`: materials, tiers, levels, poster). So whether
  the local rider's regions touch something while riding (and what) is **not established**. Not changed (no
  guessed weighting).
  *Measured 2026-10-03 (recomp hook `COLLPOST` on `sub_82486EF0`, category `audiox`; run `riderun_20261003_101135`,
  scripted: three ~10 s pushing segments from a session marker at the Super-Ultra Mega-Park spawn, straight / carving;
  0 malformed lines; summary `tools/recomp-trace/collision_posts.py`).* Each line has the posting
  function (from the return address), its object, the object's audio state `[object+32]`, the GREC owner of that state,
  `[[object+28]+72]` (local72), the materials, the tiers and the caller chain. Findings:
  - **Clean pushing and carving on open ground: no body posts from the local rider.** Segment 1 (straight, 25.6–35.6
    s) had none. Every local body-poster post (31, local72 = 1, owner `40C33020`) came in bursts at 52.7 s and
    70.5–74.8 s, when the rider brushed and hit a rock wall at the edge of the park (screenshots). They paired legs 99
    (tiers 0–2) against materials 2 / 65 / 143 with the denim cloth 108 / 143.
  - **The other owner is an NPC skater.** The second GREC owner `40C34020` (doc above) posts with local72 = 0 through its
    own Contacts object. Its bail at ≈ 38 s produced the run's body burst (39 posts, 34 body SPLC 956 / 954 / 955 /
    947 at 38.2–38.4 s) while the local rider rode normally. So `[[object+28]+72]` tells the local rider from other
    skaters, where GREC's `[[owner+16]+72]` does not.
  - Deck impacts (`sub_824BD000`, deck 95 against the ground): local 1.28/s, the NPC 0.34/s.
  - The 48-byte message's +32 / +36 words are not SPLC ids: mostly 0, otherwise 4-hex-digit values, e.g. 0x6590. The
    sounds come later from the manager.
  - Conclusion: while riding, retail's body-material sounds come from other skaters' bails and from the rider touching
    walls, not from riding contacts. Ours having ~none on open ground matches. The 1.7/s in the user's riding
    sessions likely include NPC skaters (no COLLPOST in those sessions; the session review's PCU / park sessions had
    AI skaters). To check in a user session: `PLAY_TRACE_AUDIOX.bat` now logs COLLPOST.
- **sk8_foley:** every poster is ported. The rates differ with riding style. The recomp's riding has 0.42 plant /
  lift posts per second; ours 0.19 / 0.16 (the user: "the 19 plants are correct, i didn't ride fast"). Push-driven
  sounds scale with that: the Clothing push foley 74 / 73 (ours 0.16/s, the recomp 0.79/s) and the on-board
  footsteps. Shoe scuffs 94 / 95: ours 1.61/s, the recomp 2.71/s (both p50 0.061). The pedestrians' sk8_foley 62 / 63
  (0.53/s in the recomp, `SFXObj_PedestrianSFX`) are a world layer we can't have yet. No mechanism gap found; not
  changed.
- **Needed to settle the Skate_Collisions part:** a recomp hook on the collision message post `sub_82486EF0`
  (caller chain, the owner's local flag, materials A / B, tiers, levels), plus a short riding session by the user
  (flat ground, some pushing and carving, no bails). Then compare the local rider's body-region messages with our
  region contacts (`rimp*` / `rtag*` in the state log).

**3. Native data required.** No fallback any more:
- no AEMS banks → the runtime does not start and an error says that the skater's sounds, the world emitters and
  rolling are silent;
- no MixMap → error, no player sounds, no rolling;
- no grain recordings / tuning → error, rolling silent;
- missing player banks or Splice trees → error, those sounds silent.

The zone beds, location sets and crossfades still play their measured layers through Bevy voices.

**4. The interim path removed.**
- `cues.rs` deleted. `grain_for` moved to `grain_bed.rs` (with its test); `RETAIL_SCALE` moved to `voices.rs`
  (ambience and the Bevy-voice scale still use it).
- `skate_events.rs`: the `Event` enum, the event detectors (pop, landing, flip, bail, push, step, splash, foot
  strikes), the `play` system and its loops (rolling bands, bed, grinds, powerslide, foot drag, wheel spin) are
  gone. `observe` keeps the audio state, `Riding` (minus the fields only `play` read), the state log and the
  `AUDIO_EVENT` brake / push / grind lines.
- `physics::foot_clearance` removed (only the interim steps used it).
- Opt-outs removed: `SKATE_AEMS=0`, `SKATE_AEMS_PLAYER=0`, `SKATE_AEMS_FOOTSTEPS=0`. `"interim"` / `"native"` keys in
  `settings/audio.json` still load and are ignored (test `old_settings_files_with_interim_or_native_keys_still_load`).
  The other `SKATE_AEMS_*` switches (they pick between native variants) stay.
- `library.rs`: the grain speed bands, the wheel-spin clips, cue preloading and the patch trees (`Patches` /
  `Group`) are gone. Old manifests still load: unknown keys are ignored.
- `voices.rs`: the fold / scale stay, because the measured world layers still play through Bevy voices.
  `Play::effect` is test-only now.
- Gone with the tables: the `AUDIO_CUE` / `AUDIO_LOOP` lines and `AUDIO_EVENT land` / `rolling` (interim-only).

**Verification.**
- `cargo test -p skate-audio --locked`: all pass, including `the_splash_plays_once_per_water_stay_by_its_time_in_water`.
- `game_audio::` (release): 54 pass, 4 ignored (60 before; the 8 interim tests went, 2 added).
- `cargo build --locked` (dev): OK. The 3 warnings are items that were already test-only at 45c6e65.
- e2e byte-identical to the 45c6e65 renders (`opt/runs/fma_plain3`): 13 scenarios + 4 real sessions, row and
  `E2E_FPS=300` (`opt/runs/retire1`), and 084712 at `E2E_FPS=60` against `check0847`. The e2e path never ran the
  interim layer, and the splash inputs are false there.
- Not run here: the muted 5-map smoke test (it opens a window) and bin\ staging.

**For the user's check in game:**
- a fall into deep water: one 1197 splash per entry, plus 1198 when the board lands in the water;
- riding on flat ground without the random bed (Skate_Collisions / sk8_foley pieces), against the recomp.

Rerun: `bed_vs_recomp.py <recomp sessions> <DIR:NAME,…>`,
`riding_posters.py <recomp sessions>` (local scripts; PYTHONIOENCODING=utf-8).

### The ragdoll part masses against the recomp; the missing |Δv·n| (2026-10-03, analysis only; no code change)

Open item 1 of "Bail density" listed the recomp's SkeletonState `+4560` masses rounded to two digits and asked for a
check against ours. Ours = `SkeletonAnimationMasses::part_weights` = the raw bone-box product x·y·z of each
`PhysicsParamBoneData` size (`assets/private/stock/physics-skeletons.json`, f32 as the code multiplies). The recomp =
the `mass` field of every BAILREG line of the fixed-hook runs t2 / ok / ok2 / ok3 (12,054 lines, 0 malformed), printed
to six decimals:

| part | ours | the recomp | | part | ours | the recomp |
|---|---|---|---|---|---|---|
| 1 neck1 (head) | 0.005248 | 0.005248 | | 13 spine1 | 0.005832 | 0.005832 |
| 2 neck | 0.000784 | 0.000784 | | 15 / 19 toes | 0.000307 / 0.000307 | 0.000307 / 0.000307 |
| 3 / 7 hands | 0.000336 / 0.000336 | 0.000336 / 0.000336 | | 16 / 20 feet | 0.001015 / 0.001016 | 0.001015 / 0.001016 |
| 4 / 8 forearms | 0.001666 / 0.001667 | 0.001666 / 0.001667 | | 17 / 21 legs | 0.005109 / 0.005110 | 0.005109 / 0.005110 |
| 5 / 9 arms | 0.003134 / 0.003050 | 0.003134 / 0.003050 | | 18 / 22 uplegs | 0.006708 / 0.006708 | 0.006708 / 0.006708 |
| 6 / 10 shoulders | 0.001483 / 0.001483 | 0.001483 / 0.001483 | | 23 hips | 0.009753 | 0.009753 |

**The masses are identical** for all 20 parts the recomp's bails touched (the left / right arm asymmetry 0.003134 /
0.003050 included). The 2-digit list in "Bail density" was rounding, and the earlier "≈ 0.003 arm … 0.015 torso"
estimate was a guess. So the masses are not part of the bail-density gap. (Parts 11 / 12 / 14, the upper spine, never
touched in those runs: ours 0.007751 / 0.007001 / 0.005935.)

**Found on the way: retail takes the absolute value of Δv·n; ours doesn't.** `sub_82BD60C8`'s region loop computes
`vmsum3fp128 v44 = Δv · n`, then `vandc128 v42 = v44 & ~(v63 << 31)` (the sign mask): |Δv·n|. The recomp lines agree:
with Δv·n = v_new·n − v_old·n from each line, clamp01(max(0.001, |Δv·n| × mass × 10)) matches the written impact in
12,054 / 12,054 lines (within 0.002), the signed form in 11,776. `skate_events::region_impacts` uses the signed
`along`, so a step with Δv·n < 0 is floored to 0.001 there.
- How often: 5,355 of the recomp's 12,054 region steps have Δv·n < 0. Of the 1,729 above the floor, 447 (26 %) are
  negative ones; above 0.1 it is 8 of 166, above 0.25 4 of 46.
- What it does to ours: about a quarter of the above-floor region contacts read 0.001, under every body floor (0.002
  head / torso / legs, 0.005 arms) and the concrete floor (0.12). Those contacts post nothing. It barely touches tier 1
  / 2 (5 % of the impacts above 0.1). So it costs mostly tier-0 posts: a small part of the 4–5× gap, which stays the
  ragdoll's small per-step Δv (open items 1–2).
- Not changed (this task was report-only). The fix is audio-side, not physics: `along.abs()` in `region_impacts`
  (the doc's "matches term by term" missed the `vandc`). It changes the bail renders, so it needs the user's go.
  *Applied 2026-10-03 with the user's approval: see "Region impacts take |Δv·n|" below.*

### The per-player recomp hooks logged an NPC skater too; measurements re-checked (2026-10-03, recomp hooks + analysis; no engine change)

**Problem.** GREC / GRECX / FIRSTHIT / SKID / TREAT / SEAMPAT / SEAMHIT were meant to log the local rider only. Once an
NPC skater spawned (20–80 s into a session), they also logged its board, treatment and seams objects, interleaved per
frame. The earlier note "earlier sessions had one owner" was wrong: 15 of the 18 sessions with these hooks had the
second owner (local script `owner_scan.py`); only `all_20261002_204336`, `all_20261002_232153` and
`bailrun_verify_20261003_095507` are clean. In `all_20261002_180430` (the bed-level session) the NPC owner is 227,295 of
460,476 GREC lines.

**Root cause.** The hooks tested the 32-bit WORD at `[[object+16]+72]`. The game's local flag is the BYTE at +72 (the
skid updater `sub_824C7A20` reads `lbz 72` of `[owner+16]`; the eqchain / grain / tricks code reads `[[object+28]+72]`).
The NPC's component has byte 72 = 0 but a non-zero byte after it, so the word test passed. The TREAT / SEAMPAT mask bit 2
(state = GREC's last accepted state) alternated with the two owners, so it selected neither reliably.

**Change (recomp research hooks only, `src/research/hooks_audio.cpp`, uncommitted).** All seven kinds now gate on the
byte `[[object+28]+72]` (local72, as COLLPOST). SKID keeps its last holder per owner; FIRSTHIT keeps its per-state slots.
TREAT / SEAMPAT / SEAMHIT's last field: bit 4 = local72 (the gate), bit 1 = byte `[[object+16]+72]`, bit 2 = state equals
GREC's local state. New kind `LOCALTEST` (category `audio`, ≤ 256 lines, 6 fields; `trace.py` FIELD_COUNTS): per hook and
object, both bytes, the old word test and the decision, whenever the pair (old, new) changes.

**Proof (our own runs, `ride_spawn.txt` at the Mega-Park spawn, background, `SKATE3_TRACE=audio,audiox`, 0 malformed).**
`localtest2_20261003_102933`: at 21.57 s the NPC's board `40C34020`, treatment `40C58200` and seams `40C60560` start
passing the old test (bytes 0 / 0, word ≠ 0) and are rejected (`LOCALTEST … 0 0 1 0`); the local `40C33020` /
`40C581A0` / `40C60320` pass with bytes 1 / 1. GREC / GRECX / SKID / TREAT / SEAMPAT / SEAMHIT each logged one object
(33,048 / 33,048 / 28,533 / 33,048 / 33,047 / 929 lines) while the NPC was posting deck and body impacts (COLLPOST
local72 0, `40C710A0`) and visible in the screenshots. Run `localtest_20261003_102703` agrees (NPC seams object
rejected at 20.4 s).

**Measurements re-checked** (copies with only the local rows: local script `filter_local.py [--mask2]`;
scripts in the same folder; the local object is the first one each trace logs, confirmed by LOCALTEST):

| measurement (session) | as published | local rider only | conclusion |
|---|---|---|---|
| Retail turn input: share of riding frames at full / in between (180430) | 16 % / 25 % | **17 % (0.95–1.0, median 0.992) / 8 %**; release ≥ 0.9 → 0 in a median 34 ms | **Changes.** The 25 % was the NPC's steering (81 % of its frames in between). Retail's local input is as binary as ours (29 % / 8 %, release 50 ms in `state_20261002_181631`). |
| I vs \|turn\| medians 0.1 / 0.3 / 0.5 / 1.0 (180430) | 0.066 / 0.185 / 0.30 / 0.60 | 0.062 / 0.180 / 0.301 / 0.600 | Holds. Retail's I is 0 by 50 ms after the input reads 0 (p90 0.000, 366 releases). |
| Wheel-0 material changes while rolling (180430, `grec_material.py`) | 0.9 /s | **0.45 /s** (keep rates 100 % / 96 % unchanged) | Value changes; the fix it supported (materials from the wheel lines; ours was 5–9 /s) holds and is stronger. The comment in `skate_events.rs` (≈ line 543) still says 0.9. |
| Trucks: neither running / (1,0)↔(0,1) swaps (180430, `grec_running.py`) | 621 of 310 k / 39 k swaps | **9 of 233 k / 86 direct swaps** (+ ~310 through both-running) | "Never stops the last sounding truck" holds. The 39 k swaps were the two owners alternating; the hand-over is rare. |
| Clean straight-roll level(1) per material (180430) | 0.219 (mat 2, 5–10), 0.21 (40, 10–15), 0.229–0.244, 0.245–0.25 | 0.219, 0.215, 0.228–0.246, 0.243–0.250 | Holds (ours within ~1 dB). Material 65's low 5–15 km/h values (0.079 / 0.024) were the NPC; local 0.208. |
| Manual rows gain A, 10–50 km/h (180430) | 0.141–0.157 | 0.139–0.158 | Holds. |
| Class_Seams cadence (223306, `seampat.py`) | calls every 2.9 ms, frame field 2.5 ms; speed changes 18 %, positions 37 % | 3.0 ms, 3.0 ms; 19 %, 41 % | Holds (~3 ms pulses). The old `mask & 2` filter kept only 14,861 of 33,785 local calls. |
| Pattern 11 hits (223306) | 15–21 /s at 20–40 km/h | 15–29 /s | Holds (sparse sidewalk = short pulse). |
| Grid hits on a cell change of that wheel (223306, `seamhit_match.py`) | 400 of 520 | 2,677 of 3,156 (85 %); pattern 11: 470 of 619 | Holds. |
| TREAT replay, air-time +236 changes (223306), sense_of_speed replay (223613) | 860 of 5,094 | same | Already filtered by object (`40C581A0`, `40C33020`). The "57,323 lines" count includes the NPC (local 33,786, ~305 /s). |
| Clean carves A/(1−I), B/I; bed per material (`review/bed.py`) | — | — | Already filtered by owner (`rlib` / local owner). |

**Not separable:** voice-based figures (PLAY, SPLC: the riding Skate_Collisions / sk8_foley rates, seam voice rates
and levels) include an NPC's sounds whenever one was near, and these sessions have no COLLPOST. That was already noted
for the 1.7 /s body posts; a user session with `PLAY_TRACE_AUDIOX.bat` (COLLPOST) settles it.

**For the main session (no engine change made):**
- "(1) Carving": the turn input is not the difference. If ours really keeps I up for ~13 frames after the stick
  returns (retail: 0 within 50 ms), the cause is on our I side, not the animation's `Turn` scalar; worth a look.
- Hook consumers written before this fix need an object filter (or `filter_local.py`); new traces need none.

### The body poster on the console cadence (2026-10-03, `player::contacts`, open item 3 of "Bail density")

**Problem.** `Contacts::body` (retail `sub_824BC188`, called from the Contacts frame `sub_824B8218`) ran once per
`PlayerAudio::process`, and its 15-frame region cooldown counted those calls. The host runs the components once per
pass with at least one 60 Hz step (`native::mixmap_frame`), so the cooldown was 0.25 s at 60 fps and above but 0.5 s
at 30 fps. Retail runs the Contacts frame once per rendered frame: on the 30 fps console the cooldown is 0.5 s. The
standing rule applies: per-frame retail processes target the 360's ~30 fps cadence, at any engine fps.

**The 4-frame region max is not part of it.** The ring is the conditioner `sub_82773298`, called from
`sub_82772748` next to the landed latch `sub_82772FD8` (same caller, same pass). Those PhysOut conditioners run per
physics step (gotchas "MixMap console cadence": the recomp's `+236` air time steps by 1/60 on 17 % of its ~345 fps
calls), and the bail hook measured the ragdoll pass at 60 Hz in the recomp too. So "4 frames" = 4 physics steps =
67 ms on the console as well, which is what `skate_events::observe` (per physics tick) already does. Unchanged.

**Change.**
- `Contacts::body_calls: Option<usize>`: `Some(n)` runs the poster n times in this process (n console frames end
  here), `None` once (the old cadence; tests).
- `PlayerAudio::body_console` (`SKATE_AEMS_BODY_CONSOLE=0` off; e2e `E2E_BODY_CONSOLE=0`) sets `body_calls` to the
  MixMap cadence's evaluations of the pass (`jitter_steps`: `mixmap::cadence`, every second 60 Hz step on a fixed
  grid). Without the MixMap console cadence (`SKATE_AEMS_MIX_CONSOLE=0`) the old cadence stays.
- The NPC skaters' host does the same for its instance (`NpcSkater::set_body_calls`, the evaluations since its last
  call).
- Diagnostics: `Contacts::body_digest` (FNV-1a over every posted message) and, in the e2e harness,
  `<name>.ours.body.tsv` (the row where the count changed, the count, the digest).
- Below 30 fps a pass spans more than one console frame and the host has only the newest step's state, so the poster
  then runs twice on it (as the MixMap evaluations do).

**Proof of frame-rate independence.**
- Unit test `the_body_poster_keeps_the_console_cadence_at_any_frame_rate`: a 20 s 60 Hz stream of rising and falling
  impacts on both arm regions, run through a host at 30 / 60 / 144 / 240 / 365 fps (process on passes that complete
  steps, newest state, `Cadence`): the same messages on the same steps (counts and digests). The old cadence differs
  between 30 and 60 fps. A held impact posts on steps 2, 32, 62, 92: every 30 steps = 0.5 s.
- e2e, the two bail sessions (`state_20261003_084712`, `state_20261003_101640`; 0 malformed; cut with
  `scenarios.py --cut r0-999999`, 3,324 / 14,922 rows), `E2E_FPS` = 30 / 60 / 144 / 365: the body traces are
  byte-identical at all four rates. With the old cadence 30 fps differs from 60 (144 / 365 equal 60).
- Identity of everything else: `E2E_BODY_CONSOLE=0` renders are byte-identical (f32 and voices) to the pre-change
  build at all four rates. The optimisation bench (13 scenarios + 4 sessions, row and `E2E_FPS=300`,
  `opt/runs/cad_t1` against `cad_base`): IDENTICAL. Those logs carry no region impacts (row mode also keeps the old
  cadence).

**The e2e effect on bails (`E2E_FPS=60`, `bails_cmp.py`, window −0.5…+2.5 s; local tools).**

| | 084712 before | 084712 after | 101640 before | 101640 after |
|---|---|---|---|---|
| body-poster messages (whole session) | 35 | 30 (−14 %) | 102 | 91 (−11 %) |
| bails | 2 | 2 | 10 | 10 |
| Skate_Collisions voices / bail | 45.5 (46, 45) | 42.0 (43, 41) | 26.9 | 26.6 |
| three banks (Collisions + Metal + Bodyslide) voices / bail | 52.0 | 48.5 | 29.0 | 28.8 |
| three banks Σg² / bail | 1.895 (1.95, 1.84) | 2.114 (1.74, 2.49) | 0.954 | 0.997 |

The longer cooldown removes 11–14 % of the body messages, far less than the "up to 2×" bound: in our bails few
regions are hit again within 0.25–0.5 s. The voice counts drop slightly. Σg² moves both ways, because the posts that
remain fall on different steps of the impact curve (bail 2 of 084712 now posts on a stronger step: peak 0.715 →
0.976). The bail-density gap against the recomp (≈ 208 Skate_Collisions voices, Σg² 4.7 per bail) stays. Its cause is
the ragdoll's per-step Δv (open items 1–2) and, for a quarter of the floor crossings, the missing |Δv·n| (section
above). The recomp's post counts are inflated by its ~345 fps cadence (cooldown ≈ 43 ms there), so it is not the
reference for this rate either; the console's 0.5 s is.

**Files.** `crates/skate-audio/src/player/contacts.rs`, `crates/skate-audio/src/world/skaters.rs`,
`crates/skate-game/src/game_audio/{player_audio,e2e,npc_skaters}.rs`.

**Open.** The deck-impact poster `sub_824BD000` has the same shape (a 6-frame cooldown counted per process), and the
other Contacts / component processes still run per 60 Hz step. Not changed here (not asked; extending the rule is the
user's call).

### NPC skaters' second grain bed (2026-10-03, `grain::GrainBed`, `Runtime::npc_grains`, `game_audio/{grain_bed,npc_skaters}.rs`)

**Problem.** The NPC skater holding the Player slot's instance 1 (`world::skaters`, inert until an AI-skater system
publishes) ran its routing, but its grain binds were dropped: the runtime had one bed. Most surfaces (asphalt,
concrete, wood, aggregate, metal) are grain surfaces, so the NPC's rolling was silent there.

**Retail's mechanism (TU3 recompilation, reference only).** Every `SFXObj_SkateBoard` owns 2 trucks × 2 GrainPlayers
(owner `+1176 + 8t` / `+1180 + 8t`) and their chains. Its process `sub_824C6A78` (vtable `0x822FC780` +20) runs for an
active record (`[+28]+52`), and its update `sub_824C6BD8` (+24) has no local test, so instance 1's board runs its own
bed. The local tests (`[owner+16]+72`) in the bed's helpers, read one by one:

| part | function | for the NPC instance |
|---|---|---|
| slope inputs 2 / 3 (D, U) | `sub_824CA738` | runs (no local test) |
| routing, soft member | `sub_824C5CA8`, `sub_824C8370` | one pass, no hand-over (already ported in `player::rolling`); soft via `sub_824B23C8` = the local player's inverse |
| push envelopes, input 4, turn intensity | `sub_824C6198` → `sub_824C8588` | run |
| chain values (HPF / LPF / pan / env send / FSS) | `sub_824C9058` | run; the graph-2 FlangeSub sends level(21) / (22) are local only |
| seam-pattern gain envelope | `sub_824CA448` | returns at once (local only) |
| graph-1 → graph-3 send, level ramp | `sub_824CAEC0` | returns at once |
| gain wobbles | `sub_824CB180` (→ `sub_824CB078`) | returns at once: the graph-1 / graph-3 Gains stay at the module default 1 |
| graph 3 | `sub_824C8878` (chain build) | built only for the local player (`lbz 72` before the graph-3 build); the eEQChain create flag is the local byte too |
| records (gain / pitch / position), brake slew, latches | `sub_824C6BD8` | run, from the instance's SkateBoard outputs level(1) / (2), pitch(3) |
| rocket `x_jet_rolling` | SenseOfSpeed `sub_824E7980` | returns for non-local (already noted) |
| pick generator | `sub_82A8AF10`, `0x82FD7D74` | one title-wide generator for every GrainPlayer |

**Change.**
- `GrainBed::local` (true by default): false skips graph 3 (`process_full(…, graph3)`). `GrainBed::share_rng` runs a
  closure with another generator swapped in.
- `Runtime::npc_grains: Option<Box<GrainBed>>`: rendered after the local bed on the local bed's generator (retail's
  one generator), at the local bed's user gain and `chain_extras`. The env sends of both beds are summed when both
  send; its dry mix is added after the local one. `None` = the previous render path exactly.
- `grain_bed::Bed` gains its instance. `Bed::for_instance(g)`: the same tunings and decoded recordings, no rocket.
  `write_inputs` writes `SkateBoard(g)` inputs 2 / 3 / 4. `step_with` (the old `step` body, with the runtime handed
  in) gates the local-only parts above and targets `npc_grains` (created at the NPC bed's first step; binds draw
  through the shared generator). `step` keeps its signature and order for the local player.
- `npc_skaters::frame`: at the claim (native rolling layers on) the held skater gets `Bed::for_instance(1)`. Per
  console evaluation: update → the bed step on this evaluation's outputs with the routing's binds of the last
  process → `write_inputs` (2 / 3 / 4) → the instance's inputs → process. Push plants = `+335` rises
  (`push_trigger`); brake = `+336` (`AudioState::brake`). At the release the bed is dropped and the trucks stop.
  Logged as `AUDIO_NPC grain bind instance 1 truck …`.

**Verification.**
- `grain::bed::tests::a_non_local_bed_has_no_graph3_and_shares_the_generator`: without graph 3 the send level is
  inert; with it, it adds the copy. At send 0 a built graph 3 adds only its filters' anti-denormal bias (< 1e-12;
  retail builds none for the NPC). A bed on a shared generator renders exactly like one owning that state, and its own
  generator never moves.
- `npc_skaters::tests::npc_skater_rolls_on_its_own_grain_bed` (real install; the synthetic pass of the existing NPC
  test): the second bed binds `concrete_smooth_soft` (local wheels hard → soft member), 2–4 voices while held, record
  A gain 0.004 at 29 m → 0.096 at 10 m → 0.009 at 29 m (the instance's MixMap distance roll-off), nothing after the
  release, the local bed never bound. In the air before the rail, the fixture's "no material" wheels rebind
  `asphalt_smooth_soft` (surface 3): the routing's known UNCERTAIN air behaviour (grain spec §2.10).
- The local player's renders: see "Verification of the 2026-10-03 cadence / NPC-bed stage" below.

**Open.**
- What the released instance's players do in retail (here they stop, as the packets are released): not read.
- Retail's order of the two owners' picks on the shared generator (ours: the local bed first per block). The picks
  are random either way; it only matters while an NPC is held.
- The brake slew's direction test (`+336` against the direction of travel) is unported for both beds.

### Verification of the 2026-10-03 cadence / NPC-bed stage (the two sections above)

- `cargo test -p skate-audio --locked`: all pass (lib 220 + the integration tests; new:
  `the_body_poster_keeps_the_console_cadence_at_any_frame_rate`, `a_non_local_bed_has_no_graph3_and_shares_the_generator`).
- `cargo test -p skate-game --release --bin skate3rust --locked -- game_audio::`: 55 pass, 4 ignored (new:
  `npc_skater_rolls_on_its_own_grain_bed`).
- `cargo build --locked` (dev): OK, only the 3 known warnings (`library.rs` ×2, `prefetch.rs`).
- e2e identity (own target folder; tools `tools/audio-bench/e2e_bench.sh`,
  local `render.sh`):
  - the optimisation bench, 13 scenarios + 4 sessions, row and `E2E_FPS=300`: `cad_base` (before) = `cad_t1` (body
    cadence) = `cad_t3` (+ NPC bed), byte-identical (f32 and voices), and equal to `retire1`;
  - the bail sessions 084712 / 101640: `E2E_BODY_CONSOLE=0` = before at 30 / 60 / 144 / 365 fps; after Task 3 = after
    Task 1 at 60 and 30 fps (f32, voices and body traces). So the NPC bed leaves the local player's renders untouched.
- Headless only: no game launch, no smoke test, bin\ / data\ untouched. For the user in game: bails (the body poster
  now has the console's 0.5 s cooldown at 60+ fps). The NPC bed stays silent until an AI-skater system publishes.

### Region impacts take |Δv·n|; the deck poster on the console cadence (2026-10-03, user-approved follow-up)

The user approved both follow-ups of the sections above: retail's |Δv·n| and the console cadence for the deck poster.

**1. |Δv·n| in `skate_events::region_impacts`.**
- Change: `along = (Δv · n).abs()`, matching the `vandc128` sign mask in `sub_82BD60C8`. A part pulled away from the
  surface now counts like one stopped by it, no longer flooring to 0.001.
- Tests:
  - `region_impacts_scale_the_velocity_change_by_the_part_mass` pins it: −2 m/s gives 0.2; a tiny step still reads the
    0.001 floor.
  - `region_impacts_match_the_recomps_bail_lines` (data-gated: the fixed-hook runs t2 / ok / ok2 / ok3) rebuilds every
    BAILREG line's Δv along its normal with its mass. Ours equals the written impact within 0.002 on **12,054 / 12,054
    lines**, 5,355 of them with Δv·n < 0.
- **The e2e can't show it.** The harness replays the impacts the game logged (`rimp0..5`), and the two bail sessions
  were logged with the signed formula. Re-rendered 084712 / 101640 at `E2E_FPS=60` with the new build (and the deck
  cadence off): byte-identical to the current state. So the per-bail change (Skate_Collisions voices, Σg², tiers) needs
  a new user session with this build.
  - What it acts on: in the logs, contact region-frames at the 0.001 floor are 1,035 of 1,237 (084712) and 649 of
    1,042 (101640), after the 4-step max. Some of these are negative steps that will now read their |Δv·n|.
  - In the recomp, 26 % of the above-floor steps are negative, mostly small: 8 of 166 above 0.1.
  - So expect more tier-0 body / cloth posts and few new tier-1 / 2 ones.

**2. The deck-impact poster `sub_824BD000` on the console cadence.**
- It had the body poster's shape: a 6-frame cooldown (vault `27D3C5DC3282B59D`) counted per `process`, so 0.1 s at
  60+ fps and 0.2 s at 30 fps. The 4-step deck-impact max (`+668`) is the per-physics-step conditioner: unchanged.
- Change:
  - `Contacts::deck_calls` (as `body_calls`); `PlayerAudio::deck_console` from the MixMap cadence's evaluations.
  - The NPC instance follows: `NpcSkater::set_deck_calls`.
  - Off switches: `SKATE_AEMS_DECK_CONSOLE=0` / `E2E_DECK_CONSOLE=0`.
  - Diagnostics: `deck_posts` / `deck_digest`, and e2e writes `<name>.ours.deck.tsv`.
- Proof:
  - Unit test `the_deck_poster_keeps_the_console_cadence_at_any_frame_rate`: the same messages on the same steps at
    30 / 60 / 144 / 240 / 365 fps; the old cadence differs at 30. A held impact posts on steps 2, 14, 26, 38, 50, i.e.
    every 12 steps = 0.2 s.
  - The bail sessions at `E2E_FPS` 30 / 60 / 144 / 365: deck traces byte-identical across the four rates; the old
    cadence's 30 fps differed.
  - Off = before: `E2E_DECK_CONSOLE=0` renders are byte-identical to the current state (at all four rates for the bail
    sessions; the bench `cad_dkoff` against `cad_t3`, row and fps300).
- Effect on the bench (`cad_dk` against `cad_t3`):
  - Row mode: identical (no console cadence there).
  - `E2E_FPS=300`: 5 outputs change, the scenario `ollies_log` and the sessions 213757, 214224, 215843, 224747. The
    other 12 scenarios are identical.
  - In each changed output the body trace is unchanged and the first differing voice row comes exactly one row after the
    first differing deck post (local script `deck_attr.py ON OFF`).
  - Later rows keep differing: from that post on, the shared Splice picks shift.

| deck messages (`E2E_FPS`=300 bench / 60 bails) | old | console |
|---|---|---|
| ollies_log | 3 | 3 (timing moved) |
| 213757 / 214224 / 215843 / 224747 | 11 / 2 / 18 / 19 | 9 / 2 / 15 / 19 |
| 084712 / 101640 | 6 / 55 | 6 / 44 |

- Bails (`E2E_FPS=60`, against the current state, `bails_cmp.py`):

| | 084712 now | 084712 deck console | 101640 now | 101640 deck console |
|---|---|---|---|---|
| Skate_Collisions voices / bail | 42.0 | 41.0 | 26.6 | 25.7 |
| Skate_Collisions Σg² / bail | 1.926 | 1.600 | 0.982 | 0.970 |
| three banks voices / bail; Σg² / bail | 48.5; 2.114 | 47.5; 1.788 | 28.8; 0.997 | 27.6; 0.982 |
| body / cloth voices, tier 0 / 1 / 2 | 47 / 5 / 0 | 47 / 5 / 0 | 110 / 26 / 2 | 107 / 27 / 4 |

  - 084712's bail 2 Σg² drop (2.485 → 1.834) has the same count of deck posts; the post timings moved and so did the
    Splice picks after them.
  - The tier counts come from `bails_cmp.py`'s top-24 ids per bail; the body traces themselves are unchanged.

**Verification.**
- `cargo test -p skate-audio --locked`: all pass (lib 221).
- `game_audio::`: 56 pass, 4 ignored.
- `cargo build --locked`: OK, with the 3 known warnings.

**Files.** `game_audio/{skate_events,player_audio,e2e,npc_skaters}.rs`, `skate-audio/src/player/contacts.rs`,
`skate-audio/src/world/skaters.rs`.

### Bail density: the missing speed curve, not the ragdoll (2026-10-03, research only; no engine change)

Spec: `audio-specs/ragdoll-contact-spec.md` (retail mechanism, ours, decision points). Summary:

- **The retail Δv figures above (p99 1.8–13 m/s, max 7.6–20) are not comparable with our bails.** The scripted runs at
  the Mega-Park spawn bailed into the gap or slid down the MegaRamp roll-in, so the body hit at 11–23 m/s (screenshots;
  e.g. foot v·n −23 → −12 → −1 m/s). In the ordinary-height tumbles of run `bailx3_20261003_110208` (first 2 s of each
  bail) retail's |Δv·n| is p90 2.3, max 3.7 m/s, approach speeds ≥ −4.3 m/s: the size of ours. The ragdoll solve
  (Horus solver, 50 iterations, restitution 0, ragdoll friction 0.9, angular drag 0.5 × 60, ragdoll masses) is ported in
  ours from the same code and data; no difference was found in the contact response.
- **What is missing: the audio bridge's speed curve.** `sub_824B0DA8`, after copying the conditioner's 8 region impacts
  into the audio state (`+496..`), multiplies each by an 8-point graph (`sub_82481E10`) of the previous frame's |COM v|
  (`+216` ← `+212`): x 0 / 0.366 / 0.448 / 0.521 / 0.593 / 0.741 / 0.863 / 0.946, y 1.0 / 1.2 / 1.486 / 1.914 / 2.429 /
  3.6 / 4.571 / 5.0 (read from the running game with a watch on `[[[0x830CFDA4]+36]+4]`). New recomp hook `BANDQ` on
  `sub_82497088` (impact, tier, band edges per query) shows the body poster's impact = 5.0 × the clamped region impact
  (ratio p50 5.0 over 218 posts) and band edges equal to our exported table. Ours skips the curve, so our body impacts
  are 1/5 of retail's at the poster: no tier 1 / 2, and concrete 1047 / 991 (> 1.0 / 1.85) unreachable — this also
  answers the open question "which impact posts concrete tier 1 / 2" above.
- **Not changed** (needs the user's go): port the curve in `skate_events` (audio only; skate-core untouched), together
  with the |Δv·n| abs and the body poster's 30 fps cadence; then re-measure bail density against the recomp.
  *Ported 2026-10-03 (user: "Port it"): see "The bridge's speed curve on the body impacts" below.*
- Script: `bail_ledge.txt` now uses `lt rt l3 r3` (6 / 6 bails; user tip), with an X jump + mid-air `l3 r3 lt rt` fallback.

### The bridge's speed curve on the body impacts (2026-10-03, user decision "Port it"; `player::contacts`, `skate_events`)

The user decided (2026-10-03): port retail's speed curve on the body impacts ("Port it"); the ragdoll physics item is
closed with no physics change.

**Listening before the curve** (user, session `state_20261003_110725`, build with |Δv·n| and the body / deck posters on
the console cadence, no curve): "bails sounded good, a bit soft like you said. Deck hits sound really good and seem to
be either equal to retail or almost perfect to retail." The deck hits are approved, so the curve must leave the deck
posts alone (shown below).

**Retail's mechanism (TU3 recompilation, reference only).**
- The audio-state bridge `sub_824B0DA8` copies the skater entry's 116-byte block `+320` into the audio state `+496`.
  This holds the conditioner's 8 region impacts, already clamped to [0, 1] by `sub_82BD60C8`.
- Then, for i = 0..7: `+496 + 4i` = `fmuls`(graph(`+216`), `+496 + 4i`). The graph is `sub_82481E10(8, rec + 16, rec + 48, v)`
  with rec = `[[0x830CFDA4] + 36] + 4`.
- **No clamp after the multiply.** The poster sees up to 5.0. The recomp's BANDQ lines agree: the body poster's queries
  (callers `824BC4C8` / `824BC530`) reach 4.11 (bandq run) and 4.88 (bailx3 run), concrete (material 2) up to 2.72 at
  tier 2. The deck poster's queries (`824BD090` / `824BD0C4`) stay ≤ 1.0.
- The deck impact `+668` lies outside the multiplied range (`+496..+524`).
- **Which frame.** `+212` is written earlier in the same call from the skater entry's `+108`. `+216` is read by the
  multiply, then set from `+212` after the loop. So the graph sees the *previous* bridge call's `+212`.
- **Units.** The entry's `+108` is written by `sub_827A1B78`: the length (`vmsum3fp128` + `vrsqrtefp128` with two
  Newton steps) of the vector at `[r30 + 36] + 16`. That is SystemReckoning `+16`, the COM velocity, in m/s. It is the
  same quantity as our `AudioState::com_speed()` (the footsteps' walk curve reads it too).
- The BANDQ floor queries agree with m/s. A lying body posts 0.00101–0.00116, i.e. ×1.0–1.16, and the graph gives that
  below 0.37 m/s.
- **Interpolation** (`sub_82481E10`, already ported for the footsteps' 16-point curves):
  - below x0 → y0; from x7 on, or for an unordered input → y7;
  - in between, the first i with v < x[i]: slope first, then `fmadds`;
  - y[i] where two x coincide.
- **Data: the install's own vault.** The words read from the running game equal the stock vault's class
  `6EBA5BCD3E38A98A` `default` field `8B164823E008749C` (a `Sk8::PointNegGraphData8`, the same collection as the body
  cooldown and the deck cooldown). Header 0 / 1 / 1 / 5, x at +16, y at +48:
  - x 0, 0.3664494, 0.4478826, 0.5211726, 0.592834, 0.7410426, 0.863192, 0.946254;
  - y 1.0, 1.2, 1.485714, 1.914286, 2.428571, 3.6, 4.571427, 5.0.

**Change.**
- `skate_audio::player::contacts::SpeedGraph8`: the 8-point graph and its evaluator. `ContactsTuning::body_speed_curve`
  defaults to the vault words (`SpeedGraph8::BODY_SPEED`, bit patterns).
- Setup exports the graph: `audio_export.collision_tuning` posters `body_speed_x` / `body_speed_y`. `library.rs`
  prefers them when present. Installs set up before today use the identical default.
- `AudioState::com_speed_216`: retail's `+216`. `skate_events::observe` sets it per physics tick to last tick's
  `com_speed()` (`Seen::com_speed_212`).
- `AudioState::body_impact` stays the conditioner's (pre-graph) value. `Contacts::body` multiplies each region's
  impact by `body_speed_curve.eval(com_speed_216)` (f32, no clamp). The cloth and pad contacts read the same product,
  as retail's poster reads `+496`.
- Switch `Contacts::body_speed_on`: `SKATE_AEMS_BODY_CURVE=0` / `E2E_BODY_CURVE=0` turn it off, giving the impacts as
  logged. The NPC instance's Contacts leaves it off. Its state has no `+216` (0 → ×1.0) and no ragdoll impacts yet.
- The deck poster, the 4-step region max and `region_impacts` are unchanged.
- **Replays.** The state log gains a `com_speed` column (`+212`, appended), and `scenarios.py` (both copies) carries
  `com_speed` and `com_x/y/z` into cut scenarios. The e2e harness computes each row's `+216` from the previous row:
  - from `com_speed` when the log has it;
  - else (logs before today) |Δ COM position| × 60 from `com_x/y/z`, the reckoning's followed point at 4 decimals
    (±0.006 m/s). The graph saturates from 0.95 m/s, so this only matters for slow bodies. A respawn jump reads as fast
    (×5, as any speed > 0.95);
  - else 0 (×1).

  So the old bail sessions render with the curve.
- Diagnostics: `E2E_BODY_LOG=1` writes `<name>.ours.bodymsg.tsv` (every body message with its row, region, the impact
  the poster read, `+216`, materials, tiers, levels). `Contacts::body_log` has no effect on the output.

**Tests.**
- `the_bridge_speed_graph_is_the_vault_curve_piecewise_linear_and_clamped` checks the knots, both clamps, NaN, the
  midpoint, and is bit-exact against the slope-first `mul_add`.
- `the_speed_graph_scales_the_body_impacts_not_the_deck`: at rest (×1) a 0.3 arm hit on concrete is arm tier 1 /
  concrete tier 0. Moving (×5) it is 1.5, i.e. arm tier 2 / concrete tier 1 (1047). 0.4 → 2.0 gives concrete tier 2
  (991). Off equals at rest. 0.0011 × 5 crosses the arm floor. The deck messages are identical with the graph on and off.
- `the_body_speed_graph_is_the_stock_vault_record` (`library.rs`, data-gated on the converted `skater-collections.json`):
  exported posters win; the default equals the vault record word for word.

**Frame-rate independence and identity** (local scripts `render.sh`, `bails.py`, `body_attr.py`;
inputs re-cut with `scenarios.py --cut r0-999999`, 0 malformed).
- Body traces with the curve are byte-identical at `E2E_FPS` 30 / 60 / 144 / 365 for all three sessions.
- `E2E_BODY_CURVE=0` renders (f32 and voices) are byte-identical to the previous state's (`bodycad/dk`) at all four rates
  for 084712 / 101640. The new COM columns are inert when the curve is off.
- **Deck posts unchanged:** the deck traces (count + FNV digest of every message) are identical with the curve on and
  off in all three sessions, at every rate.
- In each session the first differing voice row is the row of the first differing body post (084712 row 1101, 101640
  row 2427, 110725 row 1963, all in a bail, state 300). Before it, the voices are identical.
- After that the voices keep differing, outside the bails too: the shared Splice picks shift, as with the deck cadence
  change. The deck hits' messages (timing, materials, tiers, levels) stay the same. Only the random variant a hit draws
  may differ after the first bail.
- **Bench** (13 scenarios + 4 sessions, row and `E2E_FPS=300`, `opt/runs/curve` against `cad_dk`): IDENTICAL. Those
  inputs carry no region impacts and no COM columns, so they cannot change. The graph acts only on body posts, and it
  scales them only while the body moves (> 0 m/s): ×1.2 at 0.37 m/s, ×5 from 0.95 m/s.

**Per bail** (`E2E_FPS=60`; posts = message sides with a tier, each plays one collision id; window = bail rise − 0.5 s …
bail clear, as the recomp's 094336 summary; voices / Σg² = `bails_cmp.py`, −0.5…+2.5 s):

| per bail | the recomp (094336, 10 bails) | 084712 off → on (2) | 101640 off → on (10) | 110725 off → on (7) |
|---|---|---|---|---|
| body posts | 71 | 15.0 → 24.5 | 7.8 → 10.9 | 15.1 → 20.0 |
| tier 0 / 1 / 2 | ≈ 49 / 13 / 9 | 14.0 / 1.0 / 0 → 19.5 / 3.5 / 1.5 | 6.5 / 1.1 / 0.2 → 6.7 / 2.9 / 1.3 | 12.7 / 1.7 / 0.7 → 12.3 / 5.4 / 2.3 |
| tier-2 1030 / 1031 / 1032 | 1.8 / 3.3 / 2.1 | 0 / 0 / 0 → 0 / 0.5 / 0.5 | 0 / 0 / 0 → 0.1 / 0.6 / 0.3 | 0 / 0.1 / 0 → 0.3 / 0.9 / 0.3 |
| torso 951 | 1.0 | 0 → 0.5 | 0.2 → 0.2 | 0.3 → 0.4 |
| concrete 958 / 1047 / 991 | 3.4 / 2.3 / 0.9 | 1.0 / 0 / 0 → 2.0 / 0.5 / 0 | 0.1 / 0 / 0 → 0.5 / 0 / 0 | 0.6 / 0 / 0 → 1.9 / 0.4 / 0 |
| Skate_Collisions voices (recomp: 38 bails, nine sessions) | 207.7 | 41.0 → 59.5 | 25.7 → 30.6 | 35.6 → 43.7 |
| three banks Σg² | ≈ 5.1 | 1.79 → 2.52 | 0.98 → 1.03 | 1.58 → 1.78 |
| peak impact the poster read (max over the bails) | 4.9 (bailx3) | 0.21 → 1.03 | 0.32 → 1.59 | 1.00 → 5.00 |

- What changed: the tier-1 / 2 hits and concrete 1047 now occur. The posts per bail rise 30–60 %: floor contacts
  0.001 × 5 = 0.005 pass the arm floor, and more regions clear their bands.
- Concrete 991 (> 1.85, i.e. a region impact > 0.37 while moving) still doesn't occur in our bails.
- The gap to the recomp's 71 posts stays, but the recomp is not the reference for counts. Its ~345 fps cadence makes
  the 15-frame cooldown ≈ 43 ms (0.5 s on the console). With 6 regions and a 0.5 s cooldown, our poster can't post
  more than ~12 groups a second.
- Σg² rises less than the counts. A tier-2 hit's level comes from its own window, and many of the new posts are
  quiet tier-0 floor hits.
- 110725's last bail (155.9 s, 4.5 s long) has no region impacts at all (`rimp*` 0 throughout). Not investigated.

**|Δv·n| against the older sessions** (110725 was recorded with |Δv·n|, 084712 / 101640 without; from the logs):

| | 084712 | 101640 | 110725 |
|---|---|---|---|
| contact region-frames at the 0.001 floor | 84 % (1,035 / 1,237) | 62 % (649 / 1,042) | 76 % (1,950 / 2,554) |
| above-floor region-frames / bail | 101 | 39 | 86 |
| region-frames > 0.1 / > 0.25 | 12 / 0 | 68 / 16 | 71 / 29 |
| body posts / bail, no curve | 15.0 | 7.8 | 15.1 |

- No session-level signature of the abs. The bails differ more between sessions than the expected effect: 26 % of
  above-floor steps in the recomp, mostly small, 8 of 166 above 0.1.
- The abs can't be isolated without the same bail logged both ways (the replay reads the logged impacts). 110725's
  many hits above 0.25 are its larger falls (peak 1.0).

**Files.** `crates/skate-audio/src/player/{contacts,state}.rs`,
`crates/skate-game/src/game_audio/{skate_events,state_log,e2e,player_audio,library}.rs`,
`tools/asset_pipeline/audio_export.py`, `tools/audio-e2e/scenarios.py`.

**Open.**
- The NPC instance gets no `+216` (its state comes from `world::skaters`, and no ragdoll impacts are published).
- Old logs use the COM-position fallback. Sessions from this build log `com_speed`.
- For the user in game: bails should now have more tier-1 / 2 body hits and concrete tier 1 (louder, punchier). Deck
  hits are unchanged.

### The turn intensity after release; trace / comment leftovers (2026-10-03)

**Turn intensity "linger": no change. The recomp's 50 ms is its frame rate.**
- Retail's `sub_824C8588` moves the signed I toward the raw target by the grain collection's step per call: `+1160` ±
  the primary truck's rise / fall step (the vault field lookup `sub_82B72420`; 0.06, 0.1 on one surface). There is no dt.
- The local GREC rows of `all_20261002_180430` (filtered by `filter_local.py`; 1,874 calls where the input reads 0 and
  I > 0.07) step by exactly 0.06 per call (1,832; 0.1 on 36), at 2.8 ms per call (the recomp's ~345 fps).
- So the recomp's I is back to 0 in ~10 calls ≈ 30 ms. That is the "0 by 50 ms" figure, a recomp artefact. On the
  30 fps console the same 10 steps take ~333 ms.
- Ours: `TurnIntensity::step` with `frames = dt × 60` takes 0.06 per 1/60 s.
  - In 110725 / 101640 our input (the animation's Turn) falls from ≥ 0.9 to 0 in 3 frames (p50; p90 4–5), like
    retail's 34–50 ms.
  - Our I then reaches 0 in 8 frames (p50; p90 9–10, simulated from the log at cap 0.6): the "~13 frames" from the
    release start.
  - That is already half the console's time, not a linger.
- The retail mechanism on the console-cadence rule would be 0.06 per console frame (`frames = dt × 30`; the brake slew
  `BRAKE_STEP` has the same shape). That doubles our fall time to ≈ 330 ms and changes the rolling bed in every riding
  scenario. Not changed: it is the opposite of the reported symptom and needs the user's call.
  **Decided 2026-10-03 ("lets go for like retail"): see "The turn and brake slews on the console cadence" below.**

**Leftovers.**
- `skate_events.rs`: the wheel-material comment now says 0.45 changes / s (local rider only).
- The published `tools/recomp-trace/trace.py` FIELD_COUNTS gains BAILLOCAL, BAILSTEP, BAILREG, BAILCAND, COLLPOST,
  BANDQ and LOCALTEST (scrubbed comments).

**Verification (both sections).**
- `cargo test -p skate-audio --locked`: all pass (lib 223).
- `cargo test -p skate-game --release --bin skate3rust --locked -- game_audio::`: 57 pass, 4 ignored.
- `cargo build --locked`: OK, with the 3 known warnings.
- Headless only (own target folder); bin\ and data\ untouched.

**Listening (user, 2026-10-03, bin\ 11:34 with the bail speed curve; session `state_20261003_113953`, 0 malformed):** "Sounds so much better!"

### The turn and brake slews on the console cadence (2026-10-03, user decision "on the decision lets go for like retail"; `grain::board::OwnerSlews`, `game_audio/grain_bed.rs`)

The user decided on the open item of "The turn intensity after release" above: "on the decision lets go for like
retail".

**Retail's mechanism (TU3 recompilation, reference only).**
- The SkateBoard process `sub_824C6A78` (vtable `0x822FC780`, called with the frame's dt in f1) runs, in order:
  slope `sub_824CA738`, routing `sub_824C5CA8`, the owner modulators `sub_824C6198` (dt passed on), then
  `sub_824C9058`, `sub_824C7438`, `sub_824C7738`, `sub_824C9F68` and the seam envelope `sub_824CA448` (dt).
- In `sub_824C6198`, everything that uses time takes dt: the push speed-scale and shift envelopes (`+912` / `+1036`)
  advance through `sub_8248D510` with f1 = dt.
- Two values move by a fixed step per call with **no dt**:
  - **Turn intensity** `sub_824C8588` (the call at `0x824C68F4`; the result is stored as `+1164`): the signed `+1160`
    moves toward the raw target by the primary truck's rise or fall step from the vault (`sub_82B72420`; default
    word at `0x830D0850`), with `fadds` / `fsubs`, then `stfs +1160`.
  - **Brake** `+1168`, inline after it. While braking (state `+336`), the cosine of the two velocity vectors
    (state `+128`, `+96`) is checked: below 0, slew toward 1 (f31); otherwise toward 0 (f27). Without braking, slew
    toward 0. The step is the `lfs` at `0x82165A00` (0.05, grain-player-spec §2.7). The value snaps to the target
    once within one step.
- The function has no other per-call slews. The manual latch (`+1504`, inline in `sub_824C8588`) and the trick latch
  (`sub_824CA6E0`) are set / clear latches, not steps. They stay per rendered frame here: they are idempotent on a
  repeated state, and sampling them at 30 Hz would drop one-step pulses (the reason the MixMap flags needed
  `hold_input`).
- **Call site cadence.** The process runs once per call of the audio manager's half (`sub_82485190`, "The MixMap on
  the console cadence"). On the ~30 fps console both halves run on every frame, so: one step per console frame. A
  full release from the cap 0.6 to 0 takes 10 calls (f32 leaves 7.45e-9, which the 11th call removes) = 333 ms. A
  full brake release 1 → 0 takes 20 calls = 667 ms.
- Ours stepped by 0.06 / 0.05 × (dt × 60), i.e. per 1/60 s: twice as fast (167 ms / 333 ms). The NPC bed ran at
  `dt = CONSOLE_DT × evaluations`, so its step was also 0.12 per evaluation.

**Change.**
- `skate_audio::grain::board::OwnerSlews { turn, brake }` and `board::BRAKE_STEP` (moved from `grain_bed.rs`).
  `OwnerSlews::step(tuning, com_speed, turn, special, braking, calls, frames)`:
  - `calls = Some(n)`: n retail calls (each step × 1);
  - `None`: once, with the steps × `frames`, the old code path.
- `grain_bed::Bed`:
  - `slews` replaces `turn` / `brake`.
  - `slew_calls` is set by the host before each step and taken by it: the console evaluations of this pass from
    `mixmap::cadence`.
  - `slew_console` (`SKATE_AEMS_SLEW_CONSOLE=0` off) is copied to the NPC beds.
- Hosts:
  - `native::mixmap_frame` sets `slew_calls = Some(0)` on every frame (also frames without a 60 Hz tick) and
    `Some(calls)` on frames that tick. With `SKATE_AEMS_MIX_CONSOLE=0` it sets `None`.
  - The NPC host sets `Some(evaluations)`.
  - e2e: `E2E_SLEW_CONSOLE=0` off. The cadence follows `E2E_MIX_CONSOLE` (on with `E2E_FPS`, off in the per-row
    renders).
- Between console frames the bed keeps feeding the records the held |I| and Bk.
- Diagnostics: `E2E_SLEW_LOG=1` writes `<name>.ours.slew.tsv` per call (turn input, |COM v|, I, signed I, braking,
  Bk). It has no effect on the output.

**Proofs** (local tools `render.sh`, `compare.py`, `localise.py`, `release.py`).
- Inputs: the sessions 084712, 101640, 110725, **113953** (cut with `scenarios.py --cut r0-999999`; 5,484 rows =
  lines − 1, so 0 malformed) and the bench's 213757, 214224, 215843, 224747.
- **Off = byte-identical.** `E2E_SLEW_CONSOLE=0` renders (f32, voices, body, deck) equal the pre-change tree's at
  `E2E_FPS` 30 / 60 / 144 / 365 for all eight sessions. `E2E_SLEW_LOG=1` was on in those renders.
- **Frame-rate independence.**
  - Unit test `owner_slews_are_the_same_at_any_frame_rate`: a host that runs once per rendered frame with the newest
    60 Hz state and the cadence's calls gives bit-identical I / Bk on every console frame at 30 / 60 / 144 / 240 / 365
    fps. The old per-frame host differs between 30 and 60.
  - e2e: the slew logs' values on every evaluation are identical at 30 / 60 / 144 / 365 for all eight sessions.
- **Step count.** Unit test `owner_slews_take_one_step_per_console_frame`:
  - one 0.06 / 0.05 step on each console frame, none on the steps between;
  - turn 0.6 → < 1e-6 after 10 console frames (20 steps), exactly 0 after 11;
  - brake 1 → 0 after 20 (40 steps);
  - the old host takes 11 steps;
  - without a primary truck the brake still slews and the turn holds (reads 0).
- **What changes (on vs off, `E2E_FPS=60`).**
  - Only grain-bed records differ: grain0/1 A/B gain, pitch and position. No other bank's voices differ in any
    session.
  - Every differing voice frame lies within 0.5 s of a call where I or Bk differ.
  - The first differing voice frame is at or after the first slew difference.
  - Per session:

    | session | calls where I / Bk differ | voice frames that differ |
    |---|---|---|
    | 084712 | 615 / 0 of 3,384 | 17.9 % |
    | 101640 | 2,905 / 104 of 14,982 | 19.6 % |
    | 110725 | 1,788 / 0 of 9,804 | 17.5 % |
    | 113953 | 646 / 0 of 5,544 | 11.6 % |
    | 213757 | 565 / 0 of 2,838 | 19.7 % |
    | 214224 | 137 / 0 of 1,746 | 7.6 % |
    | 215843 | 844 / 0 of 11,712 | 6.9 % |
    | 224747 | 1,893 / 1 of 8,208 | 22.6 % |
- **Bench** (13 scenarios + 4 sessions; `opt/runs/slew_on` against `slew_base`):
  - row mode: IDENTICAL (the per-row renders keep the old cadence);
  - `E2E_SLEW_CONSOLE=0` (`slew_off`): IDENTICAL in both modes;
  - fps300: brake20, carve20 and ollies_log change (the scenarios with braking or turning) and the 4 sessions. The
    other 10 scenarios are identical (roll10/20/30/45, grind_ledge / metal, manual15, ollie20, slide20, sweep).

**Before / after release times** (`release.py`; a turn release = a continuous fall of I from a peak to 0, from the last
row at the peak; the eight sessions pooled; `E2E_FPS=60`, the same at 30):

| | before (off) | after (on) |
|---|---|---|
| turn, peak ≥ 0.5 | n 193, p50 217 ms, p90 400 ms | n 61, p50 367 ms, p90 800 ms |
| turn, peak ≥ 0.3 | n 320, p50 167 ms, p90 300 ms | n 168, p50 267 ms, p90 500 ms |
| brake (sessions, n 2–3) | 333 ms | 517–533 ms |
| brake20 scenario | 1 → 0 in 333 ms, 0 → 1 in 333 ms | 667 ms each (the scenario ends at 0.25, 30 rows after the release) |
| turn from the cap 0.6, input to 0 (theory) | 11 steps = 183 ms | 10 console frames = 333 ms (+ 1 for the f32 remainder) |

- Fewer releases reach 0 after: with the slower fall the rider often turns again before I is back at 0.
- The long tails (≈ 1–5 s) are slow input ramps, not the slew.

**Files.** `crates/skate-audio/src/grain/board.rs`,
`crates/skate-game/src/game_audio/{grain_bed,native,npc_skaters,e2e}.rs`.

**Verification.**
- `cargo test -p skate-audio --locked`: all pass (lib 225, 2 new).
- `cargo test -p skate-game --release --bin skate3rust --locked -- game_audio::`: 57 pass, 4 ignored.
- `cargo build --locked`: OK, with the 3 known warnings.
- Headless only (own target folder); bin\ and data\ untouched.

**For the user in game.** After a carve the turning layer (grain player B, the turn gain) fades out over about a third
of a second instead of a sixth. The brake layer fades in and out over 2/3 s instead of 1/3 s. Straight rolling, tricks,
bails and grinds are unchanged.

### Concrete ledges grinded as metal: the grind material is the tag − 1 (2026-10-03, `skate_events`, `e2e`)

**Report (user, 2026-10-03, session `state_20261003_115334`, 0 malformed):** "the last area I was in, the concrete
sounded like metal where I was grinding". The spot is on University, around board (258, 64, −257) / (198, 64, −207) /
(145, 64, −168).

**Cause.** Our grind material skipped the tag − 1 step.
- The engine's grind audio surface is the 7-bit collision tag (Grinds+216 = probe surface & 0x7F), the same kind of
  value as the wheel tags.
- Retail's packer `sub_827A1B78` writes every material into the record as tag − 1 (0 or out of 0..143 → 143): the
  wheels at `+464..+476`, and the grind at `+512`. The bridge `sub_824B0DA8` copies `+512` to the audio state's `+692`.
  The grind start `sub_824BB0E0` reads `+692` as material B (143 → 10), and Class_grind / the on / off sounds look it
  up in the AudioSurfaceMap.
- Ours used the tag directly (`skate_events::audio_state`), so every grind read the next entry of the surface map:
  - tag 66 (concrete: material 65, rolling `concrete_rough`, grind surface 1) read entry 66, which is grind surface 4
    (metal: Skate_Metal on / off, the metal GRINDS layer);
  - tag 5 (material 4, grind surface 0) read entry 5 (surface 8, metal);
  - tag 3 read surface 0 instead of 1. Tag 16 (a rail) gave surface 6 either way.
- The map's tag is right. Rolling over the same ledges reads tag 66 too, and the recomp's material at the same
  positions is 65.

**Evidence (the recomp, `audiox_grind_20261003_120315`, 0 malformed, local rider `local72` = 1).** The user rode the
same route from the PCU library to the spot. Grind start COLLPOST `824BB0E0` material B (= `+692`) and position, against
our grind tags at the same places:

| retail material B | positions (x, z) | our tag there | GRINDS samples, retail | Skate_Metal voices, retail |
|---|---|---|---|---|
| 65 | (255, −257), (198, −206), (150, −213), (259, −239…−189) | 66 | 13–23 | 0 in 17 grinds |
| 2 | (149, −390), (266, −246), (230, −232) | 3 | 13–23 | 0 |
| 4 | (175, −116) | 5 | 4–8, 14 | 0 |
| 15 | (237, −413), (84, −403) | 16 | 55–65 | 5–17 per grind |

Every retail value is our tag − 1. Retail also never reads 66 or 3 here.

Our replay of the session's tag-66 grinds (e2e, `E2E_FPS=60`):
- before: GRINDS 55–65 (the metal layer) plus 3–11 Skate_Metal voices per grind;
- after: GRINDS 13–23, no Skate_Metal, as retail;
- tag 16 rails: unchanged (GRINDS 55–65 plus Skate_Metal).

**Change.**
- `skate_events::audio_state`: `grind_material = material_of_tag(Grinds+216)`, as the wheels, the deck and the feet
  already do.
- The state log keeps the engine's tag in `grind_tag`, so the logs don't change. `e2e` converts it the same way.
- `AudioState::grind_material` doc updated.
- No off switch: this is the retail mechanism, and the old path read the wrong table entry.

**Bench** (local script `bench.sh`: the 13 scenarios plus 5 sessions incl. 115334, before / after):
- Changed: grind_ledge, grind_metal, real_115334, real_215843, real_224747. These are exactly the inputs with grinds.
- All other outputs are byte-identical.
- In the changed ones, nothing differs before the first grind. Within 1 s after a grind, the differences are the
  grind's own voices.
- Later differences are Splice voices only (Skate_Collisions / sk8_foley member picks). The shared CRT rand stream
  moves when a grind's on / off record has a different member count. No AEMS or grain voice differs after a grind.
- The bench's `grind_metal` scenario (tag 9) used to render a concrete grind (entry 9 = surface 1). It now plays
  metal (entry 8 = surface 4), as `player_audio`'s tests (which already used `material_of_tag(9)`) intended.

**Verification.**
- `cargo test -p skate-audio --locked`: all pass (lib 225).
- game_audio tests: 57 pass, 4 ignored.
- `cargo build --locked`: OK, with the 3 known warnings.
- Headless only (own target folder); bin\ and data\ untouched.
- Tools: local scripts `bench.sh`, `ours.py`, `retail.py`.

**For the user in game.** Concrete ledges and curbs (e.g. that University spot) grind with the concrete GRINDS layer
and the Skate_Collisions on / off sounds, no metal ring. Metal rails still ring. Some other concrete / stone grinds
change surface too (tag 3: surface 0 → 1; tag 5 was metal, now concrete).

**Listening (user, 2026-10-03, couch launcher, bin\ with the turn / brake console cadence and the grind tag - 1 fix; session `state_20261003_135706`, 0 malformed; asked about the University concrete ledge, a metal rail, carving, braking, enclosed spots (reverb) and fast riding above 46 km/h):** "all of it sounded really good"

### Riding body-collision posts settled (2026-10-03, recomp session `audiox_ride_20261003_141046`, 0 malformed)

User ride (~4.5 min, 2 bails). Outside the bails (+3 s): body-poster posts from NPC skaters 163 (0.70/s, local72 0), from the local rider 49 (0.21/s), and the local ones are only three bursts, no steady riding posts: 45.9 s (4), 172.7 s (19; screenshot: landing a 7.5 ft stair gap) and 202.6 s (26; pushing into a parked car, vehicle material 36). So steady riding gives 0 local body posts in retail, as in ours; the earlier ~1.7/s was NPC skaters plus such events. Follow-up: check that ours posts body hits on a big-drop landing and on bumping a car (a state-log session with both).

### PR #32 review fixes, step 1: the host clock is the physics steps; guards, tests, cleanups (2026-10-03, headless)

**Problem (code review of PR #32).** `skate_events::observe` runs once per physics step (FixedUpdate, Virtual
time, 16.6666 ms) and overwrote `Cues.riding` each step. `native::mixmap_frame` (Update) ran its own `Time<Real>`
accumulator (`mix_clock += delta`, ticks = mix_clock / (1/60), cap 4) and ran inputs / process / update on whatever
sample was in `Cues`. So the two grids drifted:
- a frame with 2 physics steps and 1 mix tick lost the first step's one-step pulse (`+335`, the push plant's rise);
- a frame with 1 mix tick and 0 steps re-processed a stale sample;
- while the menu paused `Time<Virtual>`, `mixmap_frame` and the grain bed kept ticking on the stale sample;
- below 60 fps the components saw every second step (still true, see below: that is retail's behaviour too).

**Root cause.** Two clocks for one stream of samples. Retail's audio manager (`sub_82485190`) reads the record the
game steps wrote; it never runs on a sample twice and never on a frame the game didn't step.

**Change.**
- `skate_events::Cues` counts the steps published since the host last took them (`Cues::publish` /
  `take_steps`). While steps are pending, the one-step pulses of the new step are OR-ed with the pending one
  (`latch_pulses`: `push_trigger` = `+335`). Every other field is a level (the latest step's value is what retail's
  per-frame manager reads too), a latch (`+228`, `+468`, `+192` / `+692`), a 4-frame max (`+668`, `+496..`) or a
  counter (`Riding::pushes`); the components find their own edges (landing, bail, grind) against the last sample
  they processed, so those survive a 2-step frame without latching.
- `native::HostClock::pass` (used by `mixmap_frame` and the e2e harness): takes the frame's steps. No step, or the
  game silenced (menu, replay, the multiplayer menu that doesn't pause): no pass (no inputs, process, tick or
  update; a silenced frame drops its steps and pulses, as the stream is paused). Otherwise ticks = steps, at most
  `MAX_STEPS_PER_FRAME` = 4; the console cadence advances by those ticks.
- **The cap (4 steps = 2 console evaluations).** Retail runs its audio manager once per rendered frame and never
  catches up: a long frame is one call with a long dt. Our host counts steps to stay on the console's 30 Hz grid at any
  frame rate, so it bounds the catch-up instead. 4 keeps frame-rate independence down to 15 fps (the old accumulator's
  cap) while a hitch (Bevy runs up to 15 physics steps after a 250 ms frame) can't release a burst of evaluations,
  Jitter steps or poster calls. The pulses of the dropped steps stay latched.
- `Native::frame_ticks`: the grain bed (`grain_bed::update`) runs only on frames with a pass, dt = ticks × 1/60 (as
  the e2e harness always did), not every rendered frame on `Time<Real>`. Pause no longer advances its envelopes.
- Class_Seams' console cadence (`seam_frame`) still runs on every rendered frame (its own 30 Hz grid), now on game
  time (`Time<Virtual>`, still while paused) and not while silenced. The rendered board's interpolation pair is the
  last two physics steps (`Riding::wheels_before`, `PlayerAudio::step_wheels`), so a frame that takes two steps
  interpolates between the right two.
- Pause.in0 is no longer written as 32767: nothing ticks while silenced, so no evaluation would read it.
- **Camera cuts.** `Presentation::cuts` counts the engine's snaps (teleport flag, camera discontinuity, cadence
  change). On a change, or a map change (`CurrentMap::generation`), `mixmap_frame` resets the listener's last camera
  (`PlayerAudio::reset_listener`: no Doppler velocity from the jump) and the seam pair (no interpolation from the old
  place), and counts `Native::cuts`; the world / NPC hosts drop their camera velocity on a change of it.

**Evidence / proof.**
- e2e bench (`tools/audio-bench/e2e_bench.sh`, 13 scenarios + 4 whole sessions; baseline built from `git archive
  HEAD` in a separate folder): `row`, `fps60`, `fps144`, `fps300` byte-identical to the baseline (audio and voice
  logs). `fps30` / `fps45` differ, as intended. Attribution: with the latch and the step pair both disabled in a
  throw-away build, fps30 / fps45 are byte-identical to the baseline too, so these two are the only differences.
  - The latch: in the 4 real sessions, 16 of 34 logged push edges fell on a row without a pass at 30 fps and were
    dropped before; now each reaches one pass (also `ollies_log`'s one push).
  - The step pair: at < 60 fps the old change-tracking pair spanned two steps.
  - The e2e harness already modelled the ideal host at ≥ 60 fps (one pass per row); it now uses `HostClock` and
    `Cues` itself, so the harness and the game share the scheduling code.
- Unit test `native::tests::the_host_clock_takes_every_physics_step_once`: a Bevy app with `TimePlugin`, the physics
  period and `TimeUpdateStrategy::ManualDuration` frame times (alternating 1 / 2 steps, frames without a step, 30 fps,
  a 300 ms hitch = 15 steps, a paused second, a silenced stretch that still steps). Every step is taken by exactly one
  frame; a pass sees a pulse iff one of its steps had one (the hitch frame merges three); ticks = steps capped at 4;
  no pass while paused / silenced or without a step; console evaluations = ticks / 2.

**Other review items in this step.**
- Removed `trace-downtown-lag.json` (now ignored as `/trace-*.json`, `TRACE_PLAY.bat` writes it) and
  `tools/audio_audition.py` (the audition pages are gone; the "Dev tools" and "Files" entries above are updated).
- **No fallback, for real.** `emitters.rs` no longer plays the measured `PROFILES` table through Bevy voices when the
  native runtime is absent: without it the emitters are silent (the runtime's start logs why). Removed with it: the
  relay / loop patterns, the shuffle bag, `Library::sample_seconds` and the manifest's `seconds` field read.
  `native::follow_volume`: Bevy voices always play at 1 / `RETAIL_SCALE` with the native fold and swapped ears (before,
  an install without the runtime kept ×2 and Bevy's mirrored panning). Identical with the runtime.
- Data-gated tests: 68 tests in `skate-audio` and `game_audio` (96 skip sites, incl. the world / NPC ones) are
  `#[ignore = "needs the private install data"]`, and a missing piece now panics ("missing private data: …") instead
  of passing silently. Run them with `-- --ignored` (or `--include-ignored`). On this machine every ignored test passes
  (skate-audio 51, game_audio 21, older diagnostics included). The wall-clock assertion in the prefetch test is gone (timings are printed).
- `AUDIO_NATIVE post` / `release` lines (one per component post, under the runtime lock) only with
  `SKATE_AUDIO_TRACE=1`. No tool reads them.
- Guards: `formats::abk` template end is a checked add; `Runtime::fill_stereo` turns non-finite samples into 0 for
  the device (host-side safety, not a parity change: finite samples pass untouched; e2e reads the bus before it).
- Allocations: the evaluator reuses unloaded bank slots (map changes no longer grow the list); `Evaluator::walk`
  reuses its order snapshot; `redeliver` / `release` read their client lists in place; `destroy` reads the module's
  object lists through the bank's `Arc` (it copied them). `grain_bed::update` no longer clones the seam wobbles and
  builds a `PlayerTuning` every frame. `tests/render_alloc.rs` gains a hand-built AEMS bank (`eval::synthetic`, the
  evaluator tests' builder made public and hidden from docs) with a playing program, a redeliver every console frame
  and a release: 0 allocations in 480 warm blocks; the allocation counter is per thread now. All e2e outputs of
  these changes are byte-identical (bench `clk_final` vs `clk_new`, every mode).
- clippy `approx_constant` errors in the `dsp::pan` tests (`FRAC_1_SQRT_2`); 0 clippy errors in `skate-audio`.
- `.gitignore`: `*.wav` is scoped to the folders retail audio is written to (`/assets/private`, `/data`,
  `/maps/private`, `/.local`); no untracked WAV became visible. Mods and the SDK may ship WAVs.
- Credit: the FSS `sin` / `cos` polynomials are XNA Math's `XMVectorSin` / `XMVectorCos` (successor DirectXMath,
  Microsoft, MIT); see Credits.

**Files.** `game_audio/{skate_events,native,grain_bed,player_audio,e2e,emitters,library,world_sources,npc_skaters}.rs`,
`game_audio/native/prefetch.rs`, `presentation.rs` (the cut counter), `skate-audio/src/{eval/mod.rs,eval/synthetic.rs,
eval/tests.rs,formats/abk.rs,runtime.rs,dsp/pan.rs}`, `skate-audio/tests/render_alloc.rs`, every data-gated test file,
`skate-audio-fma/src/lib.rs`, `.gitignore`.

**Open questions.**
- Slow motion: the physics timer period changes (`physics/clock.rs`), so the host now ticks slower in real time with
  it (it followed real time before). What retail's audio manager does in slow motion is not traced (`+220` time
  scale is written 1.0).
- The world / NPC hosts keep NodeIds across `unload_map_banks` (not fixed here; the world build-out step).
- Needs the user's listening check at a low or uneven frame rate (pushes) and after a menu pause.

### World audio hook-in, phases P0–P2: the map-change fix, the engine / mod surface, the dev test mod (2026-10-03, headless)

Full write-up, field tables and examples: [doc 15](15-world-audio.md). Design: `audio-specs/world-audio-hookin-spec.md`.

- **P0, map change.** `Native::unload_map_banks` destroyed every instance of the 13 world banks, but `WorldHost`
  kept the dead nodes and "already posted" objects, so an owner that survived the change was silent for the rest of
  its life. Now `Native::map_epoch` is bumped by the unload. On a change, `WorldHost` / `NpcHost` release every node,
  clear the pools / records, deactivate the 3DObjPos blocks, stop the ped Splice steps and the NPC bed, and drop
  their objects; the owners still published are claimed and posted afresh. The evaluation count is a
  `saturating_sub`. The world hosts' `dt` is now 1/30 per console evaluation and 1/60 per tick with
  `SKATE_AEMS_MIX_CONSOLE=0` (it was `CONSOLE_DT` either way, double the real time in the non-console mode; that mode
  went with the A/B switches, below, so it is 1/30 now).
  Regression test `world_owners_post_again_after_a_map_change`, which fails without the reset.
- **Retail limits from the gap runs** (spec §7.3): traffic list cut at 40 m horizontal (4 nearest); ped list cut at
  50 m (15 nearest); ped footsteps for the 3 nearest (`S+68`); `S+148` / `S+156` = the distance / 20 m. Applied by
  the hosts (radii) and the bridge (footsteps-on, the pair).
- **P1, the engine surface** `crate::world_audio`: `TrafficAudio`, `PedAudio`, `NpcSkaterAudio`, `AudioVelocity`,
  the read-back `WorldAudioInstance`, `LivingWorldAudio`, `WorldAudioStats`, the messages `PedSpeechEvent` /
  `VehicleHorn` / `VehicleAlarm` (8 s). Bridge: `game_audio/world_bridge.rs`.
  - The per-skater builder `skate_events::skater_audio_state` + `SkaterAudioMemory` is a pure move of `observe`'s
    code. 1,500 production physics steps (`audio_state_capture`) gave a published state identical line for line
    to the pre-refactor capture, and the builder equals `observe` on every step.
  - `AudioState::rolling(&LiteSkater)` is the documented minimal fill for skaters not simulated with the player's
    physics.
- **User decisions included** (2026-10-03):
  - mod cars may opt in to retail traffic engine sounds (`body=`);
  - remote multiplayer players take the NPC skater instance, first in the list (non-retail; a lite state with the
    ground's material);
  - opt-in non-retail "more audible" (`settings/audio.json` `"more_audible_world": true`: 8 / 24 / 3 instances;
    the MixMap is built with them; instance 0 unchanged);
  - no traffic-light sounds.
- **Mods:** `sdk.world_audio.spawn / update / event / remove / read / info` (capability `world_audio` = 1). 16
  objects per mod, 64 in all; an object is parked after 0.5 s without updates; everything is cleaned up on disable,
  reload or failure. The existing `sdk.audio.*` is unchanged.
- **P2, the dev test publisher** `mods/world-audio-test/`:
  - 16 cars (c00–c08 records, honks, skids, F8 alarm);
  - 20 peds (walk / jog / run, warn / cheer / slam);
  - a ghost NPC skater replaying a window of one of the user's state logs (`logs/<name>.tsv` or
    `SKATE_AUDIO_STATE_LOGS`);
  - debug boxes by audibility.

  The e2e state-log replay moved unchanged from `e2e.rs` to `game_audio/state_replay.rs` for the ghost.
- **Proof for the local player:** the e2e bench is byte-identical before / after (13 scenarios + 4 sessions, row and
  fps300). Tests: the bridge unit tests, the ghost through the real host (claim, release at range, a map change), and
  the skate-mods validation plus a 400-frame run of the test mod.
- **Learned for the next pass (P3), from G3:** for an NPC, Wheels runs (layers 0 / 1) and Clothing runs; Tricks and
  Treatment are local only; OffBoard creates packets but plays no steps. Instance 1 changes hands often (12× in
  74 s).

### A/B switches removed (2026-10-03, user decision; separate change after the world audio P0–P2)

**Decision (the user):** remove the 15 verdict-settled A/B env switches. Their ON behaviour becomes the only
behaviour.

**Removed:**
- `SKATE_AEMS_SEAM_PULSE`, `_MIX_CONSOLE`, `_PUSH_PLANT`, `_PLANT_LIFT`, `_BODY_IMPACTS`, `_GRIND_ONOFF`,
  `_AIR_TIME_STATE`, `_SUBMIX`, `_FOOTSTEP_SUBMIX`, `_REVERB_INPUTS`, `_REVERB_ZONES`, `_BODY_CONSOLE`,
  `_DECK_CONSOLE`, `_BODY_CURVE`, `_SLEW_CONSOLE`;
- their e2e equivalents: `E2E_MIX_CONSOLE`, `_BODY_CONSOLE`, `_DECK_CONSOLE`, `_BODY_CURVE`, `_PLANT_LIFT`,
  `_BODY_IMPACTS`, `_GRIND_ONOFF`, `_SLEW_CONSOLE`, `_FOOTSTEP_SUBMIX`, `_REVERB_INPUTS`, `_PUSH_PLANT`,
  `_AIR_TIME_STATE`;
- the dead comment-only names `SKATE_AEMS_PLAYER` / `SKATE_AEMS_FOOTSTEPS`.

**Kept:** `SKATE_AEMS_WORLD`, `_WORLD_PREFETCH`, `_NPC_SKATERS`, `SKATE_AUDIO_FMA`, the tools (`SKATE_AUDIO_TRACE` /
`TIMING` / `STATE_LOG` / `SET`, `SKATE_AEMS_BANKS`), and the e2e harness's part switches (`E2E_ROLLING`,
`E2E_CONTACTS`, `E2E_BUSES`, …).

**Dead code removed with them (game crate):**
- the old 60 Hz MixMap evaluation (`HostClock::pass` has no `console` parameter; every pass is on the console
  grid, the eEQChain clear included);
- the per-tick Class_Seams process (`PlayerAudio::seam_console`; the seams run in `seam_frame`);
- the fixed Reverb.in5 path;
- the zone-less reverb;
- the per-call body / deck posters (`PlayerAudio::body_console` / `deck_console`);
- the dt-scaled bed slews (`Bed::slew_console`);
- the air-flag `+236` count and the animation-contact push plant (`air_time_236` / `push_plant` lost their switch
  parameters);
- `set_review_ports` / `set_body_curve`;
- the e2e harness's non-console row mode.

**Kept on purpose:** the `skate-audio` components' own on / off fields (`Contacts::plant_lift_on` / `body_on` /
`body_speed_on`, `Grind::onoff`, `CollisionManager::submix`, `Option` body / deck / slew calls). The game always
turns them on. Their unit tests use "off" to isolate one part, so these are component configuration, not switches.
Their docs no longer name env vars.

**The e2e `row` mode is now the game's host at 60 fps.** Before, it ran the removed old 60 Hz evaluation and the
per-tick seams.

**Proof (byte-identical):**
- e2e bench before (`wa_new` row / fps300, `sw_base` fps30 / fps60 / fps144) vs after (`sw_new`, all five
  modes): fps30, fps60, fps144 and fps300 IDENTICAL for the 13 scenarios and the 4 whole sessions (26 + 8 outputs
  each);
- the new `row` renders are identical to the old `fps60` renders, which is what the game ran at 60 fps;
- `audio_state_capture` (1,500 production physics steps through `observe`) is identical to its capture from before
  both changes;
- every unit and data test passes.
- two data tests (`seams_play_their_retail_bank`, `seam_hits_shuffle_their_samples`) drove the removed per-tick seams.
  They now call `seam_frame` before each pass, as `mixmap_frame` does, and pass with the same picture (pattern 11
  at 10 / 20 / 30 km/h: 39 / 67 / 93 voices; brick: all 16 samples).

### World audio P3–P5: ped speech plays, the NPC instance's wheels / clothing / grunt, traffic, retail's order (2026-10-03, headless)

Full write-up: [15](15-world-audio.md), section "P3–P5". In short:

- **Ped speech plays.** It goes through the speech manager, the library and the living world's two streams
  (interrupt by tuning `+13` / `+14`, a 16-request queue, the 200 / 60-frame cut).
  - The stream values come from the speaker's PedestrianSpeech outputs, read from the code (`sub_824D9370`):
    main level out2, or out3 for `_f` clips; reverb send out15; high pass out13; low pass out14; pan out0;
    pitch out1.
  - Recomp, 39 lines rebuilt at their geometry: the filters match exactly, and gain recomp / ours has median
    1.005.
  - The full free-roam decode is in the dev install. Without it, speech stays silent and logged.
- **The NPC skater's instance** now runs Wheels (layers 0 / 1) and Clothing, and raises the bail grunt (event
  8206 for its voice, once per bail). Tricks, Treatment and footsteps stay local only, as recomp gap run G3
  showed.
- **Per-model ped data** (`world_tuning.ped_models` from `aud_characteristics`): 3146 of 3146 recomp PEDAUD
  lines agree.
- **Traffic.**
  - Model → engine record export; `TrafficCarPhysics.in0`; front / rear 3DObjPos points.
  - Against gap run G1: RPM within 60 in 97.3 % of sample pairs, `+176` within 0.5 m/s in 99.2 %.
- **Retail's order.** The world / NPC hosts now process before the MixMap ticks and update after them
  (`native::mixmap_frame` / `mixmap_tick`). Before, both ran after the ticks. A test proves this is exactly a
  one-evaluation shift (89 of 89).
- **The local player's output is byte-identical:** e2e 13 scenarios + 4 sessions, `row` and `fps300`, against a
  worktree of `a4ec831`.
- **Gaps:**
  - `SFXObj_Tazer`: the recordings support it, but the object is not decoded;
  - PedBodyFall: no recorded post;
  - speech: the PEAK filter, the SEND for most lines, the `Obj:Speech` inputs, which stream a request takes;
  - the traffic 3DObjPos binding's writer;
  - the NPC's board slide and on-foot voices.
- **The dev test mod is opt-in** (`"enabled_by_default": false`, `SKATE3_MODS_ENABLE`). The mod limits are now
  48 / 128, and a `WORLD_AUDIO` summary is logged once a second.

### World audio gaps closed: Tazer, PedBodyFall, speech details, the NPC board slide (2026-10-03, headless + one scripted run)

Full write-up: [15](15-world-audio.md), section "Gaps closed". In short:

- **Tazer** (`SFXObj_Tazer` decoded): `c_tazer` held while the ped tazes. Recomp 164620: 19 starts per 2 s hold
  against 10 / 20 / 19, the same gaps (192 / 192 / 128 / 96 ms vs 190 / 190 / 130 / 90–100) and order, first-start
  gain recomp / ours 0.85–1.00.
- **PedBodyFall** (decoded; trigger = the animation's `BodyFallType`): 75 recorded starts (Splice, not POST);
  containers 73 / 73 by type; voice gain recomp / ours p50 0.79.
- **Speech:** the stream voice's PEAK = azimuth curves (6 / 6 recomp pairs on them); the per-voice float; the
  pre-gain send (out21) feeds the slot's **echo submix** (ported: HPF → camera-distance delay → LPF → env) and the
  post-filter send (out15) goes to the env bus (17 / 29 and 19 / 27 recomp lines); value 49 = the phone ring → 64,
  value 29 = the photographer's 1 s repeat, `Obj:Speech` in0 / in1 / in4, first-free stream; the queue clock =
  visual game ticks.
- **Main cast (pros, special cast):** its own channel (`maincastspeech.big`, decode 6596 takes / 922 MB, opt-in
  with the living world's), pro peds, NPC skater reactions and crash (`skater_speech`), the message pairs; all 82
  recorded main-cast lines reachable through the ported words.
- **NPC:** the board slide runs for the NPC instance (static evidence; a 4-min scripted run saw no NPC bail); the
  NPC bed against 180430's NPC rows: near band A gain 0.097 / 0.108.
- Local player byte-identical (e2e bench, all sets).

### PR #32 review fixes, step 2 (code half): spec references and private-data paths (2026-10-03, headless)

**Problem.** Code comments in `crates/`, `tools/asset_pipeline/` and the dev test mod cited the working notes and
local tools by their private paths (the gitignored working-notes folder, the local skill tools), which a reader of
the PR cannot open. Data-gated tests found their research data through hard-coded paths into the gitignored work
folder of this checkout.

**Change (comments only, except the test paths below).**
- Spec references → the published specs: `audio-specs/<name>.md` everywhere (= `docs/hails-additions/audio-specs/`;
  `skate-audio/src/lib.rs` says so once), the `§n` anchors kept and checked against the published copies. 53 full
  paths (46 in `crates/`, 6 in `tools/asset_pipeline/`, 1 in `mods/world-audio-test`) and 21 bare names (`notes
  ems-emitters-re.md` → `audio-specs/ems-emitters-re.md`, …). The one anchor that was not a heading
  (`world-speech.md` "Speech manager gate") now names its section ("Mechanism", "The request").
- Tool references → the published tool (`tools/audio-file-inspect/{splc_fields,bank_layout_check,
  decode_bank_samples}.py`, `tools/recomp-trace/{retail_voices,grec_material}.py`, `tools/audio-bench/`), setup's
  `audio` group (`SKATE_SETUP_SPEECH=1` for the speech export), or "a local tool, not published" (the PoC probes,
  `mxb_tool.py` with the game's name table, `golden_compare.py`, `mixmap_compare.py`, `speech_levels.py`,
  `sos_figures.py`, `instance1_voices.py`): 15 places. Doc 15's file list: the same.
- Local data in comments → described ("the PoC's golden vectors, kept locally", "local data", the env var).

**Testing notes: where the data-gated tests find the private data.** No default paths any more: every kind has an
environment variable, and with nothing set the (ignored) test fails with "missing private data: …", as before.
`skate-audio`'s integration tests share the lookup in `tests/private_data/mod.rs`.

| Variable | Points at | Used by |
|---|---|---|
| `SKATE_AUDIO_RE_DIR` | the extracted research data root (`aems-banks/`, `splc-banks/`, `bank-wavs/`, `tricks/`, `golden/dsp/vectors.txt`); fills in the next four when they are unset | `skate-audio` tests |
| `SKATE_AEMS_BANKS` | the disc's `.abk` / `.csi` (`bank_layout_check.py --extract`) | `disc_banks`, `rolling_banks`, `offboard_levels`, `world_sources` |
| `SKATE_SPLC_BANKS` | the disc's SPLC `.bnk` files | `splice::tests`, `offboard_levels`, `world_sources` |
| `SKATE_DSP_VECTORS` | the PoC's DSP oracle vectors file | `dsp_oracle` |
| `SKATE_TU3_IMAGE` | the dumped TU3 image `default_82000000_011B0000.bin` | `mixmap::tables` |
| `SKATE3_DISC` | the extracted disc (as the published tools); the MixMap when the install has none | `mixmap::tests` |
| `SKATE_RECOMP_SESSIONS` | the recomp session folders (`<session>/trace.tsv`); `TREAT_SESSION` / `GREC_SESSION` still pick one session | `player_tricks`, `skate_events`, `game_audio::world_sources` |
| `SKATE_AUDIO_STATE_LOGS` | the audio state logs (as the dev test mod) | `npc_skaters` |
| `SKATE_SPEECH_LEVELS` | the speech level export (`levels_<session>.json`) | `world_speech` |

The tools' own default work folders (doc 14) are unchanged.

**Verification.** `cargo build --locked`. `cargo test -p skate-audio -p skate-audio-fma --locked`: all pass; with the
variables set to this machine's data, `-- --ignored`: 51 of 51 pass; with none set, every data test fails loudly
("missing private data"). `cargo test -p skate-game --release --bin skate3rust --locked -- game_audio::`: 43 pass,
28 ignored; `-- --ignored`: 28 of 28 pass. `cargo test -p skate-mods --locked`: pass except the known
`skyline_physics` (GLB not in this checkout). e2e bench (13 scenarios + 8 sessions, `row` and `fps300`):
byte-identical to the run before the change.

**Files.** Comments: `skate-audio/{src,examples,tests}` (bus, dsp, eval, formats, grain, mixer, mixmap, player,
splice, world, lib.rs), `skate-game/src/{game_audio/*,world_audio.rs}`, `tools/asset_pipeline/{audio_export,
world_audio}.py`, `mods/world-audio-test/main.lua`, doc 15. Test paths: `skate-audio/src/{mixmap/tables.rs,
mixmap/tests.rs,splice/tests.rs}`, `skate-audio/tests/{private_data/mod.rs (new),disc_banks,dsp_oracle,
offboard_levels,player_tricks,rolling_banks,world_sources}.rs`, `skate-game/src/game_audio/{skate_events,
world_sources,world_speech}.rs`, `game_audio/npc_skaters/tests.rs`.

### Optimisation pass 2: render load readout, an allocation-free render, cheaper voices (2026-10-03, headless)

**Problem.** The second pass for PR #32 starts from the baseline taken on a4ec831. That baseline measured
the render at about 100 µs per block in real play against 58 µs headless. The render allocated 0.2 to 0.6
times per block in real play, about three times per walk, and 1.8 times per block with the world hosts
running. A full world (about 85 voices) roughly tripled the render. The rule for the pass: byte-identical
output, measured before and after, and an adversarial review of every change.

**Identity reference.** `p2base` (a4ec831) and `p2pre` (the tree before this pass) are byte-identical to each
other. Every state of the pass was compared with them: 13 e2e scenarios + 8 user sessions, `row` and
`fps300`, 84 outputs per run.

The data-gated world / NPC / speech tests were also run before and after:
- `cargo test -p skate-audio -- --ignored`;
- `-p skate-game … -- --ignored game_audio::world_sources / npc_skaters / world_speech / world_bridge`.

Their printed tables were diffed; only the elapsed-time lines differ.

**Timing method.** Other agents built and ran on the machine during the pass, so the timing of a whole bench
run was unusable (one reference run came out +30 % p50 with identical output). Before / after numbers come
from saved test binaries run alternately (A B A B …, 3–4 rounds):
- two whole sessions (`real_143434`, `real_115334`, 180,638 blocks) in `row` mode;
- the full-world host bench: 64 vehicles + 64 peds, full pools, 88.5 voices and 39 instances per block.

All timing runs used one local measurement patch on both sides. The patch is a counting allocator plus
per-block walk / voices / allocation stats in the e2e harness and a timed world-host test, and it was never
landed. Rounds that another agent's build disturbed show up as one side jumping by 15–40 %. They are named
below and not counted.

| # | Change | Result | Kept |
|---|---|---|---|
| 0 | `AUDIO_TIMING` render load per block: `block_voices` (mixer voices), `block_instances` (live AEMS instances), `block_grains` (grain voices), as `name=max/avg×blocks` | output identical; `audio_timing_summary.py` prints them | yes |
| 1 | Render allocations: the subscriber lists read in place (`call_function`, `set_global`); the Player op's input records on the stack; ControlClass / CallFunction parameters in a 255-word stack buffer (the readers' counts are u8); post reads the constructor list in place; instance memory and node client lists recycled through pools (best fit) | render allocations per block 0.17–0.63 → ≤ 0.001 in the 8 sessions (the rest is pool growth, 0–8 per whole session); world bench 1.825 → 0.004; game thread −0.15 per frame; time neutral | yes |
| 2 | Evaluator walk: the self-contained opcodes straight to `ops::run` with one instance lookup for the op and its copy pairs, the bank's `Arc` instead of the program's | no measurable gain: walk − plain block 34.0 / 35.6 µs (before) vs 34.0 / 34.3 µs, the same within run spread | **dropped** |
| 3a | Voice source read: when the block's last frame index (position + ((frac + step·256) >> 16) + 1) is inside the sample and no stop fade runs, the resampler reads the channel slice directly instead of `Voice::sample` | sessions p50 −0.6 / −1.3 / −0.7 µs (3 rounds); world p50 −1 µs | yes |
| 3b | Voice scratch planes: the source and panner planes live in the mixer (12 KiB, allocated with it), not as two zeroed 6 × 256 stack arrays per voice; only the voice's own channels are read, and each is written in full first | world p50 112.4 → 105.6 µs in a clean round (with 3a) | yes |
| 4 | eEQChain silent-bus memo: a silent bus from the state of an earlier silent block that left the state unchanged adds the stored output (state and output keyed on bits) | counted first: 77 % of bus-blocks silent, 38–40 % would hit; then no gain: sessions p50 +0.8 µs, world +1–3 µs (the extra silence scans and key compares cost what the skipped EQs saved) | **dropped** |
| 5 | MixMap `tick`: the held-input save list kept between ticks | −1 allocation per evaluation | yes |
| 6 | `Jitter::process_each` (same draws, same write order; the MixMap writes don't touch the walk) instead of collecting a `Vec` per frame | −1 allocation per frame; with 5: game thread 18.0–18.9 → 16.4–17.2 allocations per frame | yes |
| 7, 8 | first emitter start after a load; per-frame map-name / tuning clones | not touched: cosmetic or ≈ 0, as the baseline said | — |

The rest of item 6 was left out: making every component's `Command { words: Vec<i32> }` allocation-free.
`Command` is public API that the audio modding (doc 16; former #36, now part of #32) builds on, so it needs a fixed-size word array agreed with
that PR. The world host's ~100 allocations per evaluation are the same pattern.

**Counts behind the choices** (local counters, `real_143434` / `real_215843` / `ollies_log`):
- Voice HPF active in 13–25 % of voice renders, LPF in 1–1.4 %, both in 1–1.4 %. Most voices bypass both
  filters, so a fused HPF → LPF cascade kernel (an idea for item 3) would buy nothing and was not written.
- eEQChain: 77–92 % of bus-blocks silent; busy channels about 10 % of all bus channels. The EQs are active on
  72–75 % of bus-blocks, silent ones included.
- No item-5 output restructuring was attempted. `write_outputs` + outputs are integer-only and spread over
  1,044 outputs with no dominant path.

**Before / after (the pass as a whole, pre-pass binary vs the final one, alternated).**
- Sessions, block p50: 61.9 / 62.7 / 60.4 / 59.8 µs → 58.7 / 58.8 / 58.0 / 59.6 µs (mean 66.9–72.5 → 63.8–68.2).
  Rounds 1–2 of the pre-pass side were partly disturbed, so the clean gain is 0.2–2.4 µs p50 (≈ 1–4 %).
- Full world (88.5 voices), clean rounds 3 and 4:
  - render p50 129.4 / 117.8 → 111.7 / 105.2 µs (−11 to −14 %);
  - p90 203.1 / 186.5 → 180.7 / 163.8 µs;
  - p99 350.9 / 310.2 → 314.7 / 200.1 µs;
  - walk blocks p50 189.8 / 179.4 → 171.3 / 165.5 µs.

  Rounds 1 and 2 were each disturbed on one side.
- MixMap tick in the same bench: 51.9 / 44.6 → 43.8 / 38.7 µs p50 (item 5 and a quieter machine). Treat it as
  an indication, not a claim.

**Review (per change; counter-examples tried).**
- **0.** Reads counters only (`Vec::len`, a walk over the grain trucks' slots) under the lock the render
  already holds, after the timed scope. When off: the existing relaxed `OnceLock` check. No state touched.
- **1, subscriber loops.** The loop bodies write instance memory only, and the borrow checker now forbids
  touching a subscriber list inside them. A re-entrant change is impossible, so the order is unchanged.
- **1, 255-word stack buffer.** Every reader of a parameter list stops at a u8 count (≤ 255 words), and a
  shorter list still reads 0 past its end.
- **1, stack input records.** The Player's input records equal the old `Vec`: n is a u8.
- **1, pools.** A recycled buffer is cleared and refilled with the template, so it has the same bytes and
  length as before. Nothing reads capacity.
- **1, pool-related counter-examples.** A pooled client list that was never pushed would allocate again on
  every post; new lists therefore start with room for four. Worst-fit buffer choice kept re-growing; it is
  now best-fit.
- **1, tests.** `render_alloc` has two new tests:
  - a synthetic requester bank (ControlClass children created / destroyed inside walks, CallFunction →
    SetGlobalVariable fan-out, game-side posts / releases);
  - a data-gated world host on the retail traffic / ped banks (4 cars, 15 peds, 27.6 voices per block).

  On the pre-pass evaluator they count 1,179 and 5,061 allocations; now 0 and 0.
- **2.** Exact (same calls and order as `exec`'s fallback arm; no instance can vanish before a
  self-contained op), but not faster. Dropped as "gain not real".
- **3a.** Test `the_resampler_reads_no_frame_past_the_direct_bound`: 2,000+ random steps up to the 4×
  ceiling × 5 phases. The highest index the resampler asks for is exactly position + reach + 1, so the
  condition is tight and sufficient.
- **3a, equivalence.** `Voice::sample` for i < total with no fade is the same `get(i)` on the same channel. A
  short PCM still gives 0 past its end; a missing channel gives an empty slice → 0.
- **3a, test.** `the_direct_source_read_matches_the_sample_lookup` renders looped, ending, released and
  pitched voices with the direct read forced off (a far-future fade) and on, and requires bit-equal output.
- **3b.** Reads of `src` are all bounded to the voice's channels (filters, gain, both mono sums, the panner).
  When started, the resampler writes every sample of each such channel; the silent first block zeroes them.
  The panner zeroes all six outputs before accumulating.
- **3b, threading and allocation.** The scratch lives in `Mixer` behind the runtime lock (no new sharing)
  and is allocated in `Mixer::new` on the game thread.
- **4.** Exact by construction. The memo stored output and state only when a silent block left the bit
  state unchanged, so a hit replays a pure function. Test against a reference with jitter, re-rolls, clip,
  bypass, a NaN frequency and −0.0 on SFX Master. The gain was not real, so it was dropped.
- **5, 6.** Same list contents and order; RNG draws unchanged (6: the MixMap write between draws touches
  neither the generator nor the channels).

No `unsafe` and no new threads were added; float arithmetic is untouched (items 3a / 3b only change where
samples are read from and which planes are zeroed). There is no new hard-coding: the 255 is the format's u8
count, not a tuning value, and no data-driven path changed.

**Verification.** For every kept state (items 0+1, 3, 5+6, and the final clean tree), the e2e bench (`row` +
`fps300`) is IDENTICAL to `p2base` / `p2pre` in all 84 outputs, and the data-gated tests are unchanged
(52 + 10 pass, tables identical; the new world allocation test failed on the pre-pass evaluator, as intended). `cargo test -p skate-audio -p skate-audio-fma --release --locked` pass
(`render_alloc` 3 + 1 data-gated); `cargo test -p skate-game --release --bin skate3rust --locked --
game_audio::`: 43 pass, 28 ignored.

**Files.**
- `skate-audio/src/eval/mod.rs`: item 1 and `instance_count`.
- `skate-audio/src/mixer.rs`: 3a, 3b and their tests.
- `skate-audio/src/mixmap/mod.rs`: item 5.
- `skate-audio/src/player/jitter.rs`: item 6.
- `skate-audio/tests/render_alloc.rs`: two tests and `RENDER_ALLOC_BT=1` backtraces.
- `skate-game/src/game_audio/{timing,native,player_audio}.rs`.
- `tools/audio-bench/audio_timing_summary.py`.

**Open.**
- The walk (≈ 34 µs per walk block in sessions, ≈ 60 µs with the world) is the interpretation itself: about
  2,700 ops, ≈ 13 ns each. Hoisting lookups did not move it. A real gain needs a different program
  representation, which is a bigger change.
- Real-play load numbers need a session on a build with item 0 (`SKATE_AUDIO_TIMING=1`).

### World audio follow-ups: speech inputs, main-cast repeat times and stops, the NPC bed's distance, the NPC slide (2026-10-04, headless)

Full write-up: [15](15-world-audio.md), section "Follow-ups". In short:

- **`Obj:Speech`** in0 / in1 / in4 read the main cast's two streams only (fix: a living-world guard no longer raises
  in1); in2 (scripted dialogue) and in3 (a main-cast stream flag pair) are never raised in free roam. The near
  lines' ~0.6 dB is gone since the voice float (near −22 mB, far +45 mB; the VU meter checked and ruled out).
- **Main cast:** the repeat times of speaker slots 31 / 30 (record `+52` / `+56`, setup + mod tuning); values
  30 / 51 stop the speaker's playing line before the new request; value 29 requests 141 and then 136. The crash's
  "cameraman line" is the announcer's `480_slam_pro` (announcer channel not ported).
- **NPC bed:** the Player slot's level lookups read the camera's distance and azimuth, as the port already did;
  the data test now uses each recomp row's geometry: 10–20 m 0.050 / 0.054, 20–30 m 0.0091 / 0.0111, row median
  0.94 (was 0.045 / 0.065 and 0.005 / 0.014 with the NPC always dead ahead). No engine change.
- **NPC board slide observed** in session 180430 (an NPC bail, two posts, `board_scrapes` 62–68 ms later).
- Local player byte-identical (e2e bench, all sets).

### Optimisation pass 3: equal channels and lockstep filters (2026-10-23, Switch port)

**Problem.** On the Switch the audio thread is busy about a fifth of a core (dev17's console profile, sk8 findings
62 §8), and the game's task threads wait for the runtime lock while a block renders. The biquad kernel was 26 % of
the thread's time, the peaking EQs 19 %: the eEQChain buses run two of them on six channels each. The kernel is
latency-bound: each sample waits on the previous one's two fused multiply-adds, which the A57 takes longer over
than an x86 core.

**Changes (bit-identical by construction and by test).** `dsp::biquad::kernel_channels` runs a filter's channels for
`Iir2`, `PeakingIir2` and `HighShelfIir2`:
1. A channel whose input and history are bit-identical to an earlier channel's gets that channel's output and
   history. `PeakingIir2` did this for silent channels only, so on a bus nothing plays into the second EQ, which
   sees the first's bias response on all six channels, ran the kernel six times; it now runs it once.
2. The channels `kernel_block` would hand straight to the kernel (blocks that aren't 8-periodic) run in lockstep,
   two to four of one length at a time (`skate_audio_fma::biquad2`, `biquad3`, `biquad4`, plain and FMA copies as
   in "Hardware FMA dispatch"). Each channel's arithmetic and its order are the kernel's; the channels' feedback
   chains overlap.

**Proof.**
- `skate_audio_fma` `biquad_lanes_match_biquad`: each lane against `biquad`, 2, 3 and 4 lanes, stable and wild
  coefficients and histories, on both paths.
- `dsp::biquad::kernel_channels_matches_the_plain_kernel`: every coefficient set, one to eight channels, twins of
  input and history (or of only one), mixed lengths, silent / -0.0 / constant / noise blocks, long enough to settle.
- `dsp::peaking::a_bus_chain_matches_the_plain_kernel`: a bus's two EQs in series, idle, a voice over the six
  channels (two pairs at one level), idle again, the parameters changing every 12 blocks.
- A NaN comes out where the kernel puts one; on the host its sign and payload matched too (0 differences).
- `tests/render_speed.rs` (ignored; `--release -- --ignored --nocapture`): two synthetic scenes with the install's
  eight eEQChain records, the reverb network, the FlangeSub returns and a grain truck. Their output hashes are the
  same before and after.

**Speed (host, x86-64 with hardware FMA, µs a block, mean / p50).** 24 voices over the eight buses: 294.6 / 272.6
before, 272.6 / 251.1 after. 8 voices on bus 0: 155.6 / 141.1 before, 140.5 / 131.5 after. The console figure is
the audio thread's share in the next CPU profile.
