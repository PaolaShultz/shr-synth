#!/usr/bin/env python3
"""Offline parameter fitting. Numerical evidence only; never invokes a live host."""

import argparse
from dataclasses import dataclass
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import tomllib

# Keep offline optimization from claiming every BLAS worker on a music machine.
os.environ.setdefault('OPENBLAS_NUM_THREADS', '1')
os.environ.setdefault('OMP_NUM_THREADS', '1')

import numpy as np
import scipy
from scipy.io import wavfile
from scipy.optimize import differential_evolution
from scipy.signal import resample_poly

ROOT = Path(__file__).resolve().parents[2]
EPS = 1e-12
MAX_SECONDS = 30


@dataclass
class Audio:
    rate: int
    data: np.ndarray

    def __post_init__(self):
        self.data = np.asarray(self.data, dtype=np.float64)
        if self.data.ndim == 1:
            self.data = self.data[:, None]
        if self.data.ndim != 2 or self.data.shape[1] not in (1, 2):
            raise ValueError('audio must be mono or stereo')
        if self.data.shape[1] == 1:
            self.data = np.repeat(self.data, 2, axis=1)
        if not 8000 <= self.rate <= 192000:
            raise ValueError('sample rate must be between 8000 and 192000 Hz')
        if not 32 <= len(self.data) <= self.rate * MAX_SECONDS:
            raise ValueError(f'audio must contain 32 samples to {MAX_SECONDS} seconds')
        if not np.isfinite(self.data).all():
            raise ValueError('audio contains NaN or infinity')
        if np.max(np.abs(self.data)) > 100:
            raise ValueError('audio exceeds the offline numerical safety bound')


def rms(x):
    return float(np.sqrt(np.mean(x * x)))


def read_audio(path):
    path = Path(path)
    if path.stat().st_size > 128 * 1024 * 1024:
        raise ValueError('select a WAV excerpt smaller than 128 MiB')
    rate, x = wavfile.read(path)
    if x.dtype == np.uint8:
        x = (x.astype(np.float64) - 128) / 128
    elif np.issubdtype(x.dtype, np.signedinteger):
        # scipy left-justifies 24-bit PCM into int32.
        x = x.astype(np.float64) / (float(np.iinfo(x.dtype).max) + 1)
    elif not np.issubdtype(x.dtype, np.floating):
        raise ValueError('unsupported WAV encoding')
    return Audio(int(rate), x)


def same_rate(audio, rate):
    if audio.rate == rate:
        return audio
    divisor = math.gcd(audio.rate, rate)
    return Audio(rate, resample_poly(audio.data, rate // divisor,
                                    audio.rate // divisor, axis=0))


def pad(x, n):
    return np.pad(x, ((0, n - len(x)), (0, 0)))


def aligned_pair(reference, candidate):
    candidate = same_rate(candidate, reference.rate)
    n = max(len(reference.data), len(candidate.data))
    a, b = pad(reference.data, n), pad(candidate.data, n)
    a_rms, b_rms = rms(a), rms(b)
    if a_rms < 1e-10 or b_rms < 1e-10:
        raise ValueError('reference and candidate must both be non-silent')
    gain = a_rms / b_rms
    return a, b * gain, gain


def spectrum(x, size):
    # L/R preserve panning; mid/side preserve width and interchannel cancellation.
    x = np.column_stack([x, x.mean(axis=1), (x[:, 0] - x[:, 1]) * 0.5])
    x = np.pad(x, ((size // 2, size // 2), (0, 0)))
    frames = np.lib.stride_tricks.sliding_window_view(x, size, axis=0)[::size // 4]
    return np.abs(np.fft.rfft(frames * np.hanning(size), axis=-1))


def envelope(x, step):
    x = np.pad(x, ((0, (-len(x)) % step), (0, 0)))
    return np.sqrt(np.mean(x.reshape(-1, step, 2) ** 2, axis=1))


class Metric:
    """Gain invariant, time-resolved spectral and envelope distance.

    No time warping, phase alignment, per-band gain or per-region normalization.
    Fixed region boundaries are relative to the supplied file's time origin.
    """

    def __init__(self, reference, attack_end=0.08, body_end=0.4):
        if not 0 < attack_end < body_end <= MAX_SECONDS:
            raise ValueError('require 0 < attack-end < body-end <= 30 seconds')
        if rms(reference.data) < 1e-10:
            raise ValueError('reference must be non-silent')
        self.reference = reference
        self.boundaries = [round(reference.rate * attack_end),
                           round(reference.rate * body_end)]
        self.sizes = [2 ** round(math.log2(reference.rate * seconds))
                      for seconds in (0.008, 0.032, 0.128)]
        self.cache = {}

    def features(self, x):
        return ([spectrum(x, size) for size in self.sizes],
                envelope(x, max(1, round(self.reference.rate * 0.005))))

    def compare(self, candidate):
        a, b, gain = aligned_pair(self.reference, candidate)
        level = rms(a)
        a, b = a / level, b / level
        cuts = [0, *[min(len(a), c) for c in self.boundaries], len(a)]
        regions = {}
        for name, start, end in zip(('attack', 'body', 'tail'), cuts, cuts[1:]):
            if start == end:
                continue
            key = (len(a), start, end)
            if key not in self.cache:
                self.cache[key] = self.features(a[start:end])
            ref_spectra, ref_env = self.cache[key]
            spectra, env = self.features(b[start:end])
            convergence, logarithmic = [], []
            for ref, actual in zip(ref_spectra, spectra):
                convergence.append(float(np.linalg.norm(actual - ref)
                                         / max(np.linalg.norm(ref), 1.0)))
                floor = max(float(np.max(ref)) * 0.001, 0.01)
                logarithmic.append(float(np.mean(np.abs(np.log1p(actual / floor)
                                                        - np.log1p(ref / floor)))))
            spectral_error = float(np.mean(convergence))
            log_error = float(np.mean(logarithmic))
            env_error = float(np.linalg.norm(env - ref_env) / max(np.linalg.norm(ref_env), 0.1))
            regions[name] = {'spectral': spectral_error, 'log_spectral': log_error,
                             'envelope': env_error,
                             'loss': 0.5 * spectral_error + 0.25 * log_error + 0.25 * env_error}
        loss = float(np.mean([v['loss'] for v in regions.values()]))
        if not math.isfinite(loss):
            raise ValueError('non-finite comparison')
        return {'loss': loss, 'regions': regions, 'candidate_gain': gain,
                'candidate_gain_db': 20 * math.log10(gain),
                'duration_difference_seconds': len(candidate.data) / candidate.rate
                                               - len(self.reference.data) / self.reference.rate}


def parameter_specs(parameters):
    if not parameters:
        raise ValueError('at least one parameter is required')
    names, bounds, initial = [], [], []
    for name, spec in parameters.items():
        lo, hi, value = [float(spec[k]) for k in ('min', 'max', 'initial')]
        if not all(map(math.isfinite, (lo, hi, value))) or not lo < hi or not lo <= value <= hi:
            raise ValueError(f'invalid bounds/initial value for {name}')
        names.append(name)
        bounds.append((lo, hi))
        initial.append(value)
    return names, bounds, initial


class BudgetReached(Exception):
    pass


class CandidateError(ValueError):
    pass


def search(render, metric, parameters, budget=300, seed=1, progress=None):
    names, bounds, initial = parameter_specs(parameters)
    if budget < 1:
        raise ValueError('budget must be positive')
    result = {'history': [], 'parameters': None, 'best': None, 'initial': None}

    def objective(values):
        if len(result['history']) >= budget:
            raise BudgetReached
        params = dict(zip(names, map(float, values)))
        entry = {'evaluation': len(result['history']) + 1, 'parameters': params}
        try:
            audio = render(params)
            metrics = metric.compare(audio)
            value = metrics['loss']
            entry['loss'] = value
            if result['initial'] is None:
                result['initial'] = metrics
            if result['best'] is None or value < result['best']['loss']:
                result.update(parameters=params, best=metrics, best_audio_sha256=audio_hash(audio))
                if progress:
                    progress(entry)
        except (ValueError, CandidateError) as error:
            if not result['history']:
                raise ValueError(f'initial candidate failed: {error}') from error
            value = 1e12
            entry['error'] = str(error)
        result['history'].append(entry)
        return value

    objective(initial)
    if budget > 1:
        try:
            differential_evolution(objective, bounds, x0=initial, rng=np.random.default_rng(seed),
                                   popsize=6, maxiter=budget, polish=False, tol=0, atol=0,
                                   workers=1, updating='immediate')
        except BudgetReached:
            pass
    return result


def edit_preset(source, parameters):
    parsed = tomllib.loads(source)
    edits = dict(parameters)
    for name, value in edits.items():
        fields = name.split('.')
        if len(fields) != 2 or fields[0] != 'macros' or fields[1] not in parsed.get('macros', {}):
            raise ValueError(f'not an existing macro: {name}')
        if not math.isfinite(value) or not 0 <= value <= 1:
            raise ValueError(f'macro outside 0..1: {name}')
    section, lines = '', []
    for line in source.splitlines(keepends=True):
        match = re.match(r'\s*\[([^]]+)\]', line)
        if match:
            section = match[1]
        match = re.match(r'(\s*([A-Za-z_][A-Za-z_0-9]*)\s*=\s*)([^#\r\n]+)(.*)', line)
        if match and f'{section}.{match[2]}' in edits:
            name = f'{section}.{match[2]}'
            trailing = match[3][len(match[3].rstrip()):]
            newline = '\n' if line.endswith('\n') else ''
            line = f'{match[1]}{edits.pop(name)!r}{trailing}{match[4]}{newline}'
        lines.append(line)
    if edits:
        raise ValueError(f'cannot safely edit preset keys: {list(edits)}')
    edited = ''.join(lines)
    tomllib.loads(edited)
    return edited


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def audio_hash(audio):
    digest = hashlib.sha256(str(audio.rate).encode())
    digest.update(np.ascontiguousarray(audio.data, dtype='<f8').tobytes())
    return digest.hexdigest()


def identity(path):
    path = Path(path).resolve()
    return {'path': str(path), 'sha256': sha256(path)}


def output_directory(path):
    path = Path(path).resolve()
    if path.is_relative_to(ROOT) and not path.is_relative_to(ROOT / 'artifacts'):
        raise ValueError('generated output inside this repository must be under artifacts/')
    path.mkdir(parents=True, exist_ok=False)
    return path


def export_pair(directory, reference, candidate):
    a, b, gain = aligned_pair(reference, candidate)
    # One common final attenuation preserves the A/B gain relationship.
    peak = max(float(np.max(np.abs(a))), float(np.max(np.abs(b))))
    common = min(1.0, 0.95 / max(peak, EPS))
    a, b = a * common, b * common
    gap = np.zeros((reference.rate // 2, 2))
    for name, data in [('reference', a), ('candidate', b), ('ab', np.concatenate([a, gap, b, gap]))]:
        if not np.isfinite(data).all():
            raise ValueError('refusing to export non-finite samples')
        wavfile.write(directory / f'{name}.wav', reference.rate, data.astype(np.float32))
    return {'candidate_gain': gain, 'common_gain': common, 'peak': peak * common,
            'order': ['reference', '0.5 seconds silence', 'candidate', '0.5 seconds silence'],
            'matching': 'whole-excerpt stereo RMS; not perceptual loudness or stem loudness'}


class Renderer:
    def __init__(self, config, config_path, reference, work):
        self.config = config
        self.work = work
        self.rate = reference.rate
        self.seconds = len(reference.data) / reference.rate
        self.frames = len(reference.data)
        self.base = config_path.parent
        self.spec = config['renderer']
        self.parameters = config['parameters']
        parameter_specs(self.parameters)
        self.kind = self.spec['kind']
        self.provenance = {}
        self.timeout = float(self.spec.get('timeout_seconds', 30))
        if not math.isfinite(self.timeout) or not 0 < self.timeout <= 600:
            raise ValueError('renderer timeout must be in (0, 600] seconds')
        if self.kind == 'shr-synth':
            self.binary = self.resolve(self.spec['binary'])
            self.preset = self.resolve(self.spec['preset'])
            self.source = self.preset.read_text()
            self.note = self.spec['note']
            self.velocity = float(self.spec.get('velocity', 0.8))
            if type(self.note) is not int or not 0 <= self.note <= 127:
                raise ValueError('note must be an integer MIDI note in 0..127')
            if not math.isfinite(self.velocity) or not 0 < self.velocity <= 1:
                raise ValueError('velocity must be in (0, 1]')
            initial = {k: v['initial'] for k, v in self.parameters.items()}
            edit_preset(self.source, initial)
            for key, value in self.parameters.items():
                if not 0 <= value['min'] < value['max'] <= 1:
                    raise ValueError(f'SHR macro bounds must be within 0..1: {key}')
            self.provenance = {'binary': identity(self.binary), 'preset': identity(self.preset),
                               'gate': 'existing CLI: NoteOff at 80% of total render frames',
                               'note': self.note, 'velocity': self.velocity}
        elif self.kind == 'command':
            argv = self.spec['argv']
            if not isinstance(argv, list) or not argv or not all(isinstance(a, str) for a in argv):
                raise ValueError('command argv must be a nonempty list of strings')
            if not any('{params}' in a for a in argv) or not any('{output}' in a for a in argv):
                raise ValueError('command argv must include {params} and {output}')
            self.argv = argv
            self.provenance = {'argv': argv, 'cwd': str(self.base),
                               'inputs': [identity(self.resolve(p)) for p in self.spec.get('inputs', [])]}
        else:
            raise ValueError('renderer kind must be shr-synth or command')

    def resolve(self, path):
        return (self.base / path).resolve()

    def __call__(self, parameters):
        wav = self.work / 'trial.wav'
        wav.unlink(missing_ok=True)
        if self.kind == 'shr-synth':
            preset = self.work / 'trial.mojsint'
            preset.write_text(edit_preset(self.source, parameters))
            argv = [str(self.binary), 'render', str(preset), str(wav), '--note', str(self.note),
                    '--velocity', str(self.velocity), '--seconds', str(self.seconds),
                    '--sample-rate', str(self.rate)]
        else:
            params = self.work / 'params.json'
            params.write_text(json.dumps(parameters, allow_nan=False))
            substitutions = {'params': str(params), 'output': str(wav),
                             'sample_rate': str(self.rate), 'seconds': str(self.seconds)}
            argv = [arg.format_map(substitutions) for arg in self.argv]
        try:
            completed = subprocess.run(argv, cwd=self.base, capture_output=True, text=True,
                                       timeout=self.timeout, check=False)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise CandidateError(f'renderer failed: {error}') from error
        if completed.returncode:
            raise CandidateError(f'renderer exit {completed.returncode}: {completed.stderr[-1500:]}')
        if not wav.is_file():
            raise CandidateError('renderer did not produce its WAV')
        audio = read_audio(wav)
        if audio.rate != self.rate or abs(len(audio.data) - self.frames) > 1:
            raise CandidateError('renderer must honor requested sample rate and duration')
        return audio

    def save(self, parameters, out):
        if self.kind == 'shr-synth':
            (out / 'best.mojsint').write_text(edit_preset(self.source, parameters))


def versions():
    return {'python': platform.python_version(), 'numpy': np.__version__, 'scipy': scipy.__version__,
            'platform': platform.platform(), 'fitter': identity(__file__)}


def write_report(out, report):
    report['exports'] = {p.name: sha256(p) for p in sorted(out.glob('*.wav'))}
    (out / 'report.json').write_text(json.dumps(report, indent=2, allow_nan=False) + '\n')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    compare = sub.add_parser('compare', help='score two WAV excerpts and export a level-matched A/B')
    compare.add_argument('reference', type=Path)
    compare.add_argument('candidate', type=Path)
    optimize = sub.add_parser('fit', help='fit bounded renderer parameters to a reference WAV')
    optimize.add_argument('reference', type=Path)
    optimize.add_argument('config', type=Path)
    optimize.add_argument('--budget', type=int, default=300, help='maximum search renders, including baseline')
    optimize.add_argument('--seed', type=int, default=1)
    for command in [compare, optimize]:
        command.add_argument('--out', type=Path, required=True, help='new output directory; never overwritten')
        command.add_argument('--attack-end', type=float, default=0.08)
        command.add_argument('--body-end', type=float, default=0.4)
    args = parser.parse_args(argv)
    try:
        reference = read_audio(args.reference)
        metric = Metric(reference, args.attack_end, args.body_end)
        report = {'reference': identity(args.reference), 'versions': versions(),
                  'metric': {'name': 'MR spectral + 5ms envelope; L/R/mid/side; equal region weights',
                             'attack_end': args.attack_end, 'body_end': args.body_end,
                             'fft_sizes': metric.sizes, 'alignment': 'file origins; no automatic time shift'},
                  'evidence': 'numerical comparison only; not listening acceptance or instrument attribution'}
        if args.action == 'compare':
            candidate = read_audio(args.candidate)
            report.update(candidate=identity(args.candidate), comparison=metric.compare(candidate))
            out = output_directory(args.out)
            report['ab'] = export_pair(out, reference, candidate)
        else:
            if args.budget < 1:
                raise ValueError('budget must be positive')
            config_path = args.config.resolve()
            config = json.loads(config_path.read_text())
            out = output_directory(args.out)
            with tempfile.TemporaryDirectory(prefix='render-', dir=out) as work:
                renderer = Renderer(config, config_path, reference, Path(work))
                initial_params = {k: v['initial'] for k, v in config['parameters'].items()}
                baseline = renderer(initial_params)
                repeat = renderer(initial_params)
                if not np.array_equal(baseline.data, repeat.data):
                    raise ValueError('renderer is not deterministic; fix its seed/reset before fitting')
                result = search(renderer, metric, config['parameters'], args.budget, args.seed,
                                progress=lambda e: print(f"evaluation {e['evaluation']}: loss {e['loss']:.6f}", flush=True))
                best = renderer(result['parameters'])
                repeated_metrics = metric.compare(best)
                if repeated_metrics != result['best'] or audio_hash(best) != result['best_audio_sha256']:
                    raise ValueError('best render did not reproduce its measured score')
                renderer.save(result['parameters'], out)
                report.update(config=identity(config_path), configuration=config,
                              renderer=renderer.provenance, seed=args.seed, budget=args.budget,
                              extra_validation_renders=3, optimization=result)
                report['ab'] = export_pair(out, reference, best)
                # Preserve initial and native-level best output for source-level auditing.
                wavfile.write(out / 'initial-native.wav', baseline.rate, baseline.data.astype(np.float32))
                wavfile.write(out / 'best-native.wav', best.rate, best.data.astype(np.float32))
                (out / 'best-parameters.json').write_text(json.dumps(result['parameters'], indent=2) + '\n')
        write_report(out, report)
        print(out / 'report.json')
        return 0
    except (ValueError, KeyError, OSError) as error:
        print(f'audio-fit: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
