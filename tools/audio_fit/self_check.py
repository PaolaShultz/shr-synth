#!/usr/bin/env python3
"""Opt-in known-patch recovery evidence; all generated data is disposable."""

import argparse
import json
from pathlib import Path
import subprocess
import sys

import fit


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=fit.ROOT / 'target/release/shr-synth')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    out = fit.output_directory(args.out)
    binary = args.binary.resolve()
    source = (fit.ROOT / 'presets/reference.mojsint').read_text()
    truth = {'macros.color': 0.6, 'macros.attack': 0.08}
    initial = {'macros.color': 0.25, 'macros.attack': 0.3}
    (out / 'initial.mojsint').write_text(fit.edit_preset(source, initial))
    (out / 'target.mojsint').write_text(fit.edit_preset(source, truth))
    config = {
        'renderer': {'kind': 'shr-synth', 'binary': str(binary),
                     'preset': str(out / 'initial.mojsint'), 'note': 60, 'velocity': 0.8},
        'parameters': {'macros.color': {'min': 0.15, 'max': 0.8, 'initial': 0.25},
                       'macros.attack': {'min': 0.01, 'max': 0.5, 'initial': 0.3}},
    }
    config_path = out / 'config.json'
    config_path.write_text(json.dumps(config, indent=2) + '\n')

    def render(preset, name, note):
        wav = out / name
        subprocess.run([str(binary), 'render', str(preset), str(wav), '--note', str(note),
                        '--seconds', '0.6', '--sample-rate', '48000', '--velocity', '0.8'],
                       check=True, capture_output=True, text=True, timeout=30)
        return wav

    reference = render(out / 'target.mojsint', 'reference.wav', 60)
    code = fit.main(['fit', str(reference), str(config_path), '--out', str(out / 'result'),
                     '--budget', '120', '--seed', '7'])
    if code:
        return code
    fitted = json.loads((out / 'result/report.json').read_text())['optimization']
    holdout_ref = render(out / 'target.mojsint', 'holdout-reference.wav', 67)
    holdout_initial = render(out / 'initial.mojsint', 'holdout-initial.wav', 67)
    holdout_best = render(out / 'result/best.mojsint', 'holdout-best.wav', 67)
    metric = fit.Metric(fit.read_audio(holdout_ref))
    before = metric.compare(fit.read_audio(holdout_initial))
    after = metric.compare(fit.read_audio(holdout_best))
    report = {'purpose': 'known Model D recovery; no Temple recording or listening evidence',
              'truth': truth, 'recovered': fitted['parameters'],
              'training': {'initial': fitted['initial']['loss'], 'best': fitted['best']['loss']},
              'holdout': {'note': 67, 'initial': before, 'best': after}}
    (out / 'proof.json').write_text(json.dumps(report, indent=2, allow_nan=False) + '\n')
    print(json.dumps(report, indent=2))
    if fitted['best']['loss'] >= fitted['initial']['loss'] * 0.05 or after['loss'] >= before['loss'] * 0.05:
        raise RuntimeError('known-patch recovery did not reduce both losses by the required amount')
    return 0


if __name__ == '__main__':
    sys.exit(main())
