"""Reference speech probabilities for tests/against_real_audio.rs.

Runs the model through ONNX Runtime the way the official wrapper does
(snakers4/silero-vad, src/silero_vad/utils_vad.py, OnnxWrapper.__call__): each
call takes 512 new samples with the previous call's last 64 prepended (zeros
first), carries the (2, 1, 128) state and passes sr as a 0-d int64.

    pip install numpy onnxruntime
    python3 reference.py <data_dir>/models/silero/silero_vad.onnx

Writes <clip>.silero.txt beside each clip: one probability per 512-sample window.
"""
import hashlib
import os
import sys
import wave

import numpy as np
import onnxruntime as ort

HERE = os.path.dirname(os.path.abspath(__file__))
CLIPS = ["speech-short.wav", "speech-long.wav"]
WINDOW = 512
CONTEXT = 64


def read_wav(path):
    with wave.open(path) as w:
        assert (w.getnchannels(), w.getsampwidth(), w.getframerate()) == (1, 2, 16000), path
        pcm = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2")
    # Same scaling as pond_voice::dsp::decode_wav.
    return pcm.astype(np.float32) / 32768.0


def probabilities(session, audio):
    state = np.zeros((2, 1, 128), dtype=np.float32)
    context = np.zeros((1, CONTEXT), dtype=np.float32)
    sr = np.array(16000, dtype=np.int64)
    probs = []
    for i in range(len(audio) // WINDOW):
        x = np.concatenate([context, audio[None, i * WINDOW : (i + 1) * WINDOW]], axis=1)
        out, state = session.run(None, {"input": x, "state": state, "sr": sr})
        context = x[..., -CONTEXT:]
        probs.append(float(out[0, 0]))
    return probs


def main():
    model = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("SILERO_MODEL")
    if not model:
        sys.exit("usage: reference.py <silero_vad.onnx> (or set SILERO_MODEL)")
    with open(model, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()

    opts = ort.SessionOptions()
    opts.inter_op_num_threads = 1
    opts.intra_op_num_threads = 1
    session = ort.InferenceSession(model, sess_options=opts, providers=["CPUExecutionProvider"])

    for clip in CLIPS:
        probs = probabilities(session, read_wav(os.path.join(HERE, clip)))
        out = os.path.join(HERE, clip.replace(".wav", ".silero.txt"))
        with open(out, "w") as f:
            f.write(f"# model sha256 {digest}, onnxruntime {ort.__version__}\n")
            f.write(f"# {WINDOW} new samples per window, the previous {CONTEXT} prepended\n")
            f.writelines(f"{p:.9g}\n" for p in probs)
        print(f"{clip}: {len(probs)} windows -> {os.path.basename(out)}")


if __name__ == "__main__":
    main()
