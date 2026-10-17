//! The echo canceller in a synthetic room: far end alone, then double talk, then far end alone
//! again, then a new echo path. Every number is printed so the report quotes measurements.

mod common;

use std::sync::OnceLock;

use common::*;
use pond_ear::ear::aec::{Aec, AecConfig, BLOCK, LATENCY};

const S: usize = RATE;

fn energy(x: &[f32]) -> f64 {
    x.iter().map(|&v| v as f64 * v as f64).sum()
}

fn db(num: f64, den: f64) -> f64 {
    10.0 * ((num + 1e-20) / (den + 1e-20)).log10()
}

struct Scene {
    far: Vec<f32>,
    echo: Vec<f32>,
    near: Vec<f32>,
    mic: Vec<f32>,
    rooms: [(usize, Vec<f32>); 2],
}

const DT: (usize, usize) = (6 * S, 10 * S);
const CHANGE: usize = 13 * S;
const END: usize = 20 * S;

/// The reply (two synthetic voices taking turns), its echo through room 1 then room 2, a third
/// voice speaking over it from 6 to 10 s, and a -70 dBFS noise floor.
fn scene() -> &'static Scene {
    static SCENE: OnceLock<Scene> = OnceLock::new();
    SCENE.get_or_init(build_scene)
}

fn build_scene() -> Scene {
    let a = voice(END, 21, 110.0, [650.0, 1_150.0, 2_450.0]);
    let b = voice(END, 22, 190.0, [800.0, 1_400.0, 2_900.0]);
    let far: Vec<f32> = (0..END)
        .map(|i| if (i / (2 * S)).is_multiple_of(2) { a[i] } else { b[i] } * 2.5)
        .collect();
    let r1 = (
        640,
        room(
            3_200,
            640,
            0.25,
            31,
            [(37, 0.3), (91, -0.2), (160, 0.15), (233, -0.1)],
        ),
    );
    let r2 = (
        1_760,
        room(
            4_800,
            1_760,
            0.30,
            32,
            [(53, -0.25), (120, 0.2), (171, -0.15), (260, 0.1)],
        ),
    );
    let e1 = convolve(&far, &r1.1);
    let e2 = convolve(&far, &r2.1);
    let echo: Vec<f32> = (0..END)
        .map(|i| if i < CHANGE { e1[i] } else { e2[i] })
        .collect();
    let talker = voice(DT.1 - DT.0, 23, 230.0, [900.0, 1_700.0, 3_000.0]);
    let near: Vec<f32> = (0..END)
        .map(|i| {
            if (DT.0..DT.1).contains(&i) {
                1.6 * talker[i - DT.0]
            } else {
                0.0
            }
        })
        .collect();
    let floor = noise(END, 24, 0.000_55);
    let mic = (0..END).map(|i| echo[i] + near[i] + floor[i]).collect();
    Scene {
        far,
        echo,
        near,
        mic,
        rooms: [r1, r2],
    }
}

/// Runs the scene as a mixer and a microphone would: far end 100 ms ahead, 10 ms pushes.
fn run(cfg: AecConfig, sc: &Scene, mut at: impl FnMut(usize, &mut Aec)) -> Vec<f32> {
    let mut aec = Aec::new(cfg);
    let (chunk, lead) = (160, 1_600);
    let mut played = 0;
    let mut out = Vec::with_capacity(END);
    let mut m = 0;
    while m < END {
        let want = (m + chunk + lead).min(END);
        if want > played {
            aec.play(played, &sc.far[played..want]);
            played = want;
        }
        aec.process(&sc.mic[m..m + chunk], &mut out);
        m += chunk;
        at(m, &mut aec);
    }
    out.resize(END, 0.0);
    out
}

/// ERLE of `out` in `[a, b)`: echo energy over the energy of what is left of it.
fn erle(sc: &Scene, out: &[f32], a: usize, b: usize) -> f64 {
    let left: Vec<f32> = (a..b).map(|i| out[i] - sc.near[i]).collect();
    db(energy(&sc.echo[a..b]), energy(&left))
}

/// When the 500 ms ERLE first reaches 20 dB and stays there until `until`, in seconds after `from`.
fn settles(sc: &Scene, out: &[f32], from: usize, until: usize) -> Option<f64> {
    let (win, hop) = (S / 2, S / 20);
    let mut t = from;
    let mut first = None;
    while t + win <= until {
        let ok = erle(sc, out, t, t + win) >= 20.0;
        match (ok, first) {
            (true, None) => first = Some(t),
            (false, _) => first = None,
            _ => {}
        }
        t += hop;
    }
    first.map(|f| (f + win - from) as f64 / S as f64)
}

fn misalignment(sc: &Scene, room: usize, aec: &mut Aec) -> f64 {
    let (delay, taps) = aec.response();
    let h = &sc.rooms[room].1;
    let mut err = 0f64;
    for (n, &v) in h.iter().enumerate() {
        let est = n
            .checked_sub(delay)
            .and_then(|j| taps.get(j))
            .copied()
            .unwrap_or(0.0);
        err += (est as f64 - v as f64).powi(2);
    }
    db(err, energy(h))
}

#[test]
fn erle_double_talk_and_a_new_echo_path_in_a_synthetic_room() {
    let sc = scene();
    let echo_dbfs = 10.0 * (energy(&sc.echo[..DT.0]) / DT.0 as f64).log10();
    let near_dbfs = 10.0 * (energy(&sc.near[DT.0..DT.1]) / (DT.1 - DT.0) as f64).log10();
    eprintln!("echo {echo_dbfs:.1} dBFS, near talker {near_dbfs:.1} dBFS, noise floor -70 dBFS");

    let linear = AecConfig {
        suppress: false,
        ..AecConfig::default()
    };
    let (mut before_dt, mut after_dt, mut after_change, mut delays) = (0.0, 0.0, 0.0, Vec::new());
    let out = run(linear, sc, |m, aec| {
        if m == DT.0 {
            before_dt = misalignment(sc, 0, aec);
        } else if m == DT.1 {
            after_dt = misalignment(sc, 0, aec);
        } else if m == END {
            after_change = misalignment(sc, 1, aec);
        }
        if m % (S / 2) == 0 {
            delays.push((
                m as f64 / S as f64,
                aec.estimated_delay(),
                aec.filter_delay(),
            ));
        }
    });

    let tl: Vec<String> = (0..END / (S / 4))
        .map(|k| format!("{:.0}", erle(sc, &out, k * S / 4, (k + 1) * S / 4)))
        .collect();
    eprintln!("linear ERLE per 250 ms: {}", tl.join(" "));
    let converge = settles(sc, &out, 0, DT.0);
    let a = erle(sc, &out, 3 * S, DT.0);
    let b = erle(sc, &out, DT.0, DT.1);
    let c = erle(sc, &out, DT.1, CHANGE);
    let c_worst = (DT.1..CHANGE - S / 10)
        .step_by(S / 10)
        .map(|t| erle(sc, &out, t, t + S / 10))
        .fold(f64::MAX, f64::min);
    let reconverge = settles(sc, &out, CHANGE, END);
    let d = erle(sc, &out, END - 3 * S, END);
    eprintln!("delay track (s, estimate, filter start): {delays:?}");
    eprintln!("linear: converged to 20 dB after {converge:?} s; ERLE {a:.1} dB over 3-6 s");
    eprintln!("linear: double talk 6-10 s ERLE {b:.1} dB; misalignment {before_dt:.1} dB before, {after_dt:.1} dB after");
    eprintln!("linear: far end alone again 10-13 s ERLE {c:.1} dB (worst 100 ms {c_worst:.1} dB)");
    eprintln!("linear: path change at 13 s reconverged to 20 dB after {reconverge:?} s; ERLE {d:.1} dB over 17-20 s; misalignment {after_change:.1} dB");

    assert!(converge.is_some_and(|t| t < 3.0), "never converged");
    assert!(a >= 20.0, "ERLE {a:.1} dB after convergence");
    assert!(
        b >= 10.0,
        "double talk took the filter apart: ERLE {b:.1} dB"
    );
    assert!(
        after_dt <= before_dt + 3.0,
        "double talk moved the filter away from the room"
    );
    assert!(
        c >= 20.0 && c_worst >= 15.0,
        "no recovery after double talk: {c:.1} dB, worst {c_worst:.1} dB"
    );
    assert!(
        reconverge.is_some_and(|t| t < 4.0),
        "never reconverged after the path change"
    );
    assert!(d >= 20.0, "ERLE {d:.1} dB after the path change");

    let out = run(AecConfig::default(), sc, |_, _| {});
    let a = erle(sc, &out, 3 * S, DT.0);
    let c = erle(sc, &out, DT.1, CHANGE);
    let d = erle(sc, &out, END - 3 * S, END);
    // Near-end distortion in double talk: the talker against everything else in the output.
    let dist: Vec<f32> = (DT.0..DT.1).map(|i| out[i] - sc.near[i]).collect();
    let near_snr = db(energy(&sc.near[DT.0..DT.1]), energy(&dist));
    eprintln!("with suppression: ERLE {a:.1} / {c:.1} / {d:.1} dB (3-6, 10-13, 17-20 s); near talker kept at {near_snr:.1} dB SNR in double talk");
    assert!(a >= 30.0 && c >= 30.0 && d >= 25.0);
    assert!(
        near_snr >= 10.0,
        "suppression ate the near talker: {near_snr:.1} dB"
    );
}

#[test]
fn latency_is_two_blocks_and_chunking_changes_nothing() {
    let sc = scene();
    let len = 4 * S;
    let reference = {
        let mut aec = Aec::new(AecConfig::default());
        aec.play(0, &sc.far[..len]);
        let mut out = Vec::new();
        aec.process(&sc.mic[..len], &mut out);
        out
    };
    assert_eq!(reference.len(), len / BLOCK * BLOCK - BLOCK);
    assert_eq!(LATENCY, 2 * BLOCK);
    for chunk in [1usize, 37, 128, 1_000] {
        let mut aec = Aec::new(AecConfig::default());
        aec.play(0, &sc.far[..len]);
        let mut out = Vec::new();
        for c in sc.mic[..len].chunks(chunk) {
            aec.process(c, &mut out);
        }
        assert!(
            out.iter()
                .zip(&reference)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "chunk {chunk}"
        );
    }
}
