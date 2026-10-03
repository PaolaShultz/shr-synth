"""Fast offline fitter contracts; no historical audio or listening fixtures."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import numpy as np
from scipy.io import wavfile

import fit


RATE = 8000


def note(decay=0.13, overtone=0.4, hz=220.0, seconds=0.5):
    t = np.arange(round(RATE * seconds)) / RATE
    envelope = (1.0 - np.exp(-t / 0.004)) * np.exp(-t / decay)
    return envelope * (np.sin(2 * np.pi * hz * t)
                       + overtone * np.sin(4 * np.pi * hz * t))


class MetricTests(unittest.TestCase):
    def test_gain_is_removed_but_pitch_and_envelope_errors_remain(self):
        target = fit.Audio(RATE, note())
        metric = fit.Metric(target)
        identical = metric.compare(fit.Audio(RATE, note() * 0.07))
        self.assertLess(identical['loss'], 1e-10)
        self.assertAlmostEqual(identical['candidate_gain'], 1 / 0.07)
        self.assertGreater(metric.compare(fit.Audio(RATE, note(hz=247)))['loss'], 0.1)
        self.assertGreater(metric.compare(fit.Audio(RATE, note(decay=0.4)))['loss'], 0.05)

    def test_stereo_width_and_channel_balance_are_not_normalized_away(self):
        x = note()
        stereo = np.column_stack([x, np.roll(x, 11) * 0.5])
        metric = fit.Metric(fit.Audio(RATE, stereo))
        mono = stereo.mean(axis=1)
        self.assertGreater(metric.compare(fit.Audio(RATE, mono))['loss'], 0.01)
        self.assertGreater(metric.compare(fit.Audio(RATE, stereo[:, ::-1]))['loss'], 0.01)

    def test_tail_and_wrong_length_are_not_silently_discarded(self):
        metric = fit.Metric(fit.Audio(RATE, note()))
        longer = np.concatenate([note(), note(seconds=0.1)])
        self.assertGreater(metric.compare(fit.Audio(RATE, longer))['loss'], 0.01)
        shifted = np.concatenate([np.zeros(200), note()])
        self.assertGreater(metric.compare(fit.Audio(RATE, shifted))['loss'], 0.01)

    def test_silence_nan_infinity_and_too_short_are_rejected(self):
        for x in [np.zeros(4000), np.full(4000, np.nan),
                  np.full(4000, np.inf), np.ones(2)]:
            with self.subTest(x=x[:2]), self.assertRaises(ValueError):
                fit.Metric(fit.Audio(RATE, x))
        with self.assertRaises(ValueError):
            fit.Metric(fit.Audio(RATE, note())).compare(fit.Audio(RATE, np.zeros(4000)))

    def test_phase_inverted_stereo_remains_audible(self):
        x = note()
        metric = fit.Metric(fit.Audio(RATE, np.column_stack([x, -x])))
        self.assertLess(metric.compare(fit.Audio(RATE, np.column_stack([x, -x])))['loss'], 1e-10)

    def test_region_report_separates_attack_body_and_tail(self):
        report = fit.Metric(fit.Audio(RATE, note())).compare(fit.Audio(RATE, note(decay=0.3)))
        self.assertEqual(set(report['regions']), {'attack', 'body', 'tail'})
        self.assertTrue(all(np.isfinite(v['loss']) for v in report['regions'].values()))


class SearchTests(unittest.TestCase):
    def test_recovers_known_source_deterministically_with_hard_budget(self):
        metric = fit.Metric(fit.Audio(RATE, note(0.21, 0.7)))
        def run():
            return fit.search(
                lambda p: fit.Audio(RATE, note(p['decay'], p['overtone'])), metric,
                {'decay': {'min': 0.06, 'max': 0.35, 'initial': 0.08},
                 'overtone': {'min': 0.1, 'max': 1.0, 'initial': 0.15}},
                budget=100, seed=7)
        a, b = run(), run()
        self.assertEqual(a['parameters'], b['parameters'])
        self.assertLessEqual(len(a['history']), 100)
        self.assertLess(a['best']['loss'], a['initial']['loss'] * 0.15)
        self.assertAlmostEqual(a['parameters']['decay'], 0.21, delta=0.025)
        self.assertAlmostEqual(a['parameters']['overtone'], 0.7, delta=0.08)

    def test_invalid_candidates_cannot_win(self):
        def render(p):
            return fit.Audio(RATE, note() if p['x'] < 0.1 else np.zeros(4000))
        result = fit.search(render, fit.Metric(fit.Audio(RATE, note())),
                            {'x': {'min': 0, 'max': 1, 'initial': 0}}, 15, 0)
        self.assertEqual(result['parameters']['x'], 0)
        self.assertTrue(any('error' in h for h in result['history']))

    def test_bad_bounds_rejected(self):
        for spec in [{'min': 1, 'max': 0, 'initial': 0},
                     {'min': 0, 'max': 1, 'initial': 2},
                     {'min': 0, 'max': float('nan'), 'initial': 0}]:
            with self.assertRaises(ValueError):
                fit.search(lambda p: fit.Audio(RATE, note()),
                           fit.Metric(fit.Audio(RATE, note())), {'x': spec}, 10, 0)


class IOTests(unittest.TestCase):
    def test_pcm_float_and_mono_stereo(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            for dtype in [np.int16, np.int32, np.float32]:
                x = note() * 0.5
                scale = float(np.iinfo(dtype).max) + 1 if np.issubdtype(dtype, np.integer) else 1
                path = d / f'{dtype.__name__}.wav'
                wavfile.write(path, RATE, (x * scale).astype(dtype))
                loaded = fit.read_audio(path)
                self.assertEqual(loaded.data.shape, (4000, 2))
                np.testing.assert_allclose(loaded.data[:, 0], x, atol=4e-5)

    def test_safe_ab_exports_use_one_gain_and_do_not_clip(self):
        with tempfile.TemporaryDirectory() as d:
            target = fit.Audio(RATE, note() * 2)
            candidate = fit.Audio(RATE, note() * 0.2)
            meta = fit.export_pair(Path(d), target, candidate)
            a, b = fit.read_audio(Path(d) / 'reference.wav'), fit.read_audio(Path(d) / 'candidate.wav')
            self.assertLessEqual(np.max(np.abs(a.data)), 0.96)
            self.assertLessEqual(np.max(np.abs(b.data)), 0.96)
            np.testing.assert_allclose(a.data, b.data, atol=1e-7)
            self.assertAlmostEqual(meta['candidate_gain'], 10)

    def test_preset_edit_preserves_other_fields_and_refuses_unknown_keys(self):
        source = 'schema_version = 10\nname = "fixed"\n[macros]\nattack = 0.2 # keep\nrelease = 0.3\n'
        edited = fit.edit_preset(source, {'macros.attack': 0.6})
        self.assertIn('attack = 0.6 # keep', edited)
        self.assertIn('release = 0.3', edited)
        self.assertIn('name = "fixed"', edited)
        with self.assertRaises(ValueError):
            fit.edit_preset(source, {'macros.missing': 0.2})

    def test_output_inside_repo_must_be_in_artifacts_and_never_overwritten(self):
        with tempfile.TemporaryDirectory(dir=fit.ROOT / 'artifacts') as d:
            new = Path(d) / 'new'
            fit.output_directory(new)
            with self.assertRaises(FileExistsError):
                fit.output_directory(new)
        with self.assertRaises(ValueError):
            fit.output_directory(fit.ROOT / 'should-not-exist')

    def test_compare_cli_writes_valid_report_and_reproducible_pair(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            wavfile.write(d / 'a.wav', RATE, note().astype(np.float32))
            wavfile.write(d / 'b.wav', RATE, (note() * 0.5).astype(np.float32))
            result = subprocess.run([sys.executable, str(Path(fit.__file__)), 'compare',
                                     str(d / 'a.wav'), str(d / 'b.wav'), '--out', str(d / 'out')],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((d / 'out' / 'report.json').read_text())
            self.assertLess(report['comparison']['loss'], 1e-9)
            self.assertEqual(len(report['reference']['sha256']), 64)
            self.assertTrue((d / 'out' / 'ab.wav').is_file())


class RendererTests(unittest.TestCase):
    def test_command_protocol_produces_a_reusable_best_parameter_file(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            renderer = d / 'renderer.py'
            renderer.write_text('''import json, sys
import numpy as np
from scipy.io import wavfile
p = json.load(open(sys.argv[1]))
rate = int(sys.argv[3])
t = np.arange(round(rate * float(sys.argv[4]))) / rate
x = np.exp(-t / p['decay']) * np.sin(2*np.pi*220*t)
wavfile.write(sys.argv[2], rate, x.astype(np.float32))
''')
            config = {'renderer': {'kind': 'command', 'argv': [sys.executable, str(renderer),
                       '{params}', '{output}', '{sample_rate}', '{seconds}'], 'inputs': [str(renderer)]},
                       'parameters': {'decay': {'min': 0.05, 'max': 0.3, 'initial': 0.08}}}
            (d / 'config.json').write_text(json.dumps(config))
            t = np.arange(4000) / RATE
            wavfile.write(d / 'reference.wav', RATE,
                          (np.exp(-t / 0.22) * np.sin(2*np.pi*220*t)).astype(np.float32))
            result = subprocess.run([sys.executable, fit.__file__, 'fit', str(d / 'reference.wav'),
                                     str(d / 'config.json'), '--out', str(d / 'out'), '--budget', '12'],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((d / 'out' / 'report.json').read_text())
            trial = report['optimization']
            self.assertLess(trial['best']['loss'], trial['initial']['loss'])
            self.assertLessEqual(len(trial['history']), 12)
            self.assertEqual(json.loads((d / 'out' / 'best-parameters.json').read_text()), trial['parameters'])
            self.assertTrue(report['renderer']['inputs'][0]['sha256'])

    def test_command_failure_and_timeout_do_not_reuse_a_stale_wav(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            for code in ['raise SystemExit(3)', 'import time; time.sleep(2)']:
                wavfile.write(d / 'trial.wav', RATE, note().astype(np.float32))
                config = {'renderer': {'kind': 'command', 'argv': [sys.executable, '-c', code,
                           '{params}', '{output}'], 'timeout_seconds': 0.1},
                          'parameters': {'x': {'min': 0, 'max': 1, 'initial': 0.5}}}
                renderer = fit.Renderer(config, d / 'config.json', fit.Audio(RATE, note()), d)
                with self.assertRaises(fit.CandidateError):
                    renderer({'x': 0.5})
                self.assertFalse((d / 'trial.wav').exists())


if __name__ == '__main__':
    unittest.main()
