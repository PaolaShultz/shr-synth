//! Authored perceptual synthesis. Fixed storage, linear signal paths, and
//! band-limited additive excitation; no sample-path transcendental setup.
use crate::{
    engine::{EngineError, envelope_config},
    envelope::Adsr,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerceptualTopology {
    MicroDelay,
    SpectralSplit,
    Dispersion,
    NotchMotion,
    VirtualPitch,
    CounterTide,
    VowelOrbit,
    PhaseBloom,
    SpectralCarousel,
    TidalMemory,
}
impl PerceptualTopology {
    fn is_swirl(self) -> bool {
        matches!(
            self,
            Self::CounterTide
                | Self::VowelOrbit
                | Self::PhaseBloom
                | Self::SpectralCarousel
                | Self::TidalMemory
        )
    }
}

/// Trapezoidal state-variable filter. Band output is damping-compensated;
/// LP/HP resonance is gain-compensated at the caller. No nonlinear feedback.
#[derive(Debug, Default, Clone, Copy)]
struct MovingFilter {
    s1: f32,
    s2: f32,
}
impl MovingFilter {
    fn tick(&mut self, input: f32, g: f32, damping: f32) -> [f32; 3] {
        let v3 = input - self.s2;
        let band = (self.s1 + g * v3) / (1.0 + g * (g + damping));
        let low = self.s2 + g * band;
        self.s1 = 2.0 * band - self.s1;
        self.s2 = 2.0 * low - self.s2;
        [low, damping * band, input - damping * band - low]
    }
}

const TABLE: usize = 2048;
const DELAY: usize = 2048;
#[derive(Debug)]
pub(crate) struct PerceptualVoice {
    rate: f32,
    topology: PerceptualTopology,
    controls: [f32; 15],
    envelope: Adsr,
    velocity: f32,
    frequency: f32,
    pitch_ratio: f32,
    phase: f64,
    lfo: f64,
    secondary_lfo: f64,
    slow_rates: [f32; 129],
    filters: [MovingFilter; 3],
    sine: [f32; TABLE + 1],
    delay: [f32; DELAY],
    cursor: usize,
    low: f32,
    allpass: [[f32; 4]; 2],
}
impl PerceptualVoice {
    pub(crate) fn new(rate: f32, topology: PerceptualTopology) -> Result<Self, EngineError> {
        if !rate.is_finite() || rate <= 0.0 {
            return Err(EngineError::InvalidSampleRate);
        }
        Ok(Self {
            rate,
            topology,
            controls: [0.5; 15],
            envelope: Adsr::new(rate, envelope_config(0.1, 0.3, 0.7, 0.2))
                .map_err(|_| EngineError::InvalidSampleRate)?,
            velocity: 0.0,
            frequency: 440.0,
            pitch_ratio: 1.0,
            phase: 0.0,
            lfo: 0.0,
            secondary_lfo: 0.0,
            slow_rates: std::array::from_fn(|i| 0.005 * 400.0_f32.powf(i as f32 / 128.0)),
            filters: [MovingFilter::default(); 3],
            sine: std::array::from_fn(|i| {
                (std::f64::consts::TAU * i as f64 / TABLE as f64).sin() as f32
            }),
            delay: [0.0; DELAY],
            cursor: 0,
            low: 0.0,
            allpass: [[0.0; 4]; 2],
        })
    }
    pub(crate) fn set_controls(&mut self, c: [f32; 15]) {
        self.controls = c;
        self.envelope
            .set_config(envelope_config(c[8], c[9], c[10], c[11]));
    }
    pub(crate) fn set_pitch_ratio(&mut self, ratio: f32) {
        self.pitch_ratio = ratio;
    }
    pub(crate) fn note_on(&mut self, note: u8, velocity: f32) {
        self.reset();
        self.frequency = 440.0 * 2.0_f32.powf((f32::from(note) - 69.0) / 12.0);
        self.velocity = velocity.clamp(0.0, 1.0);
        self.envelope.restart();
    }
    pub(crate) fn note_off(&mut self) {
        self.envelope.note_off();
    }
    pub(crate) fn is_idle(&self) -> bool {
        self.envelope.is_idle()
    }
    pub(crate) fn reset(&mut self) {
        self.envelope.reset();
        self.phase = 0.0;
        self.lfo = 0.0;
        self.secondary_lfo = 0.0;
        self.filters = [MovingFilter::default(); 3];
        self.low = 0.0;
        self.delay.fill(0.0);
        self.cursor = 0;
        self.allpass = [[0.0; 4]; 2];
    }
    fn sine_at(&self, phase: f64) -> f32 {
        let x = (phase - phase.floor()) * TABLE as f64;
        let i = x as usize;
        self.sine[i] + (self.sine[i + 1] - self.sine[i]) * (x - i as f64) as f32
    }
    fn delayed(&self, seconds: f32) -> f32 {
        let d = (seconds * self.rate).clamp(1.0, (DELAY - 2) as f32);
        let n = d as usize;
        let f = d - n as f32;
        let a = (self.cursor + DELAY - n) % DELAY;
        let b = (a + DELAY - 1) % DELAY;
        self.delay[a] + (self.delay[b] - self.delay[a]) * f
    }
    fn slow_rate(&self, control: f32) -> f32 {
        let x = control.clamp(0.0, 1.0) * 128.0;
        let i = (x as usize).min(127);
        self.slow_rates[i] + (self.slow_rates[i + 1] - self.slow_rates[i]) * (x - i as f32)
    }
    fn filter_g(&self, position: f32) -> f32 {
        let p = position.clamp(0.0, 1.0);
        let hz = (60.0 + 10000.0 * p * p * p).min(self.rate * 0.18);
        // tan(pi*f/fs) from the prepared sine table, bounded away from its pole.
        let phase = f64::from(hz / (2.0 * self.rate));
        self.sine_at(phase) / self.sine_at(phase + 0.25)
    }
    fn swirl(&mut self, source: f32, env: f32, sweep: f32) -> [f32; 2] {
        let c = self.controls;
        let quadrature = self.sine_at(self.lfo + 0.25);
        let drift = self.sine_at(self.secondary_lfo);
        self.secondary_lfo = (self.secondary_lfo
            + f64::from(self.slow_rate(c[5]) * 0.618034) / f64::from(self.rate))
        .fract();
        let center = c[0] + 0.35 * c[4] * (env - 0.5);
        let depth = 0.32 * c[6];
        let a = center + depth * sweep;
        let b = center - depth * sweep;
        let q = center + depth * quadrature;
        let damping = 2.0 - 1.84 * c[1];
        let gain = 1.0 / (1.0 + 2.0 * c[1]);
        let ga = self.filter_g(a);
        let gb = self.filter_g(b);
        let gq = self.filter_g(q);
        let width = c[7];
        let pair = match self.topology {
            PerceptualTopology::CounterTide => {
                // Parallel opposing low/high filters; Body moves their balance.
                let low = self.filters[0].tick(source, ga, damping)[0] * gain;
                let high = self.filters[1].tick(source, gb, damping)[2] * gain;
                let x = low * (1.0 - 0.75 * c[3]);
                let y = high * (0.25 + 0.75 * c[3]);
                [x + 0.35 * y, y + 0.35 * x]
            }
            PerceptualTopology::VowelOrbit => {
                // Three parallel resonant bands, independently moving centers.
                let spread = 0.08 + 0.2 * c[3];
                let g1 = self.filter_g(a - spread);
                let g2 = self.filter_g(q);
                let g3 = self.filter_g(center + spread + depth * drift);
                let x = self.filters[0].tick(source, g1, damping)[1];
                let y = self.filters[1].tick(source, g2, damping)[1];
                let z = self.filters[2].tick(source, g3, damping)[1];
                [
                    0.65 * x + 0.35 * y + 0.18 * source,
                    0.65 * z + 0.35 * y + 0.18 * source,
                ]
            }
            PerceptualTopology::PhaseBloom => {
                // Resonant low-pass feeds dry + four-stage dispersive branches.
                let low = self.filters[0].tick(source, ga, damping)[0] * gain;
                let mut pair = [0.0; 2];
                for (ch, output) in pair.iter_mut().enumerate() {
                    let mut x = low;
                    for stage in 0..4 {
                        let offset = if ch == 0 { quadrature } else { -quadrature };
                        let g = self.filter_g(
                            center
                                + stage as f32 * (0.04 + 0.06 * c[3])
                                + depth * (0.65 * offset + 0.35 * drift),
                        );
                        let coefficient = (g - 1.0) / (g + 1.0);
                        let y = coefficient * x + self.allpass[ch][stage];
                        self.allpass[ch][stage] = x - coefficient * y;
                        x = y;
                    }
                    *output = 0.5 * low + (0.15 + 0.35 * c[3]) * x;
                }
                pair
            }
            PerceptualTopology::SpectralCarousel => {
                // Rotating low/band/high image; filter centers also move.
                let low = self.filters[0].tick(source, ga, damping)[0] * gain;
                let high = self.filters[1].tick(source, gb, damping)[2] * gain;
                let band = self.filters[2].tick(source, gq, damping)[1];
                let rotation = c[6] * (sweep + c[3] * drift) / (1.0 + c[3]);
                let x = low * (0.5 + 0.4 * rotation) + high * (0.5 - 0.4 * rotation);
                let y = low * (0.5 - 0.4 * rotation) + high * (0.5 + 0.4 * rotation);
                [x + 0.4 * c[3] * band, y + 0.4 * c[3] * band]
            }
            PerceptualTopology::TidalMemory => {
                // Feed-forward unequal combs feed separate resonant filters.
                // A slower coupled phase moves delay relationships, never feedback.
                self.delay[self.cursor] = source;
                let time = 0.0004 + 0.006 * c[3];
                let ta = time * (1.0 + 0.3 * c[6] * quadrature);
                let tb = time * (1.37 + 0.25 * c[6] * drift);
                let x = 0.55 * source + 0.45 * self.delayed(ta);
                let y = 0.55 * source - 0.45 * self.delayed(tb);
                let left = self.filters[0].tick(x, ga, damping)[0] * gain;
                let right = self.filters[1].tick(y, gq, damping)[0] * gain;
                self.cursor = (self.cursor + 1) % DELAY;
                [left, right]
            }
            _ => [0.0; 2],
        };
        let mid = 0.5 * (pair[0] + pair[1]);
        let side = 0.5 * (pair[0] - pair[1]) * width;
        [mid + side, mid - side]
    }
    pub(crate) fn sample(&mut self) -> [f32; 2] {
        if self.is_idle() {
            return [0.0; 2];
        }
        let env = self.envelope.advance();
        let c = self.controls;
        let sweep = self.sine_at(self.lfo);
        let rate = if self.topology.is_swirl() {
            self.slow_rate(c[5])
        } else {
            0.03 + 3.97 * c[5] * c[5]
        };
        self.lfo = (self.lfo + f64::from(rate) / f64::from(self.rate)).fract();
        let position = (c[0] + 0.65 * c[4] * (env - 0.5) + 0.25 * c[6] * sweep).clamp(0.0, 1.0);
        let frequency = self.frequency * self.pitch_ratio;
        self.phase = (self.phase + f64::from(frequency / self.rate)).fract();
        let mut source = 0.0;
        let mut odd = 0.0;
        let mut even = 0.0;
        // Fade partials before Nyquist, including during continuous pitch bend.
        // Absolute harmonic sum is bounded without a limiter or normalizer.
        for h in 1..=12 {
            let hf = h as f32;
            let band = ((0.44 * self.rate - frequency * hf) / (0.04 * self.rate)).clamp(0.0, 1.0);
            let bright = (1.0 - c[2]) / (hf * hf) + c[2] / hf;
            let mut weight = bright * band;
            if h == 1 {
                weight *= 0.2 + 0.8 * c[3];
            }
            if self.topology == PerceptualTopology::VirtualPitch {
                // Remove the physical fundamental and scan a harmonic cluster.
                let center = 2.0 + 7.0 * position;
                let distance = (hf - center).abs();
                weight *= if h == 1 {
                    c[3] * (1.0 - c[1])
                } else {
                    1.0 / (1.0 + distance * distance * (0.2 + 2.8 * c[1]))
                };
            }
            let partial = self.sine_at(self.phase * f64::from(h)) * weight * 0.22;
            source += partial;
            if h % 2 == 0 {
                even += partial;
            } else {
                odd += partial;
            }
        }
        if self.topology.is_swirl() {
            let out = self.swirl(source, env, sweep);
            return out.map(|x| x * env * self.velocity * 0.65);
        }
        let cutoff = 100.0 + 12000.0 * (0.03 + 0.97 * c[2]) * (0.15 + 0.85 * position);
        let alpha = cutoff / (cutoff + self.rate / std::f32::consts::TAU);
        self.low += alpha * (source - self.low);
        let focused = (1.0 - c[1]) * source + c[1] * self.low;
        self.delay[self.cursor] = focused;
        let width = c[7];
        let out = match self.topology {
            PerceptualTopology::MicroDelay => {
                let t = 0.0003 + 0.003 * position;
                let a = 0.6 * self.delayed(t) + 0.4 * self.delayed(t * 1.71);
                let b = 0.6 * self.delayed(t * 1.27 + 0.0002) + 0.4 * self.delayed(t * 2.13);
                [
                    0.65 * focused + 0.35 * ((1.0 - width) * a + width * b),
                    0.65 * focused + 0.35 * a,
                ]
            }
            PerceptualTopology::SpectralSplit => {
                // Complementary low/high bands; mono sum remains the source.
                let side = (self.low - (source - self.low)) * (0.15 + 0.7 * c[1]) * width;
                [source + side, source - side]
            }
            PerceptualTopology::Dispersion | PerceptualTopology::NotchMotion => {
                let mut out = [0.0; 2];
                for (ch, output) in out.iter_mut().enumerate() {
                    let spread = if ch == 0 { -1.0 } else { 1.0 };
                    let mut x = focused;
                    for stage in 0..4 {
                        let f = 150.0
                            + 6500.0
                                * (position + spread * 0.12 * width + stage as f32 * 0.055)
                                    .clamp(0.0, 1.0);
                        let g = f / (f + self.rate / std::f32::consts::PI);
                        let a = (2.0 * g - 1.0).clamp(-0.98, 0.98);
                        let y = a * x + self.allpass[ch][stage];
                        self.allpass[ch][stage] = x - a * y;
                        x = y;
                    }
                    *output = if self.topology == PerceptualTopology::Dispersion {
                        (1.0 - 0.8 * c[1]) * focused + 0.8 * c[1] * x
                    } else {
                        // Feed-forward subtraction makes moving spectral notches.
                        0.5 * (focused - x) * (0.3 + 0.7 * c[1]) + 0.2 * c[3] * source
                    };
                }
                out
            }
            PerceptualTopology::VirtualPitch => {
                let side = (odd - even) * width * 0.6;
                [focused + side, focused - side]
            }
            // Handled before the legacy path; exhaustive without callback panic.
            PerceptualTopology::CounterTide
            | PerceptualTopology::VowelOrbit
            | PerceptualTopology::PhaseBloom
            | PerceptualTopology::SpectralCarousel
            | PerceptualTopology::TidalMemory => [0.0; 2],
        };
        self.cursor = (self.cursor + 1) % DELAY;
        out.map(|x| x * env * self.velocity * 0.65)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MODES: [PerceptualTopology; 10] = [
        PerceptualTopology::MicroDelay,
        PerceptualTopology::SpectralSplit,
        PerceptualTopology::Dispersion,
        PerceptualTopology::NotchMotion,
        PerceptualTopology::VirtualPitch,
        PerceptualTopology::CounterTide,
        PerceptualTopology::VowelOrbit,
        PerceptualTopology::PhaseBloom,
        PerceptualTopology::SpectralCarousel,
        PerceptualTopology::TidalMemory,
    ];
    #[test]
    fn swirl_rate_has_logarithmic_travel_and_phase_continuity() {
        let mut v = PerceptualVoice::new(48000.0, PerceptualTopology::CounterTide).unwrap();
        for (pot, hz) in [
            (0.0, 0.005),
            (0.25, 0.02236068),
            (0.5, 0.1),
            (0.75, 0.4472136),
            (1.0, 2.0),
        ] {
            assert!((v.slow_rate(pot) - hz).abs() < 1e-6);
        }
        let mut last = v.slow_rate(0.0);
        for cc in 1..=127 {
            let rate = v.slow_rate(cc as f32 / 127.0);
            assert!(rate > last && rate / last < 1.05);
            last = rate;
        }
        v.note_on(48, 1.0);
        let mut c = [0.5; 15];
        c[5] = 0.0;
        v.set_controls(c);
        for _ in 0..48000 {
            v.sample();
        }
        assert!((v.lfo - 0.005).abs() < 1e-7);
        c[5] = 1.0;
        v.set_controls(c);
        v.sample();
        assert!((v.lfo - (0.005 + 2.0 / 48000.0)).abs() < 1e-7);
    }

    #[test]
    fn swirl_long_sustain_resonance_and_width_remain_bounded() {
        for mode in MODES.into_iter().filter(|m| m.is_swirl()) {
            let mut v = PerceptualVoice::new(12000.0, mode).unwrap();
            let mut c = [0.7; 15];
            c[1] = 1.0;
            c[5] = 0.5; // Ten-second primary cycle; secondary remains asynchronous.
            c[6] = 1.0;
            c[8] = 0.0;
            c[9] = 0.0;
            c[10] = 1.0;
            v.set_controls(c);
            v.note_on(48, 1.0);
            let mut windows = [0.0f64; 12];
            for (window, energy) in windows.iter_mut().enumerate() {
                for _ in 0..12000 {
                    let x = v.sample();
                    assert!(x.iter().all(|v| v.is_finite() && v.abs() < 1.0), "{mode:?}");
                    *energy += f64::from(x[0] * x[0] + x[1] * x[1]);
                }
                assert!(*energy > 0.001, "{mode:?} silent window {window}");
            }
            let min = windows.iter().copied().fold(f64::INFINITY, f64::min);
            let max = windows.iter().copied().fold(0.0, f64::max);
            assert!(max > min * 1.05, "{mode:?}: no sustained evolution");
            c[7] = 0.0;
            v.set_controls(c);
            for _ in 0..1024 {
                let x = v.sample();
                assert_eq!(x[0], x[1]);
            }
        }
    }

    #[test]
    fn stationary_full_paths_reject_alias_energy_outside_authored_partials() {
        // Coherent prime FFT bins avoid counting folded components as intended
        // harmonics. Fit ONLY the twelve authored partials below the guard band.
        const N: usize = 16384;
        for mode in MODES {
            for bin in [31, 211, 1103, 3151] {
                let mut v = PerceptualVoice::new(65536.0, mode).unwrap();
                let mut c = [0.5; 15];
                c[2] = 1.0;
                c[4] = 0.0;
                c[6] = 0.0;
                c[8] = 0.0;
                c[9] = 0.0;
                c[10] = 1.0;
                v.set_controls(c);
                v.note_on(60, 1.0);
                v.frequency = (bin * 4) as f32;
                for _ in 0..N {
                    v.sample();
                }
                let samples: Vec<_> = (0..N).map(|_| v.sample()).collect();
                for ch in 0..2 {
                    let mut reconstructed = vec![0.0f64; N];
                    for h in 1..=12 {
                        if bin * h >= N / 2 {
                            continue;
                        }
                        let omega = std::f64::consts::TAU * (bin * h) as f64 / N as f64;
                        let (mut a, mut b) = (0.0, 0.0);
                        for (i, x) in samples.iter().enumerate() {
                            a += f64::from(x[ch]) * (omega * i as f64).sin();
                            b += f64::from(x[ch]) * (omega * i as f64).cos();
                        }
                        a *= 2.0 / N as f64;
                        b *= 2.0 / N as f64;
                        for (i, x) in reconstructed.iter_mut().enumerate() {
                            *x += a * (omega * i as f64).sin() + b * (omega * i as f64).cos();
                        }
                    }
                    let energy: f64 = samples.iter().map(|x| f64::from(x[ch]).powi(2)).sum();
                    let error: f64 = samples
                        .iter()
                        .zip(reconstructed)
                        .map(|(x, y)| (f64::from(x[ch]) - y).powi(2))
                        .sum();
                    let db = 10.0 * (error / energy.max(1e-30)).max(1e-30).log10();
                    eprintln!("{mode:?} bin {bin} channel {ch}: alias/residual {db:.2} dB");
                    assert!(energy > 1e-9 && db < -75.0, "{mode:?} bin {bin}: {db} dB");
                }
            }
        }
    }
    #[test]
    fn virtual_pitch_removes_fundamental_and_spectral_split_preserves_mono() {
        let mut v = PerceptualVoice::new(48000.0, PerceptualTopology::VirtualPitch).unwrap();
        let mut c = [0.5; 15];
        c[3] = 0.0;
        c[4] = 0.0;
        c[6] = 0.0;
        c[8] = 0.0;
        c[9] = 0.0;
        c[10] = 1.0;
        v.set_controls(c);
        v.note_on(60, 1.0);
        v.frequency = 200.0;
        for _ in 0..4800 {
            v.sample();
        }
        let samples: Vec<_> = (0..4800)
            .map(|_| {
                let x = v.sample();
                (x[0] + x[1]) * 0.5
            })
            .collect();
        let m = crate::analysis::measure_harmonic_system(&samples, 48000.0, 200.0).unwrap();
        assert!(m.fundamental_db < -90.0 && m.rms > 0.001, "{m:?}");
        let mut a = PerceptualVoice::new(48000.0, PerceptualTopology::SpectralSplit).unwrap();
        let mut b = PerceptualVoice::new(48000.0, PerceptualTopology::SpectralSplit).unwrap();
        c[7] = 0.0;
        a.set_controls(c);
        c[7] = 1.0;
        b.set_controls(c);
        a.note_on(48, 0.8);
        b.note_on(48, 0.8);
        for _ in 0..4800 {
            let x = a.sample();
            let y = b.sample();
            assert!(((x[0] + x[1]) - (y[0] + y[1])).abs() < 1e-6);
        }
    }
}
