# Offline audio fitter

Given a reference WAV and an adjustable sound generator, this tool repeatedly
renders candidates and searches for parameters that reduce their measured
difference. It saves the best controls and a reproducible, level-matched A/B.
It also compares two existing WAVs without fitting.

Production engines, factory patches and the live host are unchanged. No JACK,
ALSA, playback, network service, model API or third-party sample bank is used.
Python arrays and subprocesses allocate freely **outside** the audio callback.

## Setup and normal tool tests

From the repository root, with Python 3.11 or newer:

```sh
python3 -m venv artifacts/audio-fit-env
artifacts/audio-fit-env/bin/python -m pip install --only-binary=:all: -r tools/audio_fit/requirements.txt
artifacts/audio-fit-env/bin/python -m unittest discover -s tools/audio_fit -p 'test_*.py'
```

The short tests are the normal suite for this development tool. They generate
their own bounded, sub-Nyquist sine fixtures in temporary directories. They
check parameter recovery, deterministic search, invalid candidates, stereo,
tails, WAV encodings, exports, preset preservation and renderer failures.
No copyrighted recording is a fixture. Rust's normal production suite stays
separate; no Python package becomes a production dependency.

## Compare two samples

```sh
artifacts/audio-fit-env/bin/python tools/audio_fit/fit.py compare \
  /path/to/reference.wav /path/to/candidate.wav \
  --out artifacts/sample-comparison
```

Supply mono or stereo PCM/float WAV excerpts, 8–192 kHz, up to 30 seconds each.
The comparison resamples the candidate to the reference rate if needed and
pads the shorter excerpt with silence, preserving extra tails as errors.
Both files need the same musical time origin. There is no automatic shifting,
time stretching, pitch correction or trimming of attacks. Use an exposed
single note or the **same** actual phrase, register, gates and dynamics.

The output directory must be new. Generated data inside this repository is
restricted to ignored `artifacts/`; original files are read-only.

## Fit a playable SHR Synth preset

Put a job definition under `artifacts/`, for example `artifacts/fit-job.json`.
Paths are relative to that JSON file, or absolute. The following is a usage
example, not a Temple patch or reference transcription:

```json
{
  "renderer": {
    "kind": "shr-synth",
    "binary": "../target/release/shr-synth",
    "preset": "../presets/reference.mojsint",
    "note": 60,
    "velocity": 0.8
  },
  "parameters": {
    "macros.color": {"min": 0.0, "max": 1.0, "initial": 0.4125},
    "macros.attack": {"min": 0.0, "max": 0.5, "initial": 0.18094}
  }
}
```

```sh
artifacts/audio-fit-env/bin/python tools/audio_fit/fit.py fit \
  /path/to/reference.wav artifacts/fit-job.json \
  --out artifacts/fitted-source --budget 300 --seed 7
```

This adapter changes only explicitly listed existing `[macros]` values, bounded
within 0–1. All other preset text and model identity are preserved. Use the
native keys for the chosen model. The existing release executable must exist;
the fitter neither builds nor replaces it. Discrete model/patch selection is
not searched: compare different mechanisms in separate jobs.

**The existing SHR CLI renders one note and sends NoteOff at 80% of total
duration.** This adapter cannot express arbitrary gates or phrases. Match that
gate in controlled tests; use the custom renderer below for real phrases or
other gates. It does not infer MIDI from audio. An unmatched phrase/gate can
make the optimizer distort the sound to compensate for performance errors.

Outputs:

- `best.mojsint`: playable source settings for the SHR adapter.
- `best-parameters.json`: exact selected controls for either adapter.
- `best-native.wav`, `initial-native.wav`: source audio at native output gain.
- `reference.wav`, `candidate.wav`: compared audio after one candidate gain
  and one common peak-protection gain, documented in the report.
- `ab.wav`: reference, 0.5 s silence, best candidate, 0.5 s silence.
- `report.json`: source/config/binary/tool hashes, dependency versions, seed,
  bounds, region scores, rejected trials, every search evaluation and gains.

The exported preset keeps its native output gain. The comparison gain is
external and recorded; it is not silently written into that preset.

## Use an isolated prototype or an actual phrase

Replace `renderer` with a command definition. Keep the same parameter map:

```json
{
  "kind": "command",
  "argv": ["/absolute/path/python", "/absolute/path/render.py",
           "--parameters", "{params}", "--wav", "{output}",
           "--sample-rate", "{sample_rate}", "--seconds", "{seconds}"],
  "inputs": ["/absolute/path/render.py", "/absolute/path/phrase.json"],
  "timeout_seconds": 30
}
```

The executable receives a JSON file containing just the current parameter
values. It must reset deterministically and write a mono/stereo WAV at the
requested rate and duration (one frame rounding tolerance). Define the real
note/gate/velocity sequence in the renderer's inputs. List all code, phrase,
sample and configuration files in `inputs` so the report hashes them. Paths
and arguments are executed directly as an argv array, without a shell.
Use trusted offline renderers; the command adapter is not a sandbox.

Two identical baseline renders are required before search. The winning render
is repeated and checked against its exact decoded-sample hash and score.
Timeouts, crashes, missing output, silence and non-finite candidates cannot
win. Intermediate render files are removed; no batch of trial WAVs is kept.
The render budget bounds search evaluations; three additional renders verify
the initial and winning settings. A renderer timeout bounds each invocation.

This interface lets a different synthesis mechanism be tested without forcing
a factory patch to fit. A fixed candidate WAV alone has no synth controls to
optimize; it can be compared here, or used by a separately authored sampler
renderer after its content licence is reviewed.

## Metric and interpretation

The metric uses windowed spectra at three FFT sizes corresponding approximately
to 8, 32 and 128 ms, plus a 5 ms amplitude envelope. Left, right, mid and side
spectra preserve stereo information. Each region combines relative spectral
error (weight 0.5), log-magnitude error (0.25) and envelope error (0.25).

Default region boundaries are attack 0–80 ms, body 80–400 ms and tail after
400 ms. Empty regions are omitted; remaining regions have equal weights.
Set `--attack-end` and `--body-end` in seconds to reflect the excerpt. These
are user-defined comparison windows, not detected envelope measurements.
For a phrase, choose section boundaries deliberately; these labels no longer
describe every note's attack. One global stereo RMS gain applies to the whole
candidate, never separate gains for each region, channel or frequency band.

Lower loss means a closer result **under this metric**. It is not a percentage
of perceptual similarity. RMS matching is reproducible level matching, not
equal perceived loudness. Phase differences are largely ignored by spectral
magnitudes; pitch errors can have local minima, and masked parts of a full mix
cannot be identified by this score. Added effects, noise and phase tricks can
still exploit imperfect metrics. Do source fitting with effects disabled,
then evaluate effects separately. No effect processor is added by this tool.

Search uses seeded SciPy differential evolution with explicit bounds and a
hard evaluation budget. It retains the initial candidate if nothing improves.
There is no global-optimum, original-instrument or listening-fidelity claim.
Test another note, velocity and phrase after fitting to detect overfitting.
Several different source mechanisms may produce similar measured results.

## Opt-in end-to-end recovery proof

```sh
artifacts/audio-fit-env/bin/python tools/audio_fit/self_check.py \
  --binary target/release/shr-synth --out artifacts/audio-fit-proof-new
```

This deliberately changes two controls on the project's own Model D reference
patch. It fits note 60 for 120 evaluations and checks note 67 without refitting.
All resulting presets, audio and reports stay disposable. Run this proof when
the fitter, renderer protocol or its protected assumption changes; it is not
part of routine production tests. It is unrelated to Temple's timbre.

## Primary references and licensing

- Ryuichi Yamamoto, Eunwoo Song and Jae-Min Kim, *Parallel WaveGAN: A fast
  waveform generation model based on generative adversarial networks with
  multi-resolution spectrogram*, ICASSP 2020; first submitted October 2019,
  [arXiv:1910.11480](https://arxiv.org/abs/1910.11480). Prior art for comparing
  audio at multiple spectral resolutions, not validation of this fitter's
  particular windows, weights or musical accuracy. No model, source, figures,
  prose or audio from the paper is imported.
- SciPy contributors, [differential_evolution documentation](https://docs.scipy.org/doc/scipy/reference/generated/scipy.optimize.differential_evolution.html):
  bounded derivative-free search, seeding and evaluation behavior. We call
  the installed package; no implementation is copied into this repository.
- NumPy Developers, [NumPy licence](https://numpy.org/doc/stable/license.html),
  and SciPy Developers, installed wheel `LICENSE.txt`: BSD-3-Clause core
  packages. Binary wheels retain their separate bundled-library notices,
  including OpenBLAS/LAPACK and GCC runtime exceptions/LGPL components.
  These packages are local development dependencies, not shipped with SHR
  Synth. See the root `THIRD_PARTY.md` for the production boundary.

Reference audio stays owned by its rights holder. Accepting a WAV for comparison
does not license its reuse as sampler content or authorize publication of the
exported reference/A/B files. No external sample or factory bank was imported.
