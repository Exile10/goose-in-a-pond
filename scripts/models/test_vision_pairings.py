"""Offline tests for vision_pairings.py: recorded listings from scripts/models/fixtures, GGUF headers
built here. Run: python3 -m unittest discover -s scripts/models -p 'test_*.py'"""

import contextlib
import io
import json
import os
import shutil
import struct
import sys
import tempfile
import unittest
import unittest.mock
import urllib.error

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import vision_pairings as vp  # noqa: E402

FIXTURES = os.path.join(HERE, "fixtures")
TODAY = "2026-10-05"
E4B_QAT = "unsloth/gemma-4-E4B-it-qat-GGUF"
E4B_QAT_COMMIT = "8c5a9e4fd5482e2be20fe0bf013b4c262a8f4265"
E4B_QAT_MMPROJ_SHA = "7c9bafa27f82d658eda805c1d82ef62bb0368e1ff75f64f77de58ad318beaaf9"
E2B = "unsloth/gemma-4-E2B-it-GGUF"
OLD_COMMIT = "0" * 40


# ── GGUF builders ────────────────────────────────────────────────────────────


def gstr(s):
    b = s.encode()
    return struct.pack("<Q", len(b)) + b


def kv_str(key, value):
    return gstr(key) + struct.pack("<I", 8) + gstr(value)


def kv_u32(key, value):
    return gstr(key) + struct.pack("<II", 4, value)


def kv_u64(key, value):
    return gstr(key) + struct.pack("<IQ", 10, value)


def kv_i32(key, value):
    return gstr(key) + struct.pack("<Ii", 5, value)


def kv_f32(key, value):
    return gstr(key) + struct.pack("<If", 6, value)


def kv_bool(key, value):
    return gstr(key) + struct.pack("<I?", 7, value)


def kv_strings(key, values):
    return gstr(key) + struct.pack("<IIQ", 9, 8, len(values)) + b"".join(gstr(v) for v in values)


def kv_i32s(key, values):
    return gstr(key) + struct.pack("<IIQ", 9, 5, len(values)) + struct.pack("<%di" % len(values), *values)


def tensor(name, dims, kind, offset=0):
    return (gstr(name) + struct.pack("<I", len(dims)) + struct.pack("<%dQ" % len(dims), *dims)
            + struct.pack("<IQ", kind, offset))


def gguf(kvs, tensors=(), version=3):
    return b"GGUF" + struct.pack("<IQQ", version, len(tensors), len(kvs)) + b"".join(kvs) + b"".join(tensors)


def align(n, to=32):
    return -(-n // to) * to


def encoder_head(size, dim, projector="gemma4v", legacy=False, vision=True, kind=24):
    """An mmproj header whose one tensor ends exactly at `size`, as a real encoder's table does."""
    kvs = [kv_str("general.architecture", "clip"), kv_str("general.type", "mmproj"),
           kv_bool("clip.has_vision_encoder", vision), kv_u32("clip.vision.projection_dim", dim),
           kv_str("clip.projector_type" if legacy else "clip.vision.projector_type", projector),
           kv_str("clip.audio.projector_type", "gemma4a")]
    start = align(len(gguf(kvs, [tensor("v.patch_embd.weight", [1], kind)])))
    return gguf(kvs, [tensor("v.patch_embd.weight", [size - start], kind)])


def model_head(arch, width, vocab=8):
    return gguf([kv_str("general.architecture", arch), kv_str("general.type", "model"),
                 kv_u32("%s.block_count" % arch, 35), kv_u32("%s.context_length" % arch, 131072),
                 kv_u32("%s.embedding_length" % arch, width),
                 kv_strings("tokenizer.ggml.tokens", ["<tok%d>" % i for i in range(vocab)])])


# ── a recorded Hugging Face ──────────────────────────────────────────────────


class World:
    """A fixtures directory for --fixtures, a sources file and an output path, all in a temp dir."""

    def __init__(self, test):
        self.dir = tempfile.mkdtemp(prefix="vision-pairings-test.")
        test.addCleanup(shutil.rmtree, self.dir)
        self.responses = {}
        self.out = os.path.join(self.dir, "out", "vision-pairings.jsonl")
        self.sources_path = os.path.join(self.dir, "sources.json")
        self.sources(explicit=[])
        self.n = 0

    def _file(self, data):
        self.n += 1
        name = "r%03d" % self.n
        with open(os.path.join(self.dir, name), "wb") as f:
            f.write(data)
        return name

    def respond(self, url, data=None, status=None, error=None):
        if error is not None:
            self.responses[url] = {"error": error}
        elif status is not None and status >= 400:
            self.responses[url] = {"status": status}
        else:
            self.responses[url] = {"file": self._file(data)}

    def listing(self, fixture, rev="main", edit=None):
        """Serve fixtures/<fixture> as the listing at `rev`; `edit` may change it first."""
        with open(os.path.join(FIXTURES, fixture), encoding="utf-8") as f:
            data = json.load(f)
        if edit:
            edit(data)
        self.respond(vp.revision_url(data["id"], rev), json.dumps(data).encode())
        return data

    def heads(self, data, dim, width, arch="gemma4", encoder_kind=24):
        """Serve a matching header for every encoder and model file in a listing."""
        files = {s["rfilename"]: s for s in data["siblings"]}
        for path, s in files.items():
            url = vp.resolve_url(data["id"], data["sha"], path)
            if vp.is_encoder_file(path):
                self.respond(url, encoder_head(s["size"], dim, kind=encoder_kind))
            elif vp.is_model_file(path):
                self.respond(url, model_head(arch, width))

    def search(self, term, author, ids, limit=20):
        self.respond(vp.search_url(term, author, limit), json.dumps([{"id": i} for i in ids]).encode())

    def sources(self, explicit, families=(), authors=("unsloth", "example"),
                upstream_orgs=("google",), limit=20):
        with open(self.sources_path, "w", encoding="utf-8") as f:
            json.dump({"explicit": list(explicit), "families": [{"name": t, "search": t} for t in families],
                       "authors": list(authors), "upstream_orgs": list(upstream_orgs),
                       "search_limit": limit}, f)

    def existing(self, lines, canonical=True):
        os.makedirs(os.path.dirname(self.out), exist_ok=True)
        with open(self.out, "w", encoding="utf-8") as f:
            f.write(vp.render(lines) if canonical else "".join(json.dumps(line, indent=None) + "\n" for line in lines))

    def transport(self):
        with open(os.path.join(self.dir, "responses.json"), "w", encoding="utf-8") as f:
            json.dump(self.responses, f)
        return vp.FixtureTransport(self.dir)

    def run(self, *args, today=TODAY):
        self.transport()
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = vp.main(["--fixtures", self.dir, "--out", self.out, "--sources", self.sources_path,
                            "--today", today, "--jobs", "3", *args])
        return code, out.getvalue(), err.getvalue()

    def lines(self):
        with open(self.out, encoding="utf-8") as f:
            return [json.loads(line) for line in f]

    def text(self):
        with open(self.out, encoding="utf-8") as f:
            return f.read()


# ── GGUF ─────────────────────────────────────────────────────────────────────


class GgufTest(unittest.TestCase):
    def test_reads_every_scalar_and_steps_over_arrays(self):
        data = gguf([
            kv_str("general.architecture", "gemma4"), kv_i32("general.sampling.top_k", -64),
            kv_f32("general.sampling.temp", 1.0), kv_bool("gemma4.flag", True),
            kv_u64("general.parameter_count", 7_500_000_000),
            kv_strings("tokenizer.ggml.tokens", ["<pad>", "<eos>", "hello"]),
            kv_i32s("tokenizer.ggml.token_type", [3, 3, 1]),
            kv_u32("gemma4.embedding_length", 2560)])
        head = vp.parse_gguf(data)
        self.assertTrue(head.kv_complete)
        self.assertFalse(head.short)
        self.assertEqual(head.string("general.architecture"), "gemma4")
        self.assertEqual(head.kv["general.sampling.top_k"], (5, -64))
        self.assertEqual(head.kv["general.sampling.temp"], (6, 1.0))
        self.assertIs(head.flag("gemma4.flag"), True)
        self.assertEqual(head.kv["general.parameter_count"], (10, 7_500_000_000))
        self.assertEqual(head.kv["tokenizer.ggml.tokens"], (9, None))
        self.assertEqual(head.embedding_length(), 2560)
        self.assertEqual(head.data_end, align(len(data)))

    def test_a_short_buffer_keeps_what_it_read(self):
        data = gguf([kv_str("general.architecture", "gemma4"), kv_u32("gemma4.embedding_length", 2560),
                     kv_strings("tokenizer.ggml.tokens", ["t%d" % i for i in range(1000)]),
                     kv_u32("gemma4.after_the_array", 7)])
        head = vp.parse_gguf(data[:len(data) // 2])
        self.assertTrue(head.short)
        self.assertFalse(head.kv_complete)
        self.assertEqual(head.embedding_length(), 2560)
        self.assertIsNone(head.u32("gemma4.after_the_array"))

    def test_bytes_that_are_not_gguf_v2_or_v3_never_raise(self):
        self.assertEqual(vp.parse_gguf(b"").short, True)
        self.assertEqual(vp.parse_gguf(b"PK\x03\x04" + b"\0" * 40).problem, "not a GGUF file")
        self.assertEqual(vp.parse_gguf(gguf([], version=1)).problem, "GGUF version 1")
        self.assertIn("value type 99", vp.parse_gguf(
            gguf([gstr("x") + struct.pack("<I", 99)])).problem)

    def test_an_array_of_arrays_stops_the_walk_as_pond_core_does(self):
        nested = gstr("x") + struct.pack("<IIQ", 9, 9, 1)
        head = vp.parse_gguf(gguf([kv_str("general.architecture", "clip"), nested]))
        self.assertEqual(head.problem, "an array of value type 9")
        self.assertEqual(head.string("general.architecture"), "clip")
        self.assertFalse(head.kv_complete)

    def test_embedding_length_is_the_last_u32_suffix_match(self):
        head = vp.parse_gguf(gguf([kv_str("general.architecture", "gemma4"),
                                   kv_u32("gemma4.embedding_length", 2560),
                                   kv_u32("gemma4.embedding_length_per_layer_input", 256),
                                   kv_u64("other.embedding_length", 9999)]))
        self.assertEqual(head.embedding_length(), 2560)
        head = vp.parse_gguf(gguf([kv_u32("llama.embedding_length", 576),
                                   kv_u32("gemma3.embedding_length", 2560)]))
        self.assertEqual(head.embedding_length(), 2560)

    def test_data_end_follows_the_tensor_table(self):
        kvs = [kv_str("general.architecture", "clip"), kv_u32("general.alignment", 64)]
        tensors = [tensor("a", [256, 4], 1, 0), tensor("b", [512], 8, 4096), tensor("c", [256], 12, 2048)]
        data = gguf(kvs, tensors)
        head = vp.parse_gguf(data)
        self.assertEqual(head.alignment, 64)
        self.assertEqual(head.data_end, align(len(data), 64) + 4096 + 512 // 32 * 34)
        self.assertIsNone(head.problem)

    def test_a_tensor_type_pond_core_cannot_measure_leaves_no_data_end(self):
        head = vp.parse_gguf(gguf([kv_str("general.architecture", "clip")], [tensor("a", [256], 16)]))
        self.assertIsNone(head.data_end)
        self.assertIn("tensor type 16", head.problem)

    def test_the_projector_falls_back_to_the_legacy_key(self):
        head = vp.parse_gguf(encoder_head(65536, 2048, projector="mlp", legacy=True))
        self.assertEqual(head.projector(), "mlp")
        self.assertIsNone(vp.encoder_problem(head, 65536))

    def test_a_header_past_the_first_read_is_read_once_more_wider(self):
        w = World(self)
        tokens = kv_strings("tokenizer.ggml.tokens", ["token%04d" % i for i in range(200)])
        w.respond(vp.resolve_url("a/b", "c" * 40, "m.gguf"),
                  gguf([kv_str("general.architecture", "gemma4"), tokens, kv_u32("gemma4.embedding_length", 2560)]))
        w.respond(vp.resolve_url("a/b", "c" * 40, "mmproj.gguf"), encoder_head(1 << 20, 2560))
        t = w.transport()
        with unittest.mock.patch.object(vp, "HEAD_BYTES", 256):
            head = vp.read_head(t, "a/b", "c" * 40, "m.gguf", lambda h: h.embedding_length() is not None)
            self.assertEqual((head.embedding_length(), len(t.requested)), (2560, 2))
            with unittest.mock.patch.object(vp, "MAX_HEAD_BYTES", 300):
                head = vp.read_head(t, "a/b", "c" * 40, "m.gguf", lambda h: h.embedding_length() is not None)
                self.assertIsNone(head.embedding_length())
                head = vp.read_head(t, "a/b", "c" * 40, "mmproj.gguf", lambda h: h.data_end is not None)
                self.assertIn("runs past the 300 bytes read", vp.encoder_problem(head, 1 << 20))

    def test_encoder_checks_mirror_validate_encoder_header(self):
        good = vp.parse_gguf(encoder_head(65536, 2560))
        self.assertIsNone(vp.encoder_problem(good, 65536))
        self.assertIn("describes 65536 bytes", vp.encoder_problem(good, 65000))
        self.assertIn("longer than its header", vp.encoder_problem(good, 65536 + 64))
        self.assertIn("vision tower", vp.encoder_problem(vp.parse_gguf(encoder_head(65536, 2560, vision=False)), 65536))
        self.assertIn("tensor type 16", vp.encoder_problem(vp.parse_gguf(encoder_head(65536, 2560, kind=16)), 65536))
        self.assertIn("not clip", vp.encoder_problem(vp.parse_gguf(model_head("gemma4", 2560)), 65536))


# ── files ────────────────────────────────────────────────────────────────────


class FilesTest(unittest.TestCase):
    def test_model_files_leave_out_companions_and_later_shards(self):
        with open(os.path.join(FIXTURES, "revision-unsloth-gemma-4-E4B-it-qat-GGUF.json")) as f:
            names = [s["rfilename"] for s in json.load(f)["siblings"]]
        self.assertEqual(sorted(n for n in names if vp.is_model_file(n)),
                         ["gemma-4-E4B-it-qat-UD-Q2_K_XL.gguf", "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf"])
        self.assertEqual(sorted(n for n in names if vp.is_encoder_file(n)),
                         ["mmproj-BF16.gguf", "mmproj-F16.gguf", "mmproj-F32.gguf"])
        for name in ("BF16/m-BF16-00002-of-00003.gguf", "m-imatrix.gguf", "gemma-4-E2B-it-assistant-Q8_0.gguf",
                     "draft-m-Q4_0.gguf", "imatrix_unsloth.gguf_file", "README.md"):
            self.assertFalse(vp.is_model_file(name), name)
        self.assertTrue(vp.is_model_file("BF16/m-BF16-00001-of-00003.gguf"))

    def test_a_dflash_drafter_is_no_chat_model_by_name_or_by_header(self):
        # ggml-org ships `dflash-<model>-<quant>.gguf` beside each model; sorted first, it was the file
        # whose header named the pairing's architecture, and its name was listed as a chat model.
        for name in ("dflash-gemma-4-26B-A4B-it-BF16.gguf", "sub/dflash-gemma-4-31B-it-Q8_0.gguf"):
            self.assertFalse(vp.is_model_file(name), name)
        self.assertTrue(vp.is_model_file("gemma-4-26B-A4B-it-Q8_0.gguf"))
        self.assertIn("companion", vp.model_problem(vp.parse_gguf(model_head("dflash", 2816))))
        self.assertIsNone(vp.model_problem(vp.parse_gguf(model_head("gemma4", 2816))))

    def test_quantisation_is_read_as_goose_reads_it(self):
        for name, quant in (("mmproj-BF16.gguf", "BF16"), ("mmproj-model-f16.gguf", "f16"),
                            ("Q4_K_M/m-Q4_K_M-00001-of-00002.gguf", "Q4_K_M"), ("model.Q8_0.gguf", "Q8_0"),
                            ("mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf", "Q8_0"), ("model.gguf", "unknown")):
            self.assertEqual(vp.parse_quantization(name), quant, name)

    def test_the_encoder_is_the_nearest_directory_then_bf16_f16_f32(self):
        choose = vp.choose_encoder
        self.assertEqual(choose("m-Q4_K_M.gguf", ["mmproj-F32.gguf", "mmproj-BF16.gguf", "mmproj-F16.gguf"]),
                         "mmproj-BF16.gguf")
        self.assertEqual(choose("m-Q8_0.gguf", ["mmproj-F16.gguf", "mmproj-BF16.gguf"]), "mmproj-BF16.gguf")
        # goose's quant proximity would take the Q8_0 encoder for a Q4 model; one line serves every quant.
        self.assertEqual(choose("m-Q4_K_M.gguf", ["mmproj-m-Q8_0.gguf", "mmproj-m-f16.gguf"]), "mmproj-m-f16.gguf")
        self.assertEqual(choose("Q4_K_M/m-Q4_K_M.gguf", ["mmproj-BF16.gguf", "Q4_K_M/mmproj-F32.gguf"]),
                         "Q4_K_M/mmproj-F32.gguf")
        self.assertEqual(choose("Q4_K_M/m-Q4_K_M.gguf", ["Q8_0/mmproj-BF16.gguf", "mmproj-F32.gguf"]),
                         "mmproj-F32.gguf")
        self.assertEqual(choose("m.gguf", ["mmproj-b-Q8_0.gguf", "mmproj-a-Q4_0.gguf"]), "mmproj-a-Q4_0.gguf")
        self.assertIsNone(choose("m.gguf", ["sub/mmproj-F16.gguf"]))

    def test_files_are_grouped_by_the_encoder_they_take(self):
        with open(os.path.join(FIXTURES, "revision-example-subdirs-GGUF.json")) as f:
            files = {s["rfilename"]: (s["size"], None) for s in json.load(f)["siblings"]}
        encoders, groups, orphans = vp.group_files(files)
        self.assertEqual(groups, {"BF16/mmproj-BF16.gguf": ["BF16/subdirs-BF16-00001-of-00002.gguf"],
                                  "mmproj-F16.gguf": ["Q8_0/subdirs-Q8_0.gguf", "subdirs-Q4_K_M.gguf"]})
        self.assertEqual(orphans, [])


# ── naming and discovery ─────────────────────────────────────────────────────


class NamingTest(unittest.TestCase):
    def test_labels_are_family_and_size(self):
        for repo, label in (("unsloth/gemma-4-E4B-it-qat-GGUF", "Gemma 4 E4B"),
                            ("unsloth/gemma-4-12b-it-GGUF", "Gemma 4 12B"),
                            ("unsloth/gemma-4-12B-it-qat-GGUF", "Gemma 4 12B"),
                            ("unsloth/gemma-4-26B-A4B-it-GGUF", "Gemma 4 26B A4B"),
                            ("bartowski/google_gemma-3-4b-it-GGUF", "Gemma 3 4B"),
                            ("lmstudio-community/gemma-4-E2B-it-QAT-GGUF", "Gemma 4 E2B"),
                            ("unsloth/Qwen2.5-VL-7B-Instruct-GGUF", "Qwen2.5 VL 7B"),
                            ("unsloth/Qwen3-VL-8B-Thinking-1M-GGUF", "Qwen3 VL 8B Thinking 1M"),
                            ("ggml-org/SmolVLM2-2.2B-Instruct-GGUF", "SmolVLM2 2.2B"),
                            ("ggml-org/Qwen3-VL-30B-A3B-Instruct-Q8_0-GGUF", "Qwen3 VL 30B A3B Q8_0")):
            self.assertEqual(vp.derive_label(repo), label, repo)

    def test_search_results_keep_only_the_family_under_upstream_names(self):
        orgs = ["google", "qwen"]
        self.assertTrue(vp.wanted("bartowski/google_gemma-4-E2B-it-GGUF", "gemma-4", orgs))
        self.assertTrue(vp.wanted("bartowski/gemma-4-12B-it-GGUF", "gemma-4", orgs))
        self.assertTrue(vp.wanted("ggml-org/Qwen3-VL-30B-A3B-Instruct-Q8_0-GGUF", "Qwen3-VL", orgs))
        self.assertFalse(vp.wanted("bartowski/Ateron_Gemma-4-Dark-Thoughts-31B-GGUF", "gemma-4", orgs))
        self.assertFalse(vp.wanted("bartowski/thesby_Qwen2.5-VL-7B-NSFW-Caption-V3-GGUF", "Qwen2.5-VL", orgs))
        self.assertFalse(vp.wanted("unsloth/gemma-4-31B-it-GGUF", "gemma-3", orgs))

    def test_recorded_searches_keep_first_party_builds_once_each(self):
        w = World(self)
        w.sources(explicit=[E4B_QAT], families=["gemma-4"], authors=["unsloth", "bartowski"])
        for author in ("unsloth", "bartowski"):
            with open(os.path.join(FIXTURES, "search-gemma-4-%s.json" % author), "rb") as f:
                w.respond(vp.search_url("gemma-4", author, 20), f.read())
        queue = vp.discover(w.transport(), vp.load_sources(w.sources_path), [])
        self.assertEqual(queue[0], (E4B_QAT, "explicit"))
        self.assertEqual([repo for repo, kind in queue[1:]], [
            "bartowski/gemma-4-12B-it-GGUF", "bartowski/google_gemma-4-26B-A4B-it-GGUF",
            "bartowski/google_gemma-4-31B-it-GGUF", "bartowski/google_gemma-4-E2B-it-GGUF",
            "bartowski/google_gemma-4-E4B-it-GGUF", "unsloth/gemma-4-12B-it-qat-GGUF",
            "unsloth/gemma-4-12b-it-GGUF", "unsloth/gemma-4-26B-A4B-it-GGUF", "unsloth/gemma-4-26B-A4B-it-qat-GGUF",
            "unsloth/gemma-4-31B-it-GGUF", "unsloth/gemma-4-31B-it-qat-GGUF", "unsloth/gemma-4-E2B-it-GGUF",
            "unsloth/gemma-4-E2B-it-qat-GGUF", "unsloth/gemma-4-E2B-it-qat-mobile-GGUF",
            "unsloth/gemma-4-E4B-it-GGUF"])

    def test_the_plain_directory_is_the_repository_name(self):
        self.assertEqual(vp.bare_dir("unsloth/gemma-4-E4B-it-qat-GGUF"), "gemma-4-e4b-it-qat")
        self.assertEqual(vp.bare_dir("lmstudio-community/Qwen3-VL-235B-A22B-Instruct"),
                         "qwen3-vl-235b-a22b-instruct")


# ── generating the table ─────────────────────────────────────────────────────


class GenerateTest(unittest.TestCase):
    def world_with_e4b_qat(self):
        w = World(self)
        w.sources(explicit=[E4B_QAT])
        w.heads(w.listing("revision-unsloth-gemma-4-E4B-it-qat-GGUF.json"), dim=2560, width=2560)
        return w

    def test_a_recorded_listing_becomes_one_pinned_line(self):
        w = self.world_with_e4b_qat()
        code, out, err = w.run()
        self.assertEqual(code, vp.EXIT_OK, err)
        self.assertEqual(w.lines(), [{
            "model_repo": E4B_QAT,
            "model_files": ["gemma-4-E4B-it-qat-UD-Q2_K_XL.gguf", "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf"],
            "architecture": "gemma4", "embedding_length": 2560, "label": "Gemma 4 E4B",
            "encoder": {"dir": "gemma-4-e4b-it-qat", "repo": E4B_QAT, "revision": E4B_QAT_COMMIT,
                        "filename": "mmproj-BF16.gguf", "size_bytes": 991552320,
                        "sha256": E4B_QAT_MMPROJ_SHA, "projector": "gemma4v", "projection_dim": 2560},
            "checked_at": TODAY}])
        self.assertIn("+ %s mmproj-BF16.gguf" % E4B_QAT, out)
        self.assertTrue(w.text().startswith('{"model_repo":"%s","model_files":[' % E4B_QAT))

    def test_only_the_chosen_encoder_and_one_model_file_are_read(self):
        w = self.world_with_e4b_qat()
        recorder = w.transport()
        with contextlib.redirect_stderr(io.StringIO()):
            vp.generate(recorder, vp.load_sources(w.sources_path), [], TODAY)
        self.assertEqual(sorted(u.rsplit("/", 1)[1] for u in recorder.requested if "/resolve/" in u),
                         ["gemma-4-E4B-it-qat-UD-Q2_K_XL.gguf", "mmproj-BF16.gguf"])

    def test_a_table_that_differs_only_in_form_is_rewritten(self):
        w = self.world_with_e4b_qat()
        w.run()
        w.existing(w.lines(), canonical=False)
        code, out, _ = w.run("--check")
        self.assertEqual(code, vp.EXIT_STALE)
        self.assertIn("same pairings, not in the generator's form", out)
        self.assertEqual(w.run()[0], vp.EXIT_OK)
        self.assertEqual(w.text(), vp.render(w.lines()))

    def test_a_projection_dim_mismatch_is_reported_and_left_out(self):
        w = World(self)
        w.sources(explicit=["example/mismatch-GGUF"])
        w.heads(w.listing("revision-example-mismatch-GGUF.json"), dim=2048, width=4096, arch="llama")
        code, out, err = w.run()
        self.assertEqual(code, vp.EXIT_OK, err)
        self.assertFalse(os.path.exists(w.out))
        self.assertIn("skip example/mismatch-GGUF: mmproj-F16.gguf does not fit: "
                      "projection_dim 2048, embedding_length 4096", err)

    def test_text_only_and_refused_encoders_are_skipped_with_the_reason(self):
        w = World(self)
        w.sources(explicit=["example/text-GGUF", "example/mismatch-GGUF"])
        w.listing("revision-example-text-GGUF.json")
        w.heads(w.listing("revision-example-mismatch-GGUF.json"), dim=2560, width=2560, encoder_kind=16)
        code, _, err = w.run()
        self.assertEqual(code, vp.EXIT_OK, err)
        self.assertFalse(os.path.exists(w.out))
        self.assertIn("skip example/text-GGUF: text only: no mmproj file", err)
        self.assertIn("mmproj-F16.gguf would be refused: tensor type 16, which pond-core cannot measure", err)

    def test_subdirectories_give_one_line_per_encoder(self):
        w = World(self)
        w.sources(explicit=["example/subdirs-GGUF"])
        w.heads(w.listing("revision-example-subdirs-GGUF.json"), dim=5120, width=5120, arch="qwen3vl")
        code, _, err = w.run()
        self.assertEqual(code, vp.EXIT_OK, err)
        got = [(l["encoder"]["filename"], l["encoder"]["dir"], l["model_files"]) for l in w.lines()]
        self.assertEqual(got, [
            ("BF16/mmproj-BF16.gguf", "subdirs", ["BF16/subdirs-BF16-00001-of-00002.gguf"]),
            ("mmproj-F16.gguf", "example-subdirs", ["Q8_0/subdirs-Q8_0.gguf", "subdirs-Q4_K_M.gguf"])])

    def test_the_pin_holds_while_the_bytes_are_unchanged(self):
        w = self.world_with_e4b_qat()
        w.run()
        line = w.lines()[0]
        line["encoder"]["revision"] = OLD_COMMIT
        line["checked_at"] = "2026-01-01"
        w.existing([line])
        w.listing("revision-unsloth-gemma-4-E4B-it-qat-GGUF.json", rev=OLD_COMMIT)
        before = w.text()
        code, out, err = w.run("--check")
        self.assertEqual(code, vp.EXIT_OK, out + err)
        self.assertIn("up to date, 1 pairings", out)
        code, _, err = w.run(today="2026-12-24")
        self.assertEqual(code, vp.EXIT_OK, err)
        self.assertEqual(w.text(), before)

    def test_the_pin_moves_when_its_commit_is_gone(self):
        w = self.world_with_e4b_qat()
        w.run()
        line = w.lines()[0]
        line["encoder"]["revision"] = OLD_COMMIT
        line["checked_at"] = "2026-01-01"
        w.existing([line])
        w.respond(vp.revision_url(E4B_QAT, OLD_COMMIT), status=404)
        code, out, err = w.run(today="2026-12-24")
        self.assertEqual(code, vp.EXIT_OK, err)
        self.assertIn("pin moved", err)
        self.assertIn("~ %s mmproj-BF16.gguf: encoder.revision" % E4B_QAT, out)
        self.assertEqual((w.lines()[0]["encoder"]["revision"], w.lines()[0]["checked_at"]),
                         (E4B_QAT_COMMIT, "2026-12-24"))

    def test_the_pin_moves_when_the_bytes_change(self):
        w = self.world_with_e4b_qat()
        w.run()
        line = w.lines()[0]
        line["encoder"]["revision"] = OLD_COMMIT
        line["encoder"]["sha256"] = "f" * 64
        w.existing([line])
        code, out, err = w.run(today="2026-12-24")
        self.assertEqual(code, vp.EXIT_OK, err)
        self.assertIn("encoder.revision, encoder.sha256", out)
        self.assertEqual(w.lines()[0]["encoder"]["revision"], E4B_QAT_COMMIT)
        self.assertNotIn(vp.revision_url(E4B_QAT, OLD_COMMIT), w.responses)

    def test_checked_at_moves_only_with_the_line(self):
        w = World(self)
        w.sources(explicit=[E4B_QAT, E2B])
        w.heads(w.listing("revision-unsloth-gemma-4-E4B-it-qat-GGUF.json"), dim=2560, width=2560)
        w.heads(w.listing("revision-unsloth-gemma-4-E2B-it-GGUF.json"), dim=1536, width=1536)
        self.assertEqual(w.run(today="2026-10-05")[0], vp.EXIT_OK)
        first = w.text()
        self.assertEqual(w.run(today="2026-10-12")[0], vp.EXIT_OK)
        self.assertEqual(w.text(), first)

        def add_quant(data):
            data["siblings"].append({"rfilename": "gemma-4-E2B-it-UD-Q3_K_M.gguf", "size": 2_600_000_000,
                                     "lfs": {"sha256": "a" * 64, "size": 2_600_000_000}})
        w.listing("revision-unsloth-gemma-4-E2B-it-GGUF.json", edit=add_quant)
        code, out, _ = w.run(today="2026-10-19")
        self.assertEqual(code, vp.EXIT_OK)
        self.assertIn("~ %s mmproj-BF16.gguf: model_files" % E2B, out)
        dates = {l["model_repo"]: l["checked_at"] for l in w.lines()}
        self.assertEqual(dates, {E2B: "2026-10-19", E4B_QAT: "2026-10-05"})

    def test_check_reports_a_stale_table_and_writes_nothing(self):
        w = self.world_with_e4b_qat()
        code, out, _ = w.run("--check")
        self.assertEqual(code, vp.EXIT_STALE)
        self.assertIn("1 added, 0 changed, 0 removed", out)
        self.assertFalse(os.path.exists(w.out))
        w.run()
        line = w.lines()[0]
        line["model_files"] = line["model_files"][:1]
        w.existing([line])
        stale = w.text()
        code, out, _ = w.run("--check")
        self.assertEqual(code, vp.EXIT_STALE)
        self.assertIn("~ %s mmproj-BF16.gguf: model_files" % E4B_QAT, out)
        self.assertEqual(w.text(), stale)

    def test_a_failed_request_writes_nothing(self):
        w = self.world_with_e4b_qat()
        w.run()
        before = w.text()
        mtime = os.stat(w.out).st_mtime_ns
        w.respond(vp.resolve_url(E4B_QAT, E4B_QAT_COMMIT, "mmproj-BF16.gguf"), error="timed out")
        code, out, err = w.run(today="2026-12-24")
        self.assertEqual(code, vp.EXIT_FETCH)
        self.assertIn("nothing was written", err)
        self.assertEqual(out, "")
        self.assertEqual((w.text(), os.stat(w.out).st_mtime_ns), (before, mtime))
        del w.responses[vp.resolve_url(E4B_QAT, E4B_QAT_COMMIT, "mmproj-BF16.gguf")]
        self.assertEqual(w.run(today="2026-12-24")[0], vp.EXIT_FETCH)
        self.assertEqual(w.text(), before)

    def test_an_explicit_repository_that_is_gone_stops_the_run(self):
        w = World(self)
        w.sources(explicit=["example/gone-GGUF"])
        w.respond(vp.revision_url("example/gone-GGUF", "main"), status=404)
        code, _, err = w.run()
        self.assertEqual(code, vp.EXIT_FETCH)
        self.assertIn("explicit list but answered HTTP 404", err)
        self.assertFalse(os.path.exists(w.out))

    def test_output_is_sorted_and_the_same_from_any_search_order(self):
        w = World(self)
        w.sources(explicit=[], families=["gemma-4"], authors=["unsloth", "example"])
        w.heads(w.listing("revision-unsloth-gemma-4-E4B-it-qat-GGUF.json"), dim=2560, width=2560)
        w.heads(w.listing("revision-unsloth-gemma-4-E2B-it-GGUF.json"), dim=1536, width=1536)
        w.heads(w.listing("revision-example-gemma-4-E2B-it-GGUF.json"), dim=1536, width=1536)
        w.search("gemma-4", "unsloth", [E4B_QAT, E2B, "unsloth/gemma-3-4b-it-GGUF"])
        w.search("gemma-4", "example", ["example/gemma-4-E2B-it-GGUF", "example/Other_gemma-4-x-GGUF"])
        self.assertEqual(w.run()[0], vp.EXIT_OK)
        first = w.text()
        self.assertEqual([l["model_repo"] for l in w.lines()], ["example/gemma-4-E2B-it-GGUF", E2B, E4B_QAT])
        os.remove(w.out)
        w.search("gemma-4", "unsloth", ["unsloth/gemma-3-4b-it-GGUF", E2B, E4B_QAT])
        w.search("gemma-4", "example", ["example/Other_gemma-4-x-GGUF", "example/gemma-4-E2B-it-GGUF"])
        self.assertEqual(w.run("--jobs", "1")[0], vp.EXIT_OK)
        self.assertEqual(w.text(), first)

    def test_directories_keep_the_convention_and_never_collide(self):
        w = World(self)
        w.sources(explicit=[E2B], families=["gemma-4"], authors=["unsloth", "example"])
        w.heads(w.listing("revision-unsloth-gemma-4-E2B-it-GGUF.json"), dim=1536, width=1536)
        w.heads(w.listing("revision-example-gemma-4-E2B-it-GGUF.json"), dim=1536, width=1536)
        w.search("gemma-4", "unsloth", [])
        w.search("gemma-4", "example", ["example/gemma-4-E2B-it-GGUF"])
        self.assertEqual(w.run()[0], vp.EXIT_OK)
        dirs = {l["model_repo"]: l["encoder"]["dir"] for l in w.lines()}
        self.assertEqual(dirs, {E2B: "gemma-4-e2b-it", "example/gemma-4-E2B-it-GGUF": "example-gemma-4-e2b-it"})
        lines = w.lines()
        lines[0]["encoder"]["dir"] = "kept-from-before"
        w.existing(lines)
        self.assertEqual(w.run()[0], vp.EXIT_OK)
        dirs = {l["model_repo"]: l["encoder"]["dir"] for l in w.lines()}
        self.assertEqual(dirs["example/gemma-4-E2B-it-GGUF"], "kept-from-before")
        self.assertEqual(dirs[E2B], "gemma-4-e2b-it")

    def test_lines_already_in_the_table_are_rescanned_and_unknown_ones_dropped(self):
        w = World(self)
        w.sources(explicit=[], families=["gemma-4"], authors=["unsloth"])
        w.search("gemma-4", "unsloth", [])
        w.heads(w.listing("revision-unsloth-gemma-4-E4B-it-qat-GGUF.json"), dim=2560, width=2560)
        stranger = {"model_repo": "someone/else-GGUF", "model_files": ["else.gguf"], "architecture": "llama",
                    "embedding_length": 1, "label": "Else", "checked_at": "2026-01-01",
                    "encoder": {"dir": "else", "repo": "someone/else-GGUF", "revision": OLD_COMMIT,
                                "filename": "mmproj-F16.gguf", "size_bytes": 1, "sha256": "e" * 64,
                                "projector": "mlp", "projection_dim": 1}}
        carried = dict(stranger, model_repo=E4B_QAT, encoder=dict(stranger["encoder"], filename="mmproj-BF16.gguf"))
        w.existing([carried, stranger])
        code, out, err = w.run()
        self.assertEqual(code, vp.EXIT_OK, err)
        self.assertEqual([l["model_repo"] for l in w.lines()], [E4B_QAT])
        self.assertIn("- someone/else-GGUF mmproj-F16.gguf (no longer scanned)", out)

    def test_a_removed_line_says_why(self):
        w = self.world_with_e4b_qat()
        w.run()
        line = w.lines()[0]
        line["encoder"]["filename"] = "mmproj-F16.gguf"
        w.existing([line])
        code, out, _ = w.run("--check")
        self.assertEqual(code, vp.EXIT_STALE)
        self.assertIn("+ %s mmproj-BF16.gguf" % E4B_QAT, out)
        self.assertIn("- %s mmproj-F16.gguf (this repository pairs with another encoder now)" % E4B_QAT, out)

    def test_a_found_repository_that_answers_404_is_skipped(self):
        w = World(self)
        w.sources(explicit=[], families=["gemma-4"], authors=["unsloth"])
        w.search("gemma-4", "unsloth", ["unsloth/gemma-4-gone-GGUF"])
        w.respond(vp.revision_url("unsloth/gemma-4-gone-GGUF", "main"), status=404)
        code, _, err = w.run()
        self.assertEqual(code, vp.EXIT_OK, err)
        self.assertIn("skip unsloth/gemma-4-gone-GGUF: unavailable (HTTP 404)", err)

    def test_bad_input_is_refused_before_any_request(self):
        w = World(self)
        with open(w.sources_path, "w") as f:
            json.dump({"explicit": ["not a repo"], "families": [], "authors": [], "upstream_orgs": [],
                       "search_limit": 20}, f)
        self.assertEqual(w.run()[0], vp.EXIT_INPUT)
        w.sources(explicit=[])
        w.existing([])
        with open(w.out, "w") as f:
            f.write("{not json\n")
        code, _, err = w.run()
        self.assertEqual(code, vp.EXIT_INPUT)
        self.assertIn("vision-pairings.jsonl:1 is not JSON", err)


# ── the network ──────────────────────────────────────────────────────────────


class _Response:
    def __init__(self, body):
        self.body = io.BytesIO(body)

    def read(self, n=-1):
        return self.body.read(n)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class TransportTest(unittest.TestCase):
    def transport(self, answers, **kw):
        self.sleeps, self.requests = [], []

        def opener(req, timeout):
            self.requests.append(req)
            answer = answers.pop(0)
            if isinstance(answer, Exception):
                raise answer
            return _Response(answer)
        return vp.HttpTransport(opener=opener, sleep=self.sleeps.append, **kw)

    @staticmethod
    def http_error(code, headers=None):
        return urllib.error.HTTPError("https://huggingface.co/x", code, "status", headers or {}, None)

    def test_backs_off_on_429_and_honours_retry_after(self):
        t = self.transport([self.http_error(429, {"Retry-After": "7"}), self.http_error(429), b"ok"])
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(t.get("https://huggingface.co/api/models"), b"ok")
        self.assertEqual(self.sleeps, [7.0, 4.0])

    def test_server_errors_and_drops_are_retried_then_fatal(self):
        t = self.transport([self.http_error(503), OSError("reset"), b"ok"])
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(t.get("https://huggingface.co/a"), b"ok")
        t = self.transport([self.http_error(500)] * 3, retries=2)
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(vp.FetchError):
            t.get("https://huggingface.co/a")
        self.assertEqual(len(self.sleeps), 2)

    def test_401_403_404_are_answers_not_failures(self):
        for code in (401, 403, 404):
            t = self.transport([self.http_error(code)])
            with self.assertRaises(vp.Unavailable) as caught:
                t.get("https://huggingface.co/a")
            self.assertEqual((caught.exception.status, self.sleeps), (code, []))
        with self.assertRaises(vp.FetchError):
            self.transport([self.http_error(400)]).get("https://huggingface.co/a")

    def test_a_range_read_never_takes_more_than_asked(self):
        t = self.transport([b"x" * 5000])
        self.assertEqual(len(t.get("https://huggingface.co/f", limit=1024)), 1024)
        self.assertEqual(self.requests[0].get_header("Range"), "bytes=0-1023")

    def test_the_token_is_optional_and_never_follows_a_redirect(self):
        t = self.transport([b"{}"])
        t.get("https://huggingface.co/api/models")
        self.assertFalse(self.requests[0].has_header("Authorization"))
        t = self.transport([b"{}"], token="hf_test")
        t.get("https://huggingface.co/api/models")
        req = self.requests[0]
        self.assertEqual(req.unredirected_hdrs.get("Authorization"), "Bearer hf_test")
        self.assertNotIn("Authorization", req.headers)
        self.assertIsNone(vp.HttpTransport(token="").token)


if __name__ == "__main__":
    unittest.main()
