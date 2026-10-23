//! PeakingIir2 (PI20, `sub_82B2C658`; spec `audio-specs/aems-voice-graph-spec.md` §4.10): an RBJ peaking EQ.
//! Parameters: 0 = centre (Hz), 1 = linear gain, 2 = Q. Class defaults 96000 Hz / 1.0 / 3.0. The
//! module bypasses (pass-through, history cleared once) only when the gain is exactly 1.0; ω is
//! clamped to [FLOOR, CEIL] and Q to 0.2..20 for the coefficients. Same Direct Form I kernel as
//! the high/low-pass ([`super::biquad::kernel`]). Not bit-exact against retail (no replay vectors:
//! the coefficient arithmetic is RBJ in f32 with libm trig, UNCERTAIN to the ulp).
use super::biquad::{CEIL, Coefficients, FLOOR, kernel_channels, omega};

#[derive(Clone, Debug)]
pub struct PeakingIir2 {
    pub freq: f32,
    pub gain: f32,
    pub q: f32,
    cached: Option<[u32; 3]>,
    coefficients: Coefficients,
    history: [[f32; 4]; 8],
    filtering: bool,
}

impl Default for PeakingIir2 {
    fn default() -> Self {
        Self { freq: 96_000.0, gain: 1.0, q: 3.0, cached: None, coefficients: Coefficients::default(), history: [[0.0; 4]; 8], filtering: false }
    }
}

/// RBJ peaking coefficients (A = √gain, α = sin ω / 2Q), normalised by a0, in f32.
pub fn coefficients(freq: f32, gain: f32, q: f32, rate: f32) -> Coefficients {
    let w = omega(freq, rate);
    let w = if w.is_nan() { CEIL } else { w.clamp(FLOOR, CEIL) };
    let q = q.clamp(0.2, 20.0);
    let a = gain.max(0.0).sqrt();
    let s = (w as f64).sin() as f32;
    let c = (w as f64).cos() as f32;
    let alpha = s / (2.0 * q);
    let a0 = 1.0 + alpha / a;
    let inv = 1.0 / a0;
    Coefficients {
        b0: (1.0 + alpha * a) * inv,
        b1: (-2.0 * c) * inv,
        b2: (1.0 - alpha * a) * inv,
        a1: (-2.0 * c) * inv,
        a2: (1.0 - alpha / a) * inv,
    }
}

impl PeakingIir2 {
    pub fn bypassed(&self) -> bool {
        self.gain == 1.0
    }

    pub fn process(&mut self, channels: &mut [&mut [f32]], rate: f32) {
        if self.gain == 1.0 {
            if self.filtering {
                self.history = [[0.0; 4]; 8];
            }
            self.filtering = false;
            return;
        }
        let key = [self.freq.to_bits(), self.gain.to_bits(), self.q.to_bits()];
        if self.cached != Some(key) {
            self.coefficients = coefficients(self.freq, self.gain, self.q, rate);
            self.cached = Some(key);
        }
        self.filtering = true;
        kernel_channels(&self.coefficients, &mut self.history, channels);
    }
}

/// DCl0 (`sub_82B22678`): hard clip to ±level; bypass when level ≥ 100 or NaN.
pub fn clip(channels: &mut [&mut [f32]], level: f32) {
    if level.is_nan() || level >= 100.0 {
        return;
    }
    for ch in channels.iter_mut() {
        for s in ch.iter_mut() {
            *s = s.clamp(-level, level);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_gain(eq: &mut PeakingIir2, f: f32) -> f32 {
        let mut x: Vec<f32> = (0..48000).map(|i| (std::f32::consts::TAU * f * i as f32 / 48000.0).sin()).collect();
        for chunk in x.chunks_mut(256) {
            eq.process(&mut [chunk], 48000.0);
        }
        let tail = &x[24000..];
        (tail.iter().map(|v| v * v).sum::<f32>() / tail.len() as f32).sqrt() * std::f32::consts::SQRT_2
    }

    #[test]
    fn peak_gain_at_the_centre_and_flat_far_away() {
        let mut eq = PeakingIir2 { freq: 1000.0, gain: 2.0, q: 1.0, ..Default::default() };
        assert!((tone_gain(&mut eq, 1000.0) - 2.0).abs() < 0.02);
        let mut eq = PeakingIir2 { freq: 1000.0, gain: 0.25, q: 1.0, ..Default::default() };
        assert!((tone_gain(&mut eq, 1000.0) - 0.25).abs() < 0.01);
        assert!((tone_gain(&mut eq, 20.0) - 1.0).abs() < 0.03);
    }

    #[test]
    fn gain_one_is_a_bit_exact_bypass() {
        let mut eq = PeakingIir2 { freq: 500.0, ..Default::default() };
        let mut x: Vec<f32> = (0..256).map(|i| (i as f32 * 0.3).sin()).collect();
        let before = x.clone();
        eq.process(&mut [&mut x[..]], 48000.0);
        assert_eq!(x, before);
        let mut c = vec![0.5f32, -0.2, 0.09];
        clip(&mut [&mut c[..]], 0.1);
        assert_eq!(c, vec![0.1, -0.1, 0.09]);
        clip(&mut [&mut c[..]], 100.0);
    }

    /// The process before the optimisation pass (test-only reference): the plain kernel on every
    /// channel.
    fn process_reference(eq: &mut PeakingIir2, channels: &mut [&mut [f32]], rate: f32) {
        if eq.gain == 1.0 {
            if eq.filtering {
                eq.history = [[0.0; 4]; 8];
            }
            eq.filtering = false;
            return;
        }
        let key = [eq.freq.to_bits(), eq.gain.to_bits(), eq.q.to_bits()];
        if eq.cached != Some(key) {
            eq.coefficients = coefficients(eq.freq, eq.gain, eq.q, rate);
            eq.cached = Some(key);
        }
        eq.filtering = true;
        for (ch, samples) in channels.iter_mut().enumerate().take(8) {
            super::super::biquad::kernel(&eq.coefficients, &mut eq.history[ch], samples);
        }
    }

    /// Six-channel blocks as an eEQChain bus sees them (silent channels, a voice on some, -0.0,
    /// a constant), with the parameters changing as the jitter / re-rolls change them: the
    /// optimised process (twin copies, settled filters, silent tails) equals the reference bit for
    /// bit, block by block, outputs and histories.
    #[test]
    fn silent_shortcuts_match_the_plain_kernel() {
        let mut seed = 0x1234_5678u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        };
        let (mut a, mut b) = (PeakingIir2::default(), PeakingIir2::default());
        for block in 0..4000usize {
            // Parameters: re-rolled every 300 blocks, jittered every 6 for a stretch, bypass for a
            // while (gain 1), a NaN frequency once.
            let phase = block / 300;
            let (f, g, q) = match phase % 5 {
                0 => (450.0, 1.5, 3.0),
                1 => (150.0 + (block / 6 % 7) as f32 * 30.0, 0.75 + (block / 6 % 3) as f32 * 0.1, 0.25),
                2 => (5000.0, 1.0, 2.0),
                3 => (if block == 1000 { f32::NAN } else { 2500.0 }, 1.25, 20.0),
                _ => (96000.0, 0.1, 0.2),
            };
            for eq in [&mut a, &mut b] {
                (eq.freq, eq.gain, eq.q) = (f, g, q);
            }
            // Channel contents: voices in bursts on channels 0..3, the rest silent; channel 4 -0.0
            // sometimes, channel 5 a constant sometimes.
            let voice = block % 97 < 20;
            let mut input = [[0.0f32; 256]; 6];
            for (c, ch) in input.iter_mut().enumerate() {
                for x in ch.iter_mut() {
                    *x = match c {
                        0..=3 if voice && c <= block % 4 => noise() * 0.1,
                        4 if block % 11 == 0 => -0.0,
                        5 if block % 13 == 0 => 0.125,
                        _ => 0.0,
                    };
                }
            }
            let (mut xa, mut xb) = (input, input);
            {
                let mut pa: Vec<&mut [f32]> = xa.iter_mut().map(|c| &mut c[..]).collect();
                process_reference(&mut a, &mut pa, 48000.0);
                let mut pb: Vec<&mut [f32]> = xb.iter_mut().map(|c| &mut c[..]).collect();
                b.process(&mut pb, 48000.0);
            }
            let bits = |x: &[[f32; 256]; 6]| x.iter().flatten().map(|v| v.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(&xa), bits(&xb), "block {block}");
            let hist = |e: &PeakingIir2| e.history.iter().flatten().map(|v| v.to_bits()).collect::<Vec<_>>();
            assert_eq!(hist(&a), hist(&b), "block {block}");
            assert_eq!((a.filtering, a.cached), (b.filtering, b.cached));
        }
    }

    /// An eEQChain bus's two EQs in series on six channels: idle (silence into the first EQ, its
    /// bias response into the second, the same on every channel), then a voice panned over the six
    /// channels (two pairs at one level, so twins with sound in them), then idle again, with the
    /// parameters changing every 12 blocks as the jitter walk posts them. The optimised process
    /// equals the reference chain bit for bit, block by block, outputs and histories.
    #[test]
    fn a_bus_chain_matches_the_plain_kernel() {
        let mut seed = 0x7357_0001u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        };
        let (mut a, mut b) = ([PeakingIir2::default(), PeakingIir2::default()], [PeakingIir2::default(), PeakingIir2::default()]);
        let levels = [0.7, 0.7, 0.5, 0.1, 0.3, 0.3];
        for block in 0..3000usize {
            if block % 12 == 0 {
                let step = (block / 12) as f32;
                let values = [
                    (10_000.0 + 400.0 * (step % 7.0), 0.5 + 0.05 * (step % 9.0), 2.0 + 0.25 * (step % 5.0)),
                    (15_000.0 + 500.0 * (step % 11.0), 1.2 - 0.07 * (step % 6.0), 4.0 - 0.5 * (step % 3.0)),
                ];
                for chain in [&mut a, &mut b] {
                    for (eq, (f, g, q)) in chain.iter_mut().zip(values) {
                        (eq.freq, eq.gain, eq.q) = (f, g, q);
                    }
                }
            }
            let mut input = [[0.0f32; 256]; 6];
            if (block / 400) % 3 == 1 {
                for i in 0..256 {
                    let v = noise() * 0.2;
                    for (ch, level) in input.iter_mut().zip(levels) {
                        ch[i] = v * level;
                    }
                }
            }
            let (mut xa, mut xb) = (input, input);
            {
                let mut pa: Vec<&mut [f32]> = xa.iter_mut().map(|c| &mut c[..]).collect();
                for eq in &mut a {
                    process_reference(eq, &mut pa, 48000.0);
                }
                let mut pb: Vec<&mut [f32]> = xb.iter_mut().map(|c| &mut c[..]).collect();
                for eq in &mut b {
                    eq.process(&mut pb, 48000.0);
                }
            }
            let bits = |x: &[[f32; 256]; 6]| x.iter().flatten().map(|v| v.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(&xa), bits(&xb), "block {block}");
            let hist = |e: &PeakingIir2| e.history.iter().flatten().map(|v| v.to_bits()).collect::<Vec<_>>();
            for (ea, eb) in a.iter().zip(&b) {
                assert_eq!(hist(ea), hist(eb), "block {block}");
            }
        }
    }
}
