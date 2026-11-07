use shr_synth::{
    control::{MacroId, Normalized},
    engine::{Engine, Event, TimedEvent},
    preset::Preset,
};
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
const MODES: [&str; 10] = [
    "micro_delay",
    "spectral_split",
    "dispersion",
    "notch_motion",
    "virtual_pitch",
    "counter_tide",
    "vowel_orbit",
    "phase_bloom",
    "spectral_carousel",
    "tidal_memory",
];
fn preset(mode: &str) -> Preset {
    Preset::parse(&format!(
        r#"schema_version = 11
name = "Perceptual test"
voices = 4
output_gain = 0.5
instrument_volume = 1.0
model = "perceptual"
perceptual_topology = "{mode}"
[macros]
position = 0.4
focus = 0.6
brightness = 0.6
body = 0.4
env_depth = 0.6
rate = 0.3
motion = 0.4
width = 0.7
attack = 0.1
decay = 0.3
sustain = 0.7
release = 0.15
"#
    ))
    .unwrap()
}
fn render(mode: &str, change: Option<usize>) -> Vec<f32> {
    let mut engine = Engine::new(48000.0, &preset(mode)).unwrap();
    let mut l = vec![0.0; 24000];
    let mut r = l.clone();
    let mut events = vec![TimedEvent::new(
        0,
        Event::NoteOn {
            note: 48,
            velocity: 0.8,
        },
    )];
    if let Some(i) = change {
        events.push(TimedEvent::new(
            6000,
            Event::SetMacro {
                id: MacroId::ALL[i],
                value: Normalized::new(0.95).unwrap(),
            },
        ));
    }
    assert_no_alloc::assert_no_alloc(|| engine.render_block(&events, &mut l, &mut r).unwrap());
    l.extend(r);
    l
}
#[test]
fn identities_roundtrip_distinct_and_live() {
    let mut previous = Vec::new();
    for mode in MODES {
        let p = preset(mode);
        assert_eq!(Preset::parse(&p.to_toml().unwrap()).unwrap(), p);
        let audio = render(mode, None);
        assert_eq!(audio, render(mode, None));
        assert!(audio.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        assert!(audio.iter().any(|v| v.abs() > 0.01));
        for i in 0..8 {
            let changed = render(mode, Some(i));
            if MODES[5..].contains(&mode) {
                // A held-note edit must enter through the production smoother.
                // Compare the edit's incremental contribution, cancelling the
                // unedited waveform rather than mistaking its slope for a click.
                for channel in 0..2 {
                    let start = channel * 24000;
                    assert_eq!(&audio[start..start + 6000], &changed[start..start + 6000]);
                    let mut previous_delta = 0.0;
                    for sample in 6000..6500 {
                        let delta = changed[start + sample] - audio[start + sample];
                        assert!(
                            (delta - previous_delta).abs() < 0.02,
                            "{mode} control {i}: discontinuous edit"
                        );
                        previous_delta = delta;
                    }
                }
            }
            assert!(
                audio
                    .iter()
                    .zip(changed)
                    .map(|(a, b)| (a - b).abs())
                    .sum::<f32>()
                    > 0.01,
                "{mode} control {i}"
            );
        }
        assert_ne!(audio, previous);
        previous = audio;
    }
}
#[test]
fn extremes_pitch_release_and_panic_are_bounded_without_allocation() {
    for mode in MODES {
        for rate in [44100.0, 48000.0, 96000.0] {
            let mut engine = Engine::new(rate, &preset(mode)).unwrap();
            let mut l = [0.0; 256];
            let mut r = l;
            assert_no_alloc::assert_no_alloc(|| {
                for note in 0..128 {
                    engine
                        .render_block(
                            &[
                                TimedEvent::new(
                                    0,
                                    Event::NoteOn {
                                        note,
                                        velocity: 1.0,
                                    },
                                ),
                                TimedEvent::new(
                                    1,
                                    Event::SetMacro {
                                        id: MacroId::ALL[note as usize % 12],
                                        value: Normalized::new((note % 2) as f32).unwrap(),
                                    },
                                ),
                                TimedEvent::new(
                                    2,
                                    Event::PitchBend {
                                        value: if note % 2 == 0 { 8191 } else { -8192 },
                                    },
                                ),
                            ],
                            &mut l,
                            &mut r,
                        )
                        .unwrap();
                    assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() <= 1.0));
                }
                engine
                    .render_block(&[TimedEvent::new(0, Event::AllNotesOff)], &mut l, &mut r)
                    .unwrap();
                assert_eq!(l, [0.0; 256]);
                assert_eq!(r, l);
                engine
                    .render_block(
                        &[
                            TimedEvent::new(
                                0,
                                Event::SetMacro {
                                    id: MacroId::Release,
                                    value: Normalized::new(0.0).unwrap(),
                                },
                            ),
                            TimedEvent::new(
                                0,
                                Event::NoteOn {
                                    note: 60,
                                    velocity: 0.8,
                                },
                            ),
                            TimedEvent::new(128, Event::NoteOff { note: 60 }),
                        ],
                        &mut l,
                        &mut r,
                    )
                    .unwrap();
                for _ in 0..100 {
                    engine.render_block(&[], &mut l, &mut r).unwrap();
                }
                assert_eq!(engine.active_voice_count(), 0);
                assert_eq!(l, [0.0; 256]);
            });
        }
    }
}

#[test]
fn adding_second_held_note_preserves_first_voice_exactly() {
    const N: usize = 16000;
    const SECOND: usize = 12037;
    for mode in MODES {
        let p = preset(mode);
        let note = |offset, key| {
            TimedEvent::new(
                offset,
                Event::NoteOn {
                    note: key,
                    velocity: 0.8,
                },
            )
        };
        let mut both = Engine::new(48000.0, &p).unwrap();
        let mut first = Engine::new(48000.0, &p).unwrap();
        let mut second = Engine::new(48000.0, &p).unwrap();
        let mut a = vec![0.0; N];
        let mut ar = a.clone();
        let mut b = a.clone();
        let mut br = a.clone();
        let mut c = a.clone();
        let mut cr = a.clone();
        assert_no_alloc::assert_no_alloc(|| {
            both.render_block(&[note(0, 48), note(SECOND, 55)], &mut a, &mut ar)
                .unwrap();
            first.render_block(&[note(0, 48)], &mut b, &mut br).unwrap();
            second
                .render_block(&[note(SECOND, 55)], &mut c, &mut cr)
                .unwrap();
        });
        assert_eq!(both.active_voice_count(), 2);
        for i in 0..N {
            assert!(
                (a[i] - b[i] - c[i]).abs() < 1e-7 && (ar[i] - br[i] - cr[i]).abs() < 1e-7,
                "{mode}: note interaction at {i}"
            );
        }
        assert!(
            (a[SECOND] - b[SECOND]).abs() < 0.001,
            "{mode}: second-note onset step"
        );
    }
}

#[test]
#[ignore = "opt-in native offline callback-cost diagnosis; not live headroom acceptance"]
fn perceptual_callback_cost() {
    use std::time::Instant;
    for mode in MODES {
        for count in [1, 2, 4, 8] {
            let mut p = preset(mode);
            p.voices = count;
            let mut e = Engine::new(48000.0, &p).unwrap();
            let mut l = [0.0; 128];
            let mut r = l;
            for i in 0..count {
                e.render_block(
                    &[TimedEvent::new(
                        0,
                        Event::NoteOn {
                            note: 48 + i as u8 * 3,
                            velocity: 0.8,
                        },
                    )],
                    &mut l,
                    &mut r,
                )
                .unwrap();
            }
            let mut durations = Vec::with_capacity(1024);
            for i in 0..1088 {
                let events = [TimedEvent::new(
                    37,
                    Event::NoteOn {
                        note: 60,
                        velocity: 0.8,
                    },
                )];
                let start = Instant::now();
                e.render_block(if i == 64 { &events } else { &[] }, &mut l, &mut r)
                    .unwrap();
                let us = start.elapsed().as_secs_f64() * 1e6;
                if i >= 64 {
                    durations.push(us);
                }
                assert!(l.iter().chain(&r).all(|x| x.is_finite()));
            }
            durations.sort_by(f64::total_cmp);
            eprintln!(
                "{mode} voices={count} median_us={:.1} p99_us={:.1} max_us={:.1} budget_us=2666.7",
                durations[512], durations[1013], durations[1023]
            );
        }
    }
}
