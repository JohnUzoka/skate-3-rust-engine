//! Both copies of every loop, bit for bit, on random and edge inputs (NaN, ±∞, denormals, ±0,
//! huge and tiny values, random bit patterns). On a CPU without FMA the comparisons are skipped
//! (printed), and the plain copy is all that can run anyway.
use super::*;

/// xorshift32: deterministic, no dependencies.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in [-1, 1).
    fn unit(&mut self) -> f32 {
        (self.next() >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }

    /// Any f32: random bits (NaNs with payloads, infinities, denormals, both zeros) a quarter of
    /// the time, a listed edge value a quarter, a scaled unit value otherwise.
    fn any(&mut self) -> f32 {
        match self.next() % 8 {
            0 | 1 => f32::from_bits(self.next()),
            2 | 3 => EDGES[self.next() as usize % EDGES.len()],
            4 => self.unit() * 1e-3,
            5 => self.unit() * 100.0,
            _ => self.unit(),
        }
    }

    /// Mostly audio-like values with edge values mixed in.
    fn sample(&mut self) -> f32 {
        if self.next().is_multiple_of(16) { self.any() } else { self.unit() }
    }
}

const EDGES: [f32; 22] = [
    0.0,
    -0.0,
    1.0,
    -1.0,
    f32::NAN,
    -f32::NAN,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::MIN_POSITIVE,
    -f32::MIN_POSITIVE,
    1e-40,  // denormal
    -1e-40, // denormal
    f32::from_bits(1),
    f32::from_bits(0x8000_0001),
    f32::MAX,
    f32::MIN,
    1e-18,
    1e30,
    -1e30,
    f32::from_bits(0x7FA0_0001), // signalling NaN with a payload
    f32::from_bits(0xFFC1_2345), // negative quiet NaN with a payload
    std::f32::consts::PI,
];

fn fma_path() -> Option<Path> {
    let p = Path::fma();
    if p.is_none() {
        eprintln!("this CPU has no FMA: only the plain copy exists, comparison skipped");
    }
    p
}

/// Bit patterns, with every NaN mapped to one marker. The copies agree bit for bit on every
/// non-NaN value and on where the NaNs are; only a NaN's sign and payload can differ. When several
/// operands of one operation are NaN, x86 returns the first source operand's NaN, and which
/// operand comes first is the compiler's choice per instruction: the C runtime's `fmaf(a, b, c)`
/// keeps its own order, the inlined `vfmadd` may be commuted (the biquad test hits this with NaN
/// coefficients and histories). An invalid operation (∞ − ∞) gives x86's default −NaN
/// (`0xFFC00000`) on both copies. No consumer reads a NaN's bits for anything but a bit-equality
/// memo of the same copy (doc 11 "Hardware FMA dispatch"). `nan_bits_differ` counts the cases.
fn bits(x: &[f32]) -> Vec<u32> {
    x.iter().map(|v| if v.is_nan() { NAN_MARK } else { v.to_bits() }).collect()
}

const NAN_MARK: u32 = 0x7FC0_0000;

fn nan_bits_differ(a: &[f32], b: &[f32]) -> usize {
    a.iter().zip(b).filter(|(a, b)| a.is_nan() && b.is_nan() && a.to_bits() != b.to_bits()).count()
}

/// Exact bit patterns (no NaN mapping): for the finite-input tests.
fn raw(x: &[f32]) -> Vec<u32> {
    x.iter().map(|v| v.to_bits()).collect()
}

/// A stable filter (poles inside the unit circle: a1 = −2r·cos θ, a2 = r²) with random zeros:
/// finite input stays finite, so every output bit must match exactly.
fn stable(rng: &mut Rng) -> Coefficients {
    let r = 0.5 + 0.4999 * (rng.unit() * 0.5 + 0.5);
    let theta = std::f32::consts::PI * (rng.unit() * 0.5 + 0.5);
    Coefficients { a1: -2.0 * r * theta.cos(), a2: r * r, b0: rng.unit(), b1: rng.unit() * 2.0, b2: rng.unit() }
}

fn coefficients(rng: &mut Rng, wild: bool) -> Coefficients {
    let mut v = || if wild { rng.any() } else { rng.unit() * 2.0 };
    Coefficients { a1: v(), a2: v(), b0: v(), b1: v(), b2: v() }
}

const LENGTHS: [usize; 13] = [0, 1, 2, 7, 8, 9, 15, 16, 24, 255, 256, 264, 1000];

#[test]
fn biquad_copies_are_bit_identical() {
    let Some(fma) = fma_path() else { return };
    let mut rng = Rng(0x2545_F491);
    let mut nan_payloads = 0usize;
    for round in 0..4000 {
        // Stable-ish filters (realistic), random ones, and fully wild ones (NaN / ∞ / denormal
        // coefficients and histories).
        let k = coefficients(&mut rng, round % 3 == 2);
        let start = if round % 2 == 0 { [rng.sample(), rng.sample(), rng.sample(), rng.sample()] } else { [rng.any(), rng.any(), rng.any(), rng.any()] };
        let len = LENGTHS[round % LENGTHS.len()];
        let input: Vec<f32> = match round % 5 {
            0 => vec![0.0; len],
            1 => vec![-0.0; len],
            2 => (0..len).map(|_| rng.any()).collect(),
            _ => (0..len).map(|_| rng.sample()).collect(),
        };
        let (mut ha, mut hb) = (start, start);
        let (mut xa, mut xb) = (input.clone(), input.clone());
        Path::PLAIN.biquad(&k, &mut ha, &mut xa);
        fma.biquad(&k, &mut hb, &mut xb);
        assert_eq!(bits(&xa), bits(&xb), "round {round}: {k:?} start {start:?}");
        assert_eq!(bits(&ha), bits(&hb), "round {round}");
        nan_payloads += nan_bits_differ(&xa, &xb) + nan_bits_differ(&ha, &hb);

        let y0 = [start[2], start[3]];
        let (mut ya, mut yb) = (y0, y0);
        let (mut xa, mut xb) = (input.clone(), input);
        Path::PLAIN.biquad_feedback(&k, &mut ya, &mut xa);
        fma.biquad_feedback(&k, &mut yb, &mut xb);
        assert_eq!(bits(&xa), bits(&xb), "feedback round {round}");
        assert_eq!(bits(&ya), bits(&yb), "feedback round {round}");
        nan_payloads += nan_bits_differ(&xa, &xb) + nan_bits_differ(&ya, &yb);
    }
    eprintln!("biquad: {nan_payloads} NaN values with a different sign / payload (all else exact)");
}

#[test]
fn biquad_long_decay_is_bit_identical() {
    // Stable filters, finite input (noise, denormals, ±0, tiny values), then a long silence: the
    // outputs decay into the denormal range around the 1e-18 bias. No NaN can arise, so the bits
    // must match exactly, NaN mapping not needed.
    let Some(fma) = fma_path() else { return };
    let mut rng = Rng(7);
    for _ in 0..60 {
        let k = stable(&mut rng);
        let (mut ha, mut hb) = ([0.0f32; 4], [0.0f32; 4]);
        for block in 0..400 {
            let input: Vec<f32> = match block {
                0 | 1 => (0..256).map(|_| rng.unit()).collect(),
                2 => (0..256).map(|i| [1e-40, -1e-40, -0.0, 0.0, f32::MIN_POSITIVE, 1e-30, -1e-45, 0.5][i % 8]).collect(),
                _ => vec![0.0; 256],
            };
            let (mut xa, mut xb) = (input.clone(), input);
            Path::PLAIN.biquad(&k, &mut ha, &mut xa);
            fma.biquad(&k, &mut hb, &mut xb);
            assert!(xa.iter().all(|v| v.is_finite()));
            assert_eq!(raw(&xa), raw(&xb), "{k:?} block {block}");
            assert_eq!(raw(&ha), raw(&hb));
            let mut y = [ha[2], ha[3]];
            let mut yb = y;
            let (mut fa, mut fb) = (vec![0.0f32; 64], vec![0.0f32; 64]);
            Path::PLAIN.biquad_feedback(&k, &mut y, &mut fa);
            fma.biquad_feedback(&k, &mut yb, &mut fb);
            assert_eq!(raw(&fa), raw(&fb));
        }
    }
}

/// The lockstep copies (`biquad2` … `biquad4`) against `biquad` on each channel, on one path at a
/// time (both when the CPU has FMA): exact bits for stable filters on finite input; for wild
/// coefficients and histories the NaNs fall in the same places, and only a NaN's sign / payload
/// may differ (counted), as between the two paths.
#[test]
fn biquad_lanes_match_biquad() {
    let paths: Vec<Path> = std::iter::once(Path::PLAIN).chain(Path::fma()).collect();
    let mut rng = Rng(0x1357_9BDF);
    let mut nan_payloads = 0usize;
    for path in paths {
        for round in 0..3000 {
            let wild = round % 4 == 3;
            let k = if wild { coefficients(&mut rng, true) } else { stable(&mut rng) };
            let lanes = 2 + round % 3;
            let len = LENGTHS[round % LENGTHS.len()];
            let mut starts = [[0.0f32; 4]; 4];
            let mut inputs: [Vec<f32>; 4] = Default::default();
            for c in 0..lanes {
                starts[c] = if wild { [rng.any(), rng.any(), rng.any(), rng.any()] } else { [rng.unit(), rng.unit(), rng.unit(), rng.unit()] };
                inputs[c] = match (round + c) % 4 {
                    0 => vec![0.0; len],
                    1 if wild => (0..len).map(|_| rng.any()).collect(),
                    _ => (0..len).map(|_| rng.sample()).collect(),
                };
            }
            let (mut want_h, mut want_x) = (starts, inputs.clone());
            for c in 0..lanes {
                path.biquad(&k, &mut want_h[c], &mut want_x[c]);
            }
            let (mut got_h, mut got_x) = (starts, inputs);
            {
                let [h0, h1, h2, h3] = &mut got_h;
                let [x0, x1, x2, x3] = &mut got_x;
                match lanes {
                    2 => path.biquad2(&k, [h0, h1], [x0, x1]),
                    3 => path.biquad3(&k, [h0, h1, h2], [x0, x1, x2]),
                    _ => path.biquad4(&k, [h0, h1, h2, h3], [x0, x1, x2, x3]),
                }
            }
            for c in 0..lanes {
                let finite = !wild && want_x[c].iter().chain(&want_h[c]).all(|v| v.is_finite());
                if finite {
                    assert_eq!(raw(&got_x[c]), raw(&want_x[c]), "{} round {round} lane {c}: {k:?}", path.name());
                    assert_eq!(raw(&got_h[c]), raw(&want_h[c]), "{} round {round} lane {c}", path.name());
                } else {
                    assert_eq!(bits(&got_x[c]), bits(&want_x[c]), "{} round {round} lane {c}: {k:?}", path.name());
                    assert_eq!(bits(&got_h[c]), bits(&want_h[c]), "{} round {round} lane {c}", path.name());
                    nan_payloads += nan_bits_differ(&got_x[c], &want_x[c]) + nan_bits_differ(&got_h[c], &want_h[c]);
                }
            }
        }
    }
    eprintln!("biquad lanes: {nan_payloads} NaN values with a different sign / payload (all else exact)");
}

#[test]
fn trig_copies_are_bit_identical() {
    let Some(fma) = fma_path() else { return };
    let mut nan_payloads = 0usize;
    let mut check = |x: f32| {
        // black_box: no compile-time folding of the edge values (LLVM's folder makes its own NaN).
        let x = std::hint::black_box(x);
        let (a, b) = ([Path::PLAIN.sin(x), Path::PLAIN.cos(x)], [fma.sin(x), fma.cos(x)]);
        assert_eq!(bits(&a), bits(&b), "sin / cos {x:e} ({:#010x})", x.to_bits());
        if x.is_finite() {
            assert_eq!(raw(&a), raw(&b), "finite input {x:e}: exact bits");
        }
        nan_payloads += nan_bits_differ(&a, &b);
    };
    for x in EDGES {
        check(x);
    }
    // The oscillator's range densely, then random bit patterns (every class).
    for k in -2_000_000..=2_000_000 {
        check(k as f32 * 1e-5);
    }
    let mut rng = Rng(0xDEAD_BEEF);
    for _ in 0..4_000_000 {
        check(f32::from_bits(rng.next()));
    }
    eprintln!("trig: {nan_payloads} NaN results with a different sign / payload (all else exact)");
}

#[test]
fn fss_mix_copies_are_bit_identical() {
    let Some(fma) = fma_path() else { return };
    let mut rng = Rng(0x0BAD_F00D);
    let mut nan_payloads = 0usize;
    for round in 0..3000 {
        let len = [0usize, 4, 8, 12, 256, 260][round % 6];
        let i: Vec<f32> = (0..len).map(|_| rng.sample()).collect();
        let q: Vec<f32> = (0..len).map(|_| rng.sample()).collect();
        let phi = if round % 4 == 0 { rng.any() } else { rng.unit() * 6.0 };
        let delta = match round % 5 {
            0 => 0.0,
            1 => -0.0,
            2 => rng.any(),
            _ => rng.unit() * 0.1,
        };
        let silent = vec![0.0f32; len];
        let (mut a, mut b) = (silent.clone(), silent);
        Path::PLAIN.fss_mix(&mut a, &i, &q, phi, delta);
        fma.fss_mix(&mut b, &i, &q, phi, delta);
        assert_eq!(bits(&a), bits(&b), "round {round}: phi {phi:e} delta {delta:e}");
        if i.iter().chain(&q).chain([&phi, &delta]).all(|v| v.is_finite() && v.abs() < 1e6) {
            assert_eq!(raw(&a), raw(&b), "finite round {round}: exact bits");
        }
        nan_payloads += nan_bits_differ(&a, &b);
    }
    eprintln!("fss_mix: {nan_payloads} NaN values with a different sign / payload (all else exact)");
}

#[test]
fn resample_copies_are_bit_identical() {
    let Some(fma) = fma_path() else { return };
    let mut rng = Rng(0x1357_9BDF);
    let mut nan_payloads = 0usize;
    for round in 0..3000 {
        let finite = round % 2 == 0;
        let src: Vec<f32> = (0..1200).map(|_| if finite { rng.unit() } else { rng.sample() }).collect();
        let len = LENGTHS[round % LENGTHS.len()].min(256);
        let step = match round % 4 {
            0 => 65536,
            1 => 0,
            2 => 1 << 18,
            _ => rng.next() % (1 << 18),
        };
        let frac = rng.next() & 0xFFFF;
        let position = u64::from(rng.next() % 64);
        let frame = |i: u64| src.get(i as usize).copied().unwrap_or(0.0);
        let (mut a, mut b) = (vec![0.0f32; len], vec![0.0f32; len]);
        Path::PLAIN.resample(&mut a, position, frac, step, frame);
        fma.resample(&mut b, position, frac, step, frame);
        assert_eq!(bits(&a), bits(&b), "round {round}: step {step} frac {frac}");
        if finite {
            assert_eq!(raw(&a), raw(&b), "finite round {round}: exact bits");
        }
        nan_payloads += nan_bits_differ(&a, &b);
    }
    eprintln!("resample: {nan_payloads} NaN values with a different sign / payload (all else exact)");
}

#[test]
fn choice_honours_the_override() {
    assert_eq!(choose(Some("0")), Path::PLAIN);
    assert_eq!(choose(Some(" 0 ")), Path::PLAIN);
    let auto = Path::fma().unwrap_or(Path::PLAIN);
    assert_eq!(choose(None), auto);
    // "1" asks for FMA: honoured only when the CPU has it.
    assert_eq!(choose(Some("1")), auto);
    assert_eq!(choose(Some("yes")), auto);
    // The cached path is one of the two and stays put.
    let a = active();
    assert_eq!(active(), a);
    assert!(a == Path::PLAIN || Some(a) == Path::fma());
}

