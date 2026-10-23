//! skate-audio's hot fused-multiply-add loops, compiled twice and picked at run time.
//!
//! Without the `fma` target feature every `f32::mul_add` is a call into the C runtime's `fmaf`
//! (~5 ns); with it, one `vfmadd` instruction. Both are the exactly rounded a·b + c, so the two
//! copies give the same bits (doc 11 "Hardware FMA dispatch": every e2e render byte-identical,
//! and the tests below). The loops that call `mul_add` per sample live here:
//! - the Direct Form I biquad kernel, its lockstep copies for two to four channels and its
//!   silent-tail feedback loop (`dsp::biquad`, used by the high/low-pass, peaking, shelf and the
//!   FSS allpasses);
//! - the FrequencyShiftSsb oscillator with its polynomial `sin` / `cos` (`dsp::fss`);
//! - the linear resampler (`dsp::resample`, every voice).
//!
//! Each loop body is written once in `body.rs` (`#[inline(always)]`) and wrapped twice by
//! [`kernels!`]: `plain::*` (the crate's normal features) and `fma::*`
//! (`#[target_feature(enable = "fma")]`, which implies AVX and SSE4.1). [`Path`] picks one;
//! a `Path` that names the FMA copy can only be made after `std::is_x86_feature_detected!` has
//! confirmed the CPU (and the OS) support every feature the FMA copy is compiled with. That check
//! is the whole safety argument for the one `unsafe` call below; `skate-audio` itself stays
//! `#![forbid(unsafe_code)]`.
//!
//! The process-wide choice ([`active`]) is made once and cached in an atomic: FMA when the CPU has
//! it, unless `SKATE_AUDIO_FMA=0` forces the plain copy (`SKATE_AUDIO_FMA=1` asks for FMA, honoured
//! only when the CPU supports it). [`init`] makes the choice up front and says what it picked, for
//! the game's start-up log.
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod body;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicU8, Ordering};

/// Denormal guard added to the biquad's feed-forward sum (cell 0x822F87B0).
pub const BIAS: f32 = 1e-18;
/// 2π as f32 (6.2831855).
pub const TWO_PI: f32 = std::f32::consts::TAU;
/// 1/2π as the image holds it (`0x822F8904` for the wrap, lane 3 of `0x822F9850` for the trig).
pub const INV_TWO_PI: f32 = f32::from_bits(0x3E22_F983);
/// The resampler's interpolation weight constant (≈ 1/65536, about one part in 2^21 off).
pub const WEIGHT: f32 = f32::from_bits(0x377F_FC9C);
// Credit: the sin / cos polynomials are XNA Math's `XMVectorSin` / `XMVectorCos` (the Xbox 360
// math library the title links; its successor is DirectXMath, Microsoft, MIT License,
// https://github.com/microsoft/DirectXMath). The coefficients are the Taylor terms ±1/n! as f32,
// read from the shipped image at the addresses below; the evaluation order follows the image.
/// `XMVectorSin` coefficients for V³ … V²³ (`0x822F97C4` … `0x822F97EC`).
pub const SIN: [u32; 11] = [
    0xBE2A_AAAB, 0x3C08_8889, 0xB950_0D01, 0x3638_EF1D, 0xB2D7_322B, 0x2F30_9231, 0xAB57_3F9F, 0x274A_963C, 0xA317_A4DA, 0x1EB8_DC78,
    0x9A3B_0DA1,
];
/// `XMVectorCos` coefficients for V² … V²² (`0x822F97F4` … `0x822F981C`).
pub const COS: [u32; 11] = [
    0xBF00_0000, 0x3D2A_AAAB, 0xBAB6_0B61, 0x37D0_0D01, 0xB493_F27E, 0x310F_76C8, 0xAD49_CBA5, 0x2957_3F9F, 0xA534_13C3, 0x20F2_A15D,
    0x9C86_71CB,
];

/// Normalised biquad coefficients {a1, a2, b0, b1, b2} (re-exported as
/// `skate_audio::dsp::biquad::Coefficients`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Coefficients {
    pub a1: f32,
    pub a2: f32,
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Plain,
    #[cfg(target_arch = "x86_64")]
    Fma,
}

/// Which compiled copy of the loops runs. [`Path::PLAIN`] always exists; [`Path::fma`] only when
/// the CPU supports the FMA copy's features.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Path(Kind);

/// Every feature the FMA copy is compiled with: `fma` and what rustc implies for it (`avx`, and
/// through it `sse4.2`, `sse4.1`, `ssse3`, `sse3`; `sse2` is the x86-64 baseline). `fma` alone is
/// only reported with OS support for the AVX register state; the rest are checked anyway.
#[cfg(target_arch = "x86_64")]
fn cpu_has_fma() -> bool {
    std::is_x86_feature_detected!("fma")
        && std::is_x86_feature_detected!("avx")
        && std::is_x86_feature_detected!("sse4.2")
        && std::is_x86_feature_detected!("sse4.1")
        && std::is_x86_feature_detected!("ssse3")
        && std::is_x86_feature_detected!("sse3")
}

impl Path {
    /// The copy compiled with the crate's normal target features (`mul_add` calls `fmaf`).
    pub const PLAIN: Path = Path(Kind::Plain);

    /// The hardware-FMA copy, if this CPU supports it.
    pub fn fma() -> Option<Path> {
        #[cfg(target_arch = "x86_64")]
        if cpu_has_fma() {
            return Some(Path(Kind::Fma));
        }
        None
    }

    pub fn is_fma(self) -> bool {
        self != Path::PLAIN
    }

    pub fn name(self) -> &'static str {
        if self.is_fma() { "fma" } else { "plain" }
    }
}

/// Generates, for each listed function of `body`: `plain::name` (normal features), `fma::name`
/// (`#[target_feature(enable = "fma")]`, x86-64 only), the method `Path::name` that runs the copy
/// the path names, and the free function `name` that runs the [`active`] path's copy. Both copies
/// inline the same `body::name`, so they are the same source.
macro_rules! kernels {
    ($($(#[$doc:meta])* fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;)*) => {
        /// The plain copies (the crate's normal target features).
        mod plain {
            #[allow(unused_imports)]
            use super::*;
            $(
                $(#[$doc])*
                pub fn $name($($arg: $ty),*) $(-> $ret)? {
                    body::$name($($arg),*)
                }
            )*
        }

        /// The hardware-FMA copies. Safe functions with a target feature: callable only from an
        /// `unsafe` block that has checked the CPU (see [`Path::fma`]).
        #[cfg(target_arch = "x86_64")]
        mod fma {
            #[allow(unused_imports)]
            use super::*;
            $(
                $(#[$doc])*
                #[target_feature(enable = "fma")]
                pub fn $name($($arg: $ty),*) $(-> $ret)? {
                    body::$name($($arg),*)
                }
            )*
        }

        impl Path {
            $(
                $(#[$doc])*
                #[inline]
                pub fn $name(self, $($arg: $ty),*) $(-> $ret)? {
                    match self.0 {
                        Kind::Plain => plain::$name($($arg),*),
                        #[cfg(target_arch = "x86_64")]
                        // SAFETY: a `Path` holding `Kind::Fma` is only made by `Path::fma`, after
                        // `cpu_has_fma` confirmed at run time that this CPU and OS support `fma`
                        // and every feature it implies, which are exactly the features
                        // `fma::$name` is compiled with. Its arguments are the safe ones of
                        // `plain::$name`; there is no other precondition.
                        Kind::Fma => unsafe { fma::$name($($arg),*) },
                    }
                }
            )*
        }

        $(
            $(#[$doc])*
            ///
            /// Runs the [`active`] path's copy.
            #[inline]
            pub fn $name($($arg: $ty),*) $(-> $ret)? {
                active().$name($($arg),*)
            }
        )*
    };
}

kernels! {
    /// TU3 Gain four-lane de-click kernel (`82B3C098`).
    fn gain_ramp(samples: &mut [f32], start: f32, step: f32);
    /// Direct Form I biquad over one block of one channel, in place; history {x1, x2, y1, y2}.
    fn biquad(k: &Coefficients, history: &mut [f32; 4], samples: &mut [f32]);
    /// [`biquad`] on two channels of one length in lockstep, each with its own history: each
    /// channel gets the bits [`biquad`] gives it, and the two feedback chains overlap.
    fn biquad2(k: &Coefficients, history: [&mut [f32; 4]; 2], samples: [&mut [f32]; 2]);
    /// [`biquad2`] on three channels.
    fn biquad3(k: &Coefficients, history: [&mut [f32; 4]; 3], samples: [&mut [f32]; 3]);
    /// [`biquad2`] on four channels.
    fn biquad4(k: &Coefficients, history: [&mut [f32; 4]; 4], samples: [&mut [f32]; 4]);
    /// The biquad's feedback loop with the feed-forward sum fixed at [`BIAS`] (silent input,
    /// settled inputs); `y` = {y1, y2}.
    fn biquad_feedback(k: &Coefficients, y: &mut [f32; 2], samples: &mut [f32]);
    /// `XMVectorSin` (one lane).
    fn sin(x: f32) -> f32;
    /// `XMVectorCos` (one lane).
    fn cos(x: f32) -> f32;
    /// The FrequencyShiftSsb oscillator mix: out = I · cos φₙ − Q · sin φₙ, φₙ = φ + n·Δ in
    /// retail's lanes of four.
    fn fss_mix(samples: &mut [f32], i: &[f32], q: &[f32], phi: f32, delta: f32);
    /// Linear 16.16 resampling of one channel from `frame(index)`.
    fn resample(out: &mut [f32], position: u64, frac: u32, step: u32, frame: impl FnMut(u64) -> f32);
}

const UNSET: u8 = 0;
const PLAIN: u8 = 1;
const FMA: u8 = 2;
static ACTIVE: AtomicU8 = AtomicU8::new(UNSET);

/// The process-wide path, chosen on first use (see [`init`]) and cached.
#[inline]
pub fn active() -> Path {
    match ACTIVE.load(Ordering::Relaxed) {
        PLAIN => Path::PLAIN,
        // FMA is only stored by `init` from a `Path` that `Path::fma` returned.
        #[cfg(target_arch = "x86_64")]
        FMA => Path(Kind::Fma),
        _ => init().path,
    }
}

/// What [`init`] picked and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub path: Path,
    /// The CPU supports the FMA copy.
    pub cpu_fma: bool,
    /// `SKATE_AUDIO_FMA` as set (None when unset).
    pub env: Option<String>,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "path={} cpu_fma={} SKATE_AUDIO_FMA={}", self.path.name(), self.cpu_fma, self.env.as_deref().unwrap_or("unset"))
    }
}

/// Pick the path from the CPU and an `SKATE_AUDIO_FMA` value: "0" forces the plain copy; anything
/// else (unset, "1") takes FMA when the CPU supports it.
pub fn choose(env: Option<&str>) -> Path {
    match env.map(str::trim) {
        Some("0") => Path::PLAIN,
        _ => Path::fma().unwrap_or(Path::PLAIN),
    }
}

/// Set the process-wide path directly (tests that render the same scene on both copies in one
/// process; a `Path` that names the FMA copy proves the CPU has it). Other threads pick the change
/// up on their next call; both copies give the same bits, apart from NaN payloads (see `tests.rs`).
pub fn force(path: Path) {
    ACTIVE.store(if path.is_fma() { FMA } else { PLAIN }, Ordering::Relaxed);
}

/// Make (or remake) the process-wide choice from the CPU and `SKATE_AUDIO_FMA`, cache it, and
/// return it. Call once at audio start (it reads the environment, so not from the render path);
/// [`active`] calls it on first use otherwise. Concurrent first calls compute the same answer.
pub fn init() -> Choice {
    let env = std::env::var("SKATE_AUDIO_FMA").ok();
    let path = choose(env.as_deref());
    ACTIVE.store(if path.is_fma() { FMA } else { PLAIN }, Ordering::Relaxed);
    Choice { path, cpu_fma: Path::fma().is_some(), env }
}
