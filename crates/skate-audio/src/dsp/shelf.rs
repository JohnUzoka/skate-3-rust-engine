//! HighShelfIir2 (`HS20`, process `sub_82B26740`, coefficients `sub_82B43D78`; voice-graph spec
//! §4.10): an RBJ high shelf, slope S = 1. Parameters: 0 = corner (Hz, `+52`), 1 = linear gain
//! (`+60`; the plateau above the corner, A = √gain). Used in the board grain chain's graph 3
//! (5000 Hz × 0.65) and the stream bus.
//!
//! Per block: ω = f32(f32(fc / rate) · 2π). It filters only while ω < CEIL **and** gain ≠ 1.0;
//! otherwise it passes the block through and clears the histories once (when it was filtering).
//! While filtering ω is raised to FLOOR, and the coefficients are rebuilt when ω or the gain differ
//! from the cached pair (`+216` / `+220`; the bypass path caches its unclamped ω too). The kernel is
//! the shared biquad kernel `sub_82B43AF8` ([`super::biquad::kernel`]).
use super::biquad::{CEIL, Coefficients, FLOOR, TWO_PI, kernel_channels};

/// α = sin ω × this (`0x822F8E50`, not exactly 1/√2).
pub const ALPHA_SCALE: f32 = f32::from_bits(0x3F35_04EF);

/// `sub_82B43D78` in its own operation order (single precision; sin/cos in double, rounded once).
pub fn coefficients(omega: f32, gain: f32) -> Coefficients {
    let s = (omega as f64).sin() as f32;
    let c = (omega as f64).cos() as f32;
    let a = gain.sqrt();
    let root = a.sqrt();
    let plus = a + 1.0;
    let minus = a - 1.0;
    let alpha = s * ALPHA_SCALE;
    let ra = root * alpha;
    // a0 = (A+1) − (A−1)cos + 2√A·α.
    let a0 = ra.mul_add(2.0, (-minus).mul_add(c, plus));
    let inv = 1.0 / a0;
    let b0_sum = ra.mul_add(2.0, minus * c) + plus;
    let b1_sum = plus.mul_add(c, minus);
    let b2_sum = (-ra).mul_add(2.0, minus.mul_add(c, plus));
    let a1_sum = (-plus).mul_add(c, minus);
    let a2_sum = (-ra).mul_add(2.0, (-minus).mul_add(c, plus));
    Coefficients {
        a1: (a1_sum * inv) * 2.0,
        a2: a2_sum * inv,
        b0: (b0_sum * inv) * a,
        b1: ((b1_sum * inv) * a) * -2.0,
        b2: (b2_sum * inv) * a,
    }
}

#[derive(Clone, Debug)]
pub struct HighShelfIir2 {
    pub freq: f32,
    pub gain: f32,
    cached: Option<(f32, f32)>,
    coefficients: Coefficients,
    history: [[f32; 4]; 8],
    filtering: bool,
}

impl HighShelfIir2 {
    pub fn new(freq: f32, gain: f32) -> Self {
        Self { freq, gain, cached: None, coefficients: Coefficients::default(), history: [[0.0; 4]; 8], filtering: false }
    }

    pub fn bypassed(&self, rate: f32) -> bool {
        let w = (self.freq / rate) * TWO_PI;
        w >= CEIL || self.gain == 1.0
    }

    pub fn process(&mut self, channels: &mut [&mut [f32]], rate: f32) {
        let w = (self.freq / rate) * TWO_PI;
        // Float compares as retail's `fcmpu` (a NaN ω filters, at FLOOR).
        if w >= CEIL || self.gain == 1.0 {
            if self.filtering {
                self.history = [[0.0; 4]; 8];
                self.filtering = false;
            }
            self.cached = Some((w, self.gain));
            return;
        }
        self.filtering = true;
        let w = if w >= FLOOR { w } else { FLOOR };
        let key = (w, self.gain);
        if self.cached.is_none_or(|c| c != key) {
            self.coefficients = coefficients(w, self.gain);
            self.cached = Some(key);
        }
        kernel_channels(&self.coefficients, &mut self.history, channels);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_gain(eq: &mut HighShelfIir2, f: f32) -> f32 {
        let mut x: Vec<f32> = (0..48000).map(|i| (std::f32::consts::TAU * f * i as f32 / 48000.0).sin()).collect();
        for chunk in x.chunks_mut(256) {
            eq.process(&mut [chunk], 48000.0);
        }
        let tail = &x[24000..];
        (tail.iter().map(|v| v * v).sum::<f32>() / tail.len() as f32).sqrt() * std::f32::consts::SQRT_2
    }

    #[test]
    fn the_board_shelf_cuts_the_top_to_the_gain() {
        let mut eq = HighShelfIir2::new(5000.0, 0.65);
        assert!((tone_gain(&mut eq, 100.0) - 1.0).abs() < 0.01);
        // Plateau = gain (A² with A = √gain); −1.87 dB at 20 kHz, about half way at the corner.
        assert!((tone_gain(&mut eq, 20000.0) - 0.65).abs() < 0.02);
        let mid = tone_gain(&mut HighShelfIir2::new(5000.0, 0.65), 5000.0);
        assert!((mid - 0.65f32.sqrt()).abs() < 0.03, "{mid}");
    }

    #[test]
    fn rbj_reference_and_bypass() {
        // Against the textbook RBJ high shelf in f64 (S = 1).
        let w = (5000.0f32 / 48000.0) * TWO_PI;
        let k = coefficients(w, 0.65);
        let (a, cs, sn) = (0.65f64.sqrt(), (w as f64).cos(), (w as f64).sin());
        let alpha = sn / 2.0 * 2.0f64.sqrt();
        let a0 = (a + 1.0) - (a - 1.0) * cs + 2.0 * a.sqrt() * alpha;
        let want = [
            2.0 * ((a - 1.0) - (a + 1.0) * cs) / a0,
            ((a + 1.0) - (a - 1.0) * cs - 2.0 * a.sqrt() * alpha) / a0,
            a * ((a + 1.0) + (a - 1.0) * cs + 2.0 * a.sqrt() * alpha) / a0,
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cs) / a0,
            a * ((a + 1.0) + (a - 1.0) * cs - 2.0 * a.sqrt() * alpha) / a0,
        ];
        for (got, want) in [k.a1, k.a2, k.b0, k.b1, k.b2].into_iter().zip(want) {
            assert!((got as f64 - want).abs() < 1e-6, "{got} vs {want}");
        }
        let mut eq = HighShelfIir2::new(5000.0, 1.0);
        let mut x: Vec<f32> = (0..256).map(|i| (i as f32 * 0.3).sin()).collect();
        let before = x.clone();
        eq.process(&mut [&mut x[..]], 48000.0);
        assert_eq!(x, before);
        assert!(HighShelfIir2::new(24000.0, 0.5).bypassed(48000.0));
    }
}
