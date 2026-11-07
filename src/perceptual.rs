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
    pub(crate) fn sample(&mut self) -> [f32; 2] {
        if self.is_idle() {
            return [0.0; 2];
        }
        let env = self.envelope.advance();
        let c = self.controls;
        let sweep = self.sine_at(self.lfo);
        self.lfo = (self.lfo + f64::from(0.03 + 3.97 * c[5] * c[5]) / f64::from(self.rate)).fract();
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
        };
        self.cursor = (self.cursor + 1) % DELAY;
        out.map(|x| x * env * self.velocity * 0.65)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MODES: [PerceptualTopology; 5] = [
        PerceptualTopology::MicroDelay,
        PerceptualTopology::SpectralSplit,
        PerceptualTopology::Dispersion,
        PerceptualTopology::NotchMotion,
        PerceptualTopology::VirtualPitch,
    ];
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
