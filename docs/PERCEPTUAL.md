# Perceptual engine

Five authored playable starts explore different filter and perceptual mechanisms.
Select the **Perceptual** model in SHR-DAW: **E01 Micro Delay**, **E02 Spectral
Split**, **E03 Dispersion**, **E04 Notch Motion**, **E05 Virtual Pitch**. These
are listening candidates; implementation does not establish musical acceptance.

## Play and controls

Start with a held middle-register note or a short chord. Turn Position, then
Env Depth; reduce Motion to zero to isolate the envelope. Attack, Decay,
Sustain and Release shape both amplitude and the position contour. All eight
tone controls affect held notes through the existing 10 ms smoothers.

| Physical row | Pot 1 | Pot 2 | Pot 3 | Pot 4 |
| --- | --- | --- | --- | --- |
| 1 | Position | Focus | Bright | Body |
| 2 | Env Depth | Rate | Motion | Width |
| 3 | Attack | Decay | Sustain | Release |
| 4 | Volume | AUX 1 | AUX 2 | AUX 3 |

The first rotary still edits Position; its click still toggles NAV. There are
no new buttons, pages or modes. Topology belongs to the selected preset.
The native names in `.mojsint` are documented in [the schema](PRESET_SCHEMA.md).

- **Position** moves the topology's spectral or temporal center. Env Depth
  adds an ADSR-dependent excursion around it, and Motion adds slow cyclic
  motion. At zero Env Depth/Motion it stays at the knob position.
- **Focus** increases low-pass weighting, band contrast, phase contribution,
  notch contribution or harmonic-cluster selectivity, depending on topology.
- **Bright** changes the authored harmonic slope and filter opening.
- **Body** restores the physical fundamental; in Notch Motion it also restores
  dry body. Virtual Pitch starts at zero Body to omit the fundamental.
- **Rate** spans 0.03–4 Hz. **Motion** sets cyclic position excursion.
- **Width** controls differences between channels. Spectral Split and Virtual
  Pitch use complementary side signals whose mono sum preserves their center.
  The other routings may change colour in mono; compare stereo and mono when
  listening. Width is a stereo control, not an AUX effect.

## Five routings

All use a twelve-partial harmonic source with smooth high-frequency exclusion,
not imported samples. The source's physical fundamental is independently
controlled. Fixed output gain is used; there is no limiter, compressor,
normalizer, nonlinear saturation or automatic loudness matching.

| Start | Signal topology | Envelope/Position destination | Listening question |
| --- | --- | --- | --- |
| Micro Delay | Persistent source plus four unequal feed-forward sub-echo taps; dry center retained | Tap times and source filtering | Does the fused image move internally without a conspicuous repeat? |
| Spectral Split | Complementary low/high spectral energy sent oppositely to L/R | Band split | Does the center change colour while the image opens? |
| Dispersion | Two four-stage all-pass cascades, mixed with direct source | Phase-transition frequencies | Does the attack/colour become fluid without losing the note? |
| Notch Motion | Subtract the all-pass output from the direct path, with optional body | Moving spectral notches | Is the envelope-driven hollow sweep useful? |
| Virtual Pitch | Fundamental-suppressed additive harmonic cluster; odd/even stereo split | Cluster center and filtering | Is the implied low pitch convincing despite the absent fundamental? |

Dispersion and Notch Motion share all-pass primitives but use different output
summing laws. They are distinct wirings, not separate positions in one preset.
High notes progressively lose upper harmonics; Virtual Pitch can become very
quiet at the top of the MIDI range when all of its selected harmonics exceed
the anti-alias guard. Four voices are a trial default, not a Pi capacity result.

## Implementation and evidence boundaries

`src/perceptual.rs` owns fixed sine-table, delay, all-pass and envelope storage.
Construction prepares the table; note/control/render paths allocate nothing.
The sample path uses interpolated lookup and arithmetic without transcendental
coefficient setup. The ADSR drives both the VCA and position. Pitch bend,
release, voice stealing and All Notes Off use the ordinary production Engine.
No JACK, ALSA, filesystem, process or clock access enters the DSP.

`tests/perceptual_engine.rs` covers deterministic output, schema round trips,
every tone control on a held note, all MIDI keys, pitch extremes, release and
All Notes Off with a debug allocation guard at 44.1/48/96 kHz.
Unit tests measure stationary full-path residual outside the twelve intended
partials at coherent low/mid/high frequencies against a -75 dB bound. They
also verify fundamental suppression and the Spectral Split mono identity.
This stationary diagnostic is not a general time-varying alias guarantee;
sidebands from moving filters/delays and perceptual usefulness require listening.
No Pi callback, latency, headphone/speaker translation or sound-quality claim
follows from workstation tests.

## Sources and authorship

These are mathematical/perceptual references only. No third-party DSP source,
presets, samples, text, illustrations or experimental stimuli were copied.
Original SHR Synth implementation and factory starts use the repository's MIT
license; the reference publications retain their own rights.

- Julius O. Smith III, *Physical Audio Signal Processing*, 2010, online book,
  [First-Order Allpass Interpolation](https://www.dsprelated.com/freebooks/pasp/First_Order_Allpass_Interpolation.html)
  and [Phasing with First-Order Allpass Filters](https://www.dsprelated.com/freebooks/pasp/Phasing_First_Order_Allpass_Filters.html).
  Transfer-function facts support unity-magnitude phase sections and dry/phase
  summing as a source of notches. No example source code is imported.
- Richard Baker, University of Manchester, *Pitch of the Missing Fundamental*,
  undated [interactive demonstration](https://personalpages.manchester.ac.uk/staff/richard.baker/WebDemonstrations/Pitch/MissingFundamental.html).
  Supports testing harmonic complexes with their lowest components absent;
  it does not validate these presets or every register.
- Wallach, Newman and Rosenzweig (1949), Grosse et al. (2018), and Boers (1983):
  authorship, publication details, URLs and rights are retained in
  [the spatial micro-delay research record](RESEARCH.md#psychoacoustic-micro-delay-spatial-motion-source).
  These motivate the listening hypothesis, not a claim that precedence or
  localization has been demonstrated by this implementation.
