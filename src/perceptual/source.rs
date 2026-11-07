//! Authored Fourier saw tables, prepared only during voice construction.
//! Adjacent octave tables crossfade before their highest harmonic reaches
//! 0.4 fs. Cubic lookup avoids a bright table's linear-interpolation images.
use std::sync::OnceLock;

const SIZE: usize = 4096;
const LEVELS: usize = 8;
pub(super) const HARMONICS: usize = 128;
static BANK: OnceLock<SawBank> = OnceLock::new();

pub(super) struct SawBank([[f32; SIZE]; LEVELS]);
impl std::fmt::Debug for SawBank {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SawBank(immutable 1..128 harmonic octave tables)")
    }
}
impl SawBank {
    pub(super) fn prepared() -> &'static Self {
        BANK.get_or_init(|| {
            let mut bank = Self([[0.0; SIZE]; LEVELS]);
            for level in 0..LEVELS {
                let count = 1 << level;
                for i in 0..SIZE {
                    let phase = std::f64::consts::TAU * i as f64 / SIZE as f64;
                    bank.0[level][i] = (1..=count)
                        .map(|h| -2.0 / std::f64::consts::PI * (phase * h as f64).sin() / h as f64)
                        .sum::<f64>() as f32;
                }
            }
            bank
        })
    }

    fn lookup(&self, level: usize, phase: f64) -> f32 {
        let x = (phase - phase.floor()) * SIZE as f64;
        let i = x as usize;
        let t = (x - i as f64) as f32;
        let table = &self.0[level];
        let a = table[(i + SIZE - 1) % SIZE];
        let b = table[i];
        let c = table[(i + 1) % SIZE];
        let d = table[(i + 2) % SIZE];
        b + 0.5 * t * (c - a + t * (2.0 * a - 5.0 * b + 4.0 * c - d + t * (3.0 * (b - c) + d - a)))
    }

    pub(super) fn sample(&self, phase: f64, increment: f32) -> f32 {
        let limit = 0.4 / increment.max(1e-9);
        let mut level = 0;
        while level + 1 < LEVELS && (1 << (level + 1)) as f32 <= limit {
            level += 1;
        }
        let current = self.lookup(level, phase);
        if level == 0 {
            return current * ((0.44 - increment) / 0.04).clamp(0.0, 1.0);
        }
        let previous = self.lookup(level - 1, phase);
        let blend = (limit / (1 << level) as f32 - 1.0).clamp(0.0, 1.0);
        previous + blend * (current - previous)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_crossfades_do_not_switch_tables_abruptly() {
        let bank = SawBank::prepared();
        for level in 0..=LEVELS {
            let increment = 0.4 / (1 << level) as f32;
            for phase in 0..257 {
                let phase = phase as f64 / 257.0;
                let a = bank.sample(phase, increment * (1.0 - 1e-6));
                let b = bank.sample(phase, increment * (1.0 + 1e-6));
                assert!((a - b).abs() < 0.0001);
            }
        }
    }

    #[test]
    fn independently_detuned_saws_reject_unintended_spectral_energy() {
        const N: usize = 8192;
        let bank = SawBank::prepared();
        for bin in [31, 211, 1103] {
            let mut intended = [false; N / 2];
            for base in [bin, bin + 1] {
                let limit = 0.4 * N as f32 / base as f32;
                let mut ceiling = 1;
                while ceiling * 2 <= HARMONICS && (ceiling * 2) as f32 <= limit {
                    ceiling *= 2;
                }
                for h in 1..=ceiling {
                    if base * h < (N as f32 * 0.44) as usize {
                        intended[base * h] = true;
                    }
                }
            }
            let samples: Vec<f64> = (0..N)
                .map(|i| {
                    f64::from(
                        0.55 * bank.sample((i * bin) as f64 / N as f64, bin as f32 / N as f32)
                            + 0.45
                                * bank.sample(
                                    0.31 + (i * (bin + 1)) as f64 / N as f64,
                                    (bin + 1) as f32 / N as f32,
                                ),
                    )
                })
                .collect();
            let energy: f64 = samples.iter().map(|x| x * x).sum();
            let mut reconstructed = vec![0.0; N];
            for (k, present) in intended.iter().enumerate() {
                if !present {
                    continue;
                }
                let omega = std::f64::consts::TAU * k as f64 / N as f64;
                let (mut re, mut im) = (0.0, 0.0);
                for (i, x) in samples.iter().enumerate() {
                    re += x * (omega * i as f64).cos();
                    im += x * (omega * i as f64).sin();
                }
                for (i, value) in reconstructed.iter_mut().enumerate() {
                    *value += 2.0 / N as f64
                        * (re * (omega * i as f64).cos() + im * (omega * i as f64).sin());
                }
            }
            let error: f64 = samples
                .iter()
                .zip(reconstructed)
                .map(|(x, y)| (x - y).powi(2))
                .sum();
            let db = 10.0 * (error.max(1e-30) / energy).log10();
            eprintln!("detuned saw pair bin {bin}: alias/residual {db:.2} dB");
            assert!(db < -75.0, "{bin}: {db} dB");
        }
    }
}
