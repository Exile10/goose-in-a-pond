#!/usr/bin/env python3
"""Reference outputs for pond-ear's parity tests from Kaldi and ONNX Runtime on the formula inputs
of tests/common/mod.rs (dev only). Usage and outputs: docs/developer/pond-ear.md."""
import argparse
import math
from pathlib import Path

import numpy as np
import onnxruntime as ort
import kaldi_native_fbank as knf

RATE = 16000
CRATE = Path(__file__).resolve().parent.parent
OUT = CRATE / "tests" / "golden"


class Lcg:
    def __init__(self, seed):
        self.s = seed & 0xFFFFFFFF

    def next(self):
        self.s = (self.s * 1664525 + 1013904223) & 0xFFFFFFFF
        return (self.s >> 8) / 16777216.0

    def bipolar(self):
        return 2.0 * self.next() - 1.0


def quantise(x):
    q = min(max(math.floor(x * 32767.0 + 0.5), -32768.0), 32767.0)
    return np.float32(q / 32768.0)


def formant(f, centre, width):
    return 1.0 / (1.0 + ((f - centre) / width) ** 2)


def voice(n, seed, f0, formants):
    rng = Lcg(seed)
    glide, depth = 2.3, 0.15
    out = np.zeros(n, dtype=np.float32)
    for i in range(n):
        t = i / RATE
        f = f0 * (1.0 + depth * math.sin(2.0 * math.pi * glide * t))
        phase = 2.0 * math.pi * f0 * (t + depth / (2.0 * math.pi * glide) * (1.0 - math.cos(2.0 * math.pi * glide * t)))
        voiced = 0.0
        h = 1.0
        while h * f0 * 1.15 < 7000.0:
            fr = h * f
            a = formant(fr, formants[0], 130.0) + 0.7 * formant(fr, formants[1], 170.0) + 0.4 * formant(fr, formants[2], 250.0)
            voiced += a * math.sin(h * phase)
            h += 1.0
        syl = math.sin(2.0 * math.pi * 3.1 * t)
        g = max(syl, 0.0) ** 0.6
        noise = rng.bipolar()
        fricative = 0.05 * noise if syl < -0.6 else 0.0
        x = 0.08 * voiced * g + fricative + 0.002 * rng.bipolar()
        out[i] = quantise(x)
    return out


def noise(n, seed, amp):
    rng = Lcg(seed)
    return np.array([quantise(amp * rng.bipolar()) for _ in range(n)], dtype=np.float32)


def pattern(frames, a, b):
    v = np.zeros(frames * 80, dtype=np.float32)
    for i in range(frames * 80):
        t, k = float(i // 80), float(i % 80)
        v[i] = np.float32(2.0 * math.sin(a * k + b * t) * math.cos(0.05 * t * k / 80.0))
    return v


def lcg_features(frames, seed):
    rng = Lcg(seed)
    return np.array([np.float32(3.0 * rng.next() - 1.5) for _ in range(frames * 80)], dtype=np.float32)


def write(name, arr):
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / name).write_bytes(np.ascontiguousarray(arr, dtype="<f4").tobytes())
    print(f"{name}: {np.asarray(arr).size} values")


def session(path):
    so = ort.SessionOptions()
    so.intra_op_num_threads = 1
    return ort.InferenceSession(str(path), so, providers=["CPUExecutionProvider"])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--silero")
    ap.add_argument("--resnet")
    a = ap.parse_args()

    clip = voice(RATE // 2, 5, 140.0, (600.0, 1100.0, 2500.0))
    o = knf.FbankOptions()
    o.frame_opts.samp_freq = RATE
    o.frame_opts.dither = 0.0
    o.frame_opts.snip_edges = True
    o.frame_opts.window_type = "hamming"
    o.mel_opts.num_bins = 80
    o.use_energy = False
    f = knf.OnlineFbank(o)
    f.accept_waveform(RATE, (clip * 32768.0).tolist())
    f.input_finished()
    write("fbank.bin", np.stack([f.get_frame(i) for i in range(f.num_frames_ready)]).astype(np.float32))

    wake = session(CRATE / "models" / "hey-goose.onnx")
    name = wake.get_inputs()[0].name
    windows = [pattern(120, 0.21, 0.13), pattern(120, 0.07, 0.31), lcg_features(120, 3), np.zeros(120 * 80, dtype=np.float32)]
    write("wake.bin", np.array([wake.run(None, {name: w.reshape(1, 120, 80)})[0].reshape(-1)[0] for w in windows], dtype=np.float32))

    if a.silero:
        audio = np.concatenate([voice(3 * RATE, 1, 120.0, (700.0, 1220.0, 2600.0)), noise(RATE // 2, 2, 0.05), np.zeros(RATE // 2, dtype=np.float32)])
        s = session(a.silero)
        state = np.zeros((2, 1, 128), dtype=np.float32)
        context = np.zeros((1, 64), dtype=np.float32)
        sr = np.array(RATE, dtype=np.int64)
        probs = []
        for i in range(len(audio) // 512):
            x = np.concatenate([context, audio[None, i * 512:(i + 1) * 512]], axis=1)
            out, state = s.run(None, {"input": x, "state": state, "sr": sr})
            context = x[:, -64:]
            probs.append(float(out[0, 0]))
        write("silero.bin", np.array(probs, dtype=np.float32))

    if a.resnet:
        feats = pattern(200, 0.17, 0.09) + np.float32(0.3) * lcg_features(200, 11)
        r = session(a.resnet)
        emb = r.run(None, {r.get_inputs()[0].name: feats.reshape(1, 200, 80)})[0].reshape(-1)
        write("resnet34.bin", emb.astype(np.float32))


if __name__ == "__main__":
    main()
