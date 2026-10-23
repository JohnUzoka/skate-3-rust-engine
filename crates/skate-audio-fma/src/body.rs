//! The loop bodies, written once. Every function here is `#[inline(always)]`, so each wrapper in
//! `lib.rs` (the plain copy and the `#[target_feature(enable = "fma")]` copy) gets its own inlined
//! instance compiled with its own features. The source is the code that lived in `skate-audio`
//! before the dispatch (moved verbatim, doc 11 "Hardware FMA dispatch").
//!
//! `f32::mul_add` is a single rounding of a·b + c in both copies: a call into the C runtime's
//! `fmaf` without the `fma` target feature, one `vfmadd` instruction with it. Rust never contracts
//! `a * b + c` into a fused operation, so the plain products and sums below stay separate roundings
//! in both copies.
use crate::{BIAS, COS, Coefficients, INV_TWO_PI, SIN, TWO_PI, WEIGHT};

/// Direct Form I in place over one block of one channel, history {x1, x2, y1, y2}
/// (`skate_audio::dsp::biquad::kernel`; the association by `n % 8` is documented there).
#[inline(always)]
pub fn biquad(k: &Coefficients, history: &mut [f32; 4], samples: &mut [f32]) {
    let [mut x1, mut x2, mut y1, mut y2] = *history;
    for (n, s) in samples.iter_mut().enumerate() {
        let x = *s;
        let t = match n % 8 {
            0 => k.b1.mul_add(x1, k.b2.mul_add(x2, k.b0 * x + BIAS)),
            1 => k.b2.mul_add(x2, k.b1.mul_add(x1, k.b0 * x + BIAS)),
            _ => k.b0.mul_add(x, k.b2.mul_add(x2, k.b1 * x1 + BIAS)),
        };
        let y = (-k.a2).mul_add(y2, (-k.a1).mul_add(y1, t));
        x2 = x1;
        x1 = x;
        y2 = y1;
        y1 = y;
        *s = y;
    }
    *history = [x1, x2, y1, y2];
}

/// [`biquad`] on `N` channels of one length in lockstep, each with its own history. Every channel
/// runs [`biquad`]'s arithmetic in [`biquad`]'s order, so it gets the bits [`biquad`] gives it; the
/// channels' feedback chains don't depend on each other, so the CPU overlaps them, where one
/// channel waits on two fused multiply-adds a sample.
#[inline(always)]
fn biquad_lanes<const N: usize>(k: &Coefficients, history: [&mut [f32; 4]; N], samples: [&mut [f32]; N]) {
    let len = samples[0].len();
    assert!(samples.iter().all(|s| s.len() == len), "lockstep channels have one length");
    let mut x1 = [0.0f32; N];
    let mut x2 = [0.0f32; N];
    let mut y1 = [0.0f32; N];
    let mut y2 = [0.0f32; N];
    for (c, h) in history.iter().enumerate() {
        [x1[c], x2[c], y1[c], y2[c]] = **h;
    }
    let samples = samples.map(|s| &mut s[..len]);
    for n in 0..len {
        let position = n % 8;
        for c in 0..N {
            let x = samples[c][n];
            let t = match position {
                0 => k.b1.mul_add(x1[c], k.b2.mul_add(x2[c], k.b0 * x + BIAS)),
                1 => k.b2.mul_add(x2[c], k.b1.mul_add(x1[c], k.b0 * x + BIAS)),
                _ => k.b0.mul_add(x, k.b2.mul_add(x2[c], k.b1 * x1[c] + BIAS)),
            };
            let y = (-k.a2).mul_add(y2[c], (-k.a1).mul_add(y1[c], t));
            x2[c] = x1[c];
            x1[c] = x;
            y2[c] = y1[c];
            y1[c] = y;
            samples[c][n] = y;
        }
    }
    for (c, h) in history.into_iter().enumerate() {
        *h = [x1[c], x2[c], y1[c], y2[c]];
    }
}

/// [`biquad_lanes`] on two channels.
#[inline(always)]
pub fn biquad2(k: &Coefficients, history: [&mut [f32; 4]; 2], samples: [&mut [f32]; 2]) {
    biquad_lanes(k, history, samples);
}

/// [`biquad_lanes`] on three channels.
#[inline(always)]
pub fn biquad3(k: &Coefficients, history: [&mut [f32; 4]; 3], samples: [&mut [f32]; 3]) {
    biquad_lanes(k, history, samples);
}

/// [`biquad_lanes`] on four channels.
#[inline(always)]
pub fn biquad4(k: &Coefficients, history: [&mut [f32; 4]; 4], samples: [&mut [f32]; 4]) {
    biquad_lanes(k, history, samples);
}

/// The feedback half of [`biquad`] with the feed-forward sum fixed at the bias
/// (`skate_audio::dsp::biquad::kernel_silent_tail` after its run-time check). `y` = {y1, y2}.
#[inline(always)]
pub fn biquad_feedback(k: &Coefficients, y: &mut [f32; 2], samples: &mut [f32]) {
    let [mut y1, mut y2] = *y;
    for s in samples.iter_mut() {
        let v = (-k.a2).mul_add(y2, (-k.a1).mul_add(y1, BIAS));
        y2 = y1;
        y1 = v;
        *s = v;
    }
    *y = [y1, y2];
}

#[inline(always)]
fn c(table: &[u32; 11], k: usize) -> f32 {
    f32::from_bits(table[k])
}

/// x − 2π · round(x / 2π) (`vrfin` = nearest, ties to even; fused `vnmsubfp`).
#[inline(always)]
fn reduce(x: f32) -> f32 {
    let n = (x * INV_TWO_PI).round_ties_even();
    (-TWO_PI).mul_add(n, x)
}

/// `XMVectorSin` per lane (`skate_audio::dsp::fss::sin`).
#[inline(always)]
pub fn sin(x: f32) -> f32 {
    let v = reduce(x);
    let v2 = v * v;
    let mut p = v2 * v;
    let mut r = c(&SIN, 0).mul_add(p, v);
    for k in 1..11 {
        p *= v2;
        r = c(&SIN, k).mul_add(p, r);
    }
    r
}

/// `XMVectorCos` per lane in retail's power pairing (`skate_audio::dsp::fss::cos`).
#[inline(always)]
pub fn cos(x: f32) -> f32 {
    let v = reduce(x);
    let v2 = v * v;
    let v4 = v2 * v2;
    let v6 = v4 * v2;
    let v8 = v4 * v4;
    let v10 = v6 * v4;
    let v12 = v6 * v6;
    let v14 = v8 * v6;
    let v16 = v8 * v8;
    let v18 = v10 * v8;
    let v20 = v10 * v10;
    let v22 = v12 * v10;
    let powers = [v2, v4, v6, v8, v10, v12, v14, v16, v18, v20, v22];
    let mut r = 1.0f32;
    for (k, p) in powers.into_iter().enumerate() {
        r = c(&COS, k).mul_add(p, r);
    }
    r
}

/// The FrequencyShiftSsb oscillator mix (`skate_audio::dsp::fss::FrequencyShift::process`): lanes
/// {φ, φ + Δ, φ + 2Δ, φ + 3Δ} advancing by 4Δ per group of four, out = I · cos − Q · sin. A lane
/// whose bits repeat its previous group's (a 0 Hz shift) reuses its `sin` / `cos` (they are pure
/// functions of the bits). `samples.len()` a multiple of 4; `i` and `q` at least as long.
#[inline(always)]
pub fn fss_mix(samples: &mut [f32], i: &[f32], q: &[f32], phi: f32, delta: f32) {
    let mut lanes = [phi, phi + delta, delta.mul_add(2.0, phi), delta.mul_add(3.0, phi)];
    let step = delta * 4.0;
    let mut trig: [Option<(u32, f32, f32)>; 4] = [None; 4];
    for (g, out) in samples.chunks_exact_mut(4).enumerate() {
        for (k, s) in out.iter_mut().enumerate() {
            let at = 4 * g + k;
            let bits = lanes[k].to_bits();
            let (c, sn) = match trig[k] {
                Some((b, c, sn)) if b == bits => (c, sn),
                _ => {
                    let v = (cos(lanes[k]), sin(lanes[k]));
                    trig[k] = Some((bits, v.0, v.1));
                    v
                }
            };
            *s = i[at] * c - q[at] * sn;
        }
        for l in &mut lanes {
            *l += step;
        }
    }
}

/// Linear interpolation with a 16.16 phase (`skate_audio::dsp::resample::Resampler::render`): each
/// output is a + (b − a)·(frac·W) as one fused multiply-add.
#[inline(always)]
pub fn resample(out: &mut [f32], position: u64, frac: u32, step: u32, mut frame: impl FnMut(u64) -> f32) {
    let (mut position, mut frac) = (position, frac);
    let mut a = frame(position);
    let mut b = frame(position + 1);
    for o in out.iter_mut() {
        let w = frac as f32 * WEIGHT;
        *o = (b - a).mul_add(w, a);
        frac += step;
        let carry = u64::from(frac >> 16);
        frac &= 0xFFFF;
        if carry > 0 {
            position += carry;
            if carry == 1 {
                a = b;
            } else {
                a = frame(position);
            }
            b = frame(position + 1);
        }
    }
}

/// TU3 Gain's `82B3C098` kernel. Keep the native four-lane rounding order.
#[inline(always)]
pub fn gain_ramp(samples: &mut [f32], start: f32, step: f32) {
    let mut lanes = [start, start + step, step.mul_add(2.0, start), step.mul_add(3.0, start)];
    let stride = step * 4.0;
    let flat = step.mul_add(64.0, start);
    let ramp_len = samples.len().min(64);
    for (block, group) in samples[..ramp_len].chunks_mut(32).enumerate() {
        for (chunk, values) in group.chunks_mut(4).enumerate() {
            for (lane, sample) in values.iter_mut().enumerate() {
                let gain = if chunk == 0 { lanes[lane] } else { (chunk as f32).mul_add(stride, lanes[lane]) };
                *sample *= gain;
            }
        }
        if block == 0 {
            for lane in &mut lanes {
                *lane = 8.0f32.mul_add(stride, *lane);
            }
        }
    }
    for sample in samples.iter_mut().skip(64) {
        *sample *= flat;
    }
}
