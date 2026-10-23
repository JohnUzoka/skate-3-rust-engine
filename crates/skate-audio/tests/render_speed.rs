//! The render's speed and its output bits on two synthetic scenes, for optimisation passes that
//! must not change a bit (doc 11 "Optimisation pass 3"): run it before and after a change, and the
//! hashes must match. Unlike `tools/audio-bench` it needs neither the game crate nor play logs.
//!
//!   cargo test -p skate-audio --release --test render_speed -- --ignored --nocapture
//!
//! Both scenes have the install's eight eEQChain records (buses 0-4 re-roll, 5-7 take the jitter
//! walk, posted every 12 blocks as the game's clear runs every second frame), the reverb network on
//! a preset, the FlangeSub returns and a bound grain truck. `busy` routes 24 voices over the eight
//! buses; `idle` routes 8 voices into bus 0, so the other buses filter silence.
use std::sync::Arc;
use std::time::Instant;

use skate_audio::bus::eqchain::EqRecord;
use skate_audio::bus::env::{DEFAULT_PRESET, Preset};
use skate_audio::bus::flange::FlangePreset;
use skate_audio::eval::{OpenRequest, VoiceHost};
use skate_audio::formats::SampleHeader;
use skate_audio::grain::player::{GrainParams, GrainSource, Record};
use skate_audio::mixer::Pcm;
use skate_audio::runtime::Runtime;

fn reverb01() -> Preset {
    Preset([
        4000.0, 600.0, 1.0, 0.08, 0.0, 1.5, 70.0, 1.0, 0.7, 4000.0, 500.0, 1.0, 1.0, 0.25, 1.0, 1.0, 3.0, 0.1, 0.34, 1.0, 1161.0, 0.5,
        0.2, 0.6, 1.0, 1.0, 270.0, 5000.0, 0.5, 0.45, 500.0, 0.75, 0.3, 0.3, 1.0, 1.0, 90.0, 5000.0, 0.51, 0.5, 500.0, 0.75, 0.28, 0.25,
    ])
}

/// The install's `bus_tuning.eq_buses` (audio_manifest.json).
fn eq_records() -> [EqRecord; 8] {
    let enabled = |clip: f32, ranges: [[f32; 2]; 6]| EqRecord { enabled: true, clip, ranges };
    let jittered = EqRecord {
        enabled: false,
        clip: 100.0,
        ranges: [[10000.0, 5000.0], [1.2, 0.5], [4.0, 2.0], [20000.0, 15000.0], [1.2, 0.5], [4.0, 2.0]],
    };
    [
        enabled(100.0, [[450.0, 150.0], [1.5, 0.75], [3.0, 0.25], [5000.0, 1500.0], [1.25, 0.9], [2.0, 0.25]]),
        enabled(100.0, [[500.0, 150.0], [1.25, 0.75], [1.5, 0.25], [7500.0, 1000.0], [2.5, 0.25], [1.5, 0.25]]),
        enabled(100.0, [[350.0, 150.0], [1.5, 0.25], [1.5, 0.2], [6000.0, 1500.0], [2.0, 0.1], [1.5, 0.2]]),
        enabled(0.1, [[2000.0, 100.0], [1.0, 1.0], [3.0, 1.0], [8000.0, 1500.0], [1.0, 1.0], [5.0, 1.5]]),
        enabled(0.07, [[700.0, 100.0], [1.2, 0.75], [5.0, 2.0], [5000.0, 2000.0], [1.2, 0.5], [3.0, 1.0]]),
        jittered,
        jittered,
        jittered,
    ]
}

fn scene(voices: usize, buses: usize) -> (Runtime, Vec<u32>) {
    let mut rt = Runtime::new();
    let frames = 44_100 * 4;
    let wave: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.03).sin() * 0.3).collect();
    let mono = Arc::new(Pcm { rate: 44_100, channels: vec![wave.clone()] });
    let other: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.071).sin() * 0.2).collect();
    let stereo = Arc::new(Pcm { rate: 44_100, channels: vec![wave.clone(), other] });
    let header = |channels| SampleHeader { codec: 3, channels, rate: 44_100, frames: frames as u32, loop_start: Some(0) };
    rt.mixer.add_bank(0, vec![Some(header(1)), Some(header(2))], vec![Some(mono), Some(stereo)]);
    rt.mixer.buses.env.presets.insert(DEFAULT_PRESET, reverb01());
    rt.mixer.buses.env.request(DEFAULT_PRESET);
    rt.mixer.buses.flange.set_presets(
        FlangePreset([20.0, 0.3, 0.2, 0.1, 1500.0, 0.002, 0.5, 0.9, 0.03]),
        FlangePreset([1.7, 0.3, 0.0, 1.0, 250.0, 0.0005, 0.25, 0.6, 0.11]),
    );
    rt.mixer.buses.flange.frame([32692, 2313, 32692, 2313]);
    rt.mixer.buses.eq.set_records(&eq_records());
    rt.mixer.buses.eq.clear(Some(jitter(0)));
    let routes: Vec<[(u8, i32); 4]> = (0..8).map(|b| [(9u8, b), (10, 4096), (11, 1), (12, 1638)]).collect();
    let ids = (0..voices)
        .filter_map(|k| {
            rt.mixer.open(&OpenRequest { bank: 0, slot: (k % 2) as u16, level: 100, azimuth: [224, 32, 0, 0, 0, 0], stream_offset: u32::MAX, inputs: &routes[k % buses] })
        })
        .collect();
    let source = Arc::new(GrainSource { name: "t".into(), duration: 4.0, pcm: Arc::new(Pcm { rate: 44_100, channels: vec![wave] }) });
    let a = GrainParams { attack: 0.1, sustain: 0.2, release: 0.1, window: 1.6, drift: 0.05 };
    let b = GrainParams { attack: 0.2, sustain: 0.1, release: 0.2, window: 1.5, drift: 0.05 };
    rt.grains.bind_truck(0, source, [a, b], [Record { gain: 0.5, pitch: 1.0, position: 0.3 }, Record { gain: 0.25, pitch: 1.0, position: 0.2 }]);
    (rt, ids)
}

/// The jitter walk's values at a clear: PI20#1 freq, gain, Q, PI20#2 freq, gain, Q.
fn jitter(clear: usize) -> [f32; 6] {
    let s = clear as f32;
    [7500.0 + 250.0 * (s % 9.0), 0.85 + 0.03 * (s % 7.0), 3.0 + 0.1 * (s % 5.0), 17500.0 - 200.0 * (s % 11.0), 0.8 + 0.02 * (s % 13.0), 3.0 - 0.1 * (s % 6.0)]
}

/// FNV-1a over the block's bits.
fn fold(hash: &mut u64, block: &[[f32; 256]; 6]) {
    for v in block.iter().flatten() {
        for byte in v.to_bits().to_le_bytes() {
            *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
}

fn run(name: &str, voices: usize, buses: usize) {
    const WARM: usize = 200;
    const BLOCKS: usize = 6000;
    let (mut rt, ids) = scene(voices, buses);
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    let mut times = Vec::with_capacity(BLOCKS);
    for block in 0..WARM + BLOCKS {
        if block % 6 == 0 {
            for (k, &v) in ids.iter().enumerate() {
                rt.mixer.set_azimuth(v, ((block * 37 + k * 911) % 65536) as i32);
                rt.mixer.set(v, 6, 2000 + ((block + k * 13) % 20000) as i32);
                rt.mixer.set(v, 7, 77);
                rt.mixer.set(v, 2, 20000);
                rt.mixer.set(v, 5, 3000);
                rt.mixer.set(v, 0, 4096 + ((block + k) % 400) as i32);
            }
        }
        if block % 12 == 0 {
            rt.mixer.buses.eq.clear(Some(jitter(block / 12)));
        }
        let start = Instant::now();
        let out = rt.render_block();
        let elapsed = start.elapsed();
        fold(&mut hash, out);
        if block >= WARM {
            times.push(elapsed.as_secs_f64() * 1e6);
        }
    }
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    times.sort_by(f64::total_cmp);
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
    println!(
        "{name}: {} voices, {BLOCKS} blocks: mean {mean:.1} µs, p50 {:.1}, p90 {:.1}, p99 {:.1} µs a block; output {hash:016x}",
        rt.mixer.voice_count(),
        at(0.5),
        at(0.9),
        at(0.99)
    );
}

#[test]
#[ignore = "a bench: run it with --release before and after a change"]
fn render_speed_and_output() {
    run("busy", 24, 8);
    run("idle", 8, 1);
}
