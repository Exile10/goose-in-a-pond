#!/usr/bin/env python3
"""Generate crates/pond-core/data/vision-pairings.jsonl: the vision encoder each known model needs.

For every repository scripts/models/vision-sources.json names or finds, it reads the file list at the
current commit, picks the encoder (mmproj) for the model files, reads the first MiB of the encoder and
of one model file with Range requests, and keeps the pair only when the encoder's
clip.vision.projection_dim equals the model's embedding_length. Standard library only, Python 3.9+.

  python3 scripts/models/vision_pairings.py                 regenerate the table
  python3 scripts/models/vision_pairings.py --check         exit 1 if regenerating would change it
  python3 scripts/models/vision_pairings.py --fixtures DIR  replay DIR/responses.json, no network

A pin keeps its commit while the encoder's sha256 is unchanged, and a line keeps its checked_at while
its content is, so a run that finds nothing new changes nothing. HF_TOKEN is used when set.

Exit status: 0 done or up to date, 1 the table is stale (--check), 2 unusable input,
3 a request failed, in which case nothing was written.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime
import http.client
import json
import os
import re
import struct
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

HF = "https://huggingface.co"
HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(os.path.dirname(HERE))
DEFAULT_SOURCES = os.path.join(HERE, "vision-sources.json")
DEFAULT_OUT = os.path.join(REPO_ROOT, "crates", "pond-core", "data", "vision-pairings.jsonl")

HEAD_BYTES = 1 << 20
MAX_HEAD_BYTES = 8 << 20
TIMEOUT_S = 30
RETRIES = 5
JOBS = 6
USER_AGENT = "giap-vision-pairings/1"

EXIT_OK, EXIT_STALE, EXIT_INPUT, EXIT_FETCH = 0, 1, 2, 3

REPO_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*$")
HEX40 = re.compile(r"^[0-9a-f]{40}$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
SHARD = re.compile(r"-(\d+)-of-(\d+)\.gguf$")
COMPANION_TOKENS = {"mtp", "assistant", "draft", "drafter", "imatrix"}
PRECISION = {"BF16": 3, "F16": 2, "F32": 1}
LABEL_DROP = {"it", "qat", "instruct", "gguf", "ud"}
SIZE_TOKEN = re.compile(r"^[ea]?\d+(\.\d+)?[bmk]$", re.I)


class InputError(Exception):
    """The sources file, the fixtures or the committed table cannot be used."""


class FetchError(Exception):
    """A request failed for good, so the run writes nothing."""


class Unavailable(Exception):
    """Hugging Face answered 401, 403 or 404."""

    def __init__(self, url: str, status: int):
        super().__init__("%s: HTTP %d" % (url, status))
        self.status = status


def log(message: str) -> None:
    print(message, file=sys.stderr)


# ── transport ────────────────────────────────────────────────────────────────


def _backoff(attempt: int) -> float:
    return min(2.0 ** (attempt + 1), 60.0)


def _retry_after(headers, attempt: int) -> float:
    try:
        return max(0.0, min(float(headers.get("Retry-After")), 300.0))
    except (AttributeError, TypeError, ValueError):
        return _backoff(attempt)


def _read(resp, limit):
    if limit is None:
        return resp.read()
    chunks, got = [], 0
    while got < limit:
        chunk = resp.read(limit - got)
        if not chunk:
            break
        chunks.append(chunk)
        got += len(chunk)
    return b"".join(chunks)


class HttpTransport:
    """GET with a per-request timeout, backing off on 429 and 5xx, with HF_TOKEN when there is one."""

    def __init__(self, token=None, timeout=TIMEOUT_S, retries=RETRIES, sleep=time.sleep, opener=None):
        self.token = token or None
        self.timeout = timeout
        self.retries = retries
        self.sleep = sleep
        self.open = opener or urllib.request.build_opener().open

    def request(self, url: str, limit=None) -> urllib.request.Request:
        req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
        if limit is not None:
            req.add_header("Range", "bytes=0-%d" % (limit - 1))
        if self.token:
            # Unredirected: the token is for huggingface.co, never the CDN it redirects downloads to.
            req.add_unredirected_header("Authorization", "Bearer " + self.token)
        return req

    def get(self, url: str, limit=None) -> bytes:
        req = self.request(url, limit)
        attempt = 0
        while True:
            try:
                with self.open(req, timeout=self.timeout) as resp:
                    return _read(resp, limit)
            except urllib.error.HTTPError as e:
                if e.code in (401, 403, 404):
                    raise Unavailable(url, e.code) from None
                if (e.code != 429 and e.code < 500) or attempt >= self.retries:
                    raise FetchError("%s: HTTP %d" % (url, e.code)) from None
                delay = _retry_after(e.headers, attempt) if e.code == 429 else _backoff(attempt)
            except (urllib.error.URLError, http.client.HTTPException, OSError) as e:
                if attempt >= self.retries:
                    raise FetchError("%s: %s" % (url, e)) from None
                delay = _backoff(attempt)
            attempt += 1
            log("  waiting %.0f s before retrying %s" % (delay, url))
            self.sleep(delay)


class FixtureTransport:
    """Replays DIR/responses.json: URL -> {"file": name}, {"status": 4xx or 5xx} or {"error": text}."""

    def __init__(self, root: str):
        self.root = root
        try:
            with open(os.path.join(root, "responses.json"), encoding="utf-8") as f:
                self.responses = json.load(f)
        except (OSError, ValueError) as e:
            raise InputError("fixtures: %s" % e) from None
        self.requested = []

    def get(self, url: str, limit=None) -> bytes:
        self.requested.append(url)
        entry = self.responses.get(url)
        if entry is None:
            raise FetchError("%s: no recorded response" % url)
        if "error" in entry:
            raise FetchError("%s: %s" % (url, entry["error"]))
        status = entry.get("status", 200)
        if status in (401, 403, 404):
            raise Unavailable(url, status)
        if status >= 400:
            raise FetchError("%s: HTTP %d" % (url, status))
        with open(os.path.join(self.root, entry["file"]), "rb") as f:
            body = f.read()
        return body if limit is None else body[:limit]


# ── Hugging Face ─────────────────────────────────────────────────────────────


def revision_url(repo: str, rev: str) -> str:
    return "%s/api/models/%s/revision/%s?blobs=true" % (HF, repo, rev)


def search_url(term: str, author: str, limit: int) -> str:
    query = urllib.parse.urlencode([("search", term), ("author", author), ("filter", "gguf"),
                                    ("sort", "downloads"), ("direction", "-1"), ("limit", limit)])
    return "%s/api/models?%s" % (HF, query)


def resolve_url(repo: str, rev: str, path: str) -> str:
    return "%s/%s/resolve/%s/%s" % (HF, repo, rev, urllib.parse.quote(path))


def get_json(transport, url: str):
    body = transport.get(url)
    try:
        return json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        raise FetchError("%s: the answer is not JSON" % url) from None


def fetch_listing(transport, repo: str, rev: str = "main"):
    """(canonical repo id, commit, {path: (size, sha256 or None)}) at `rev`, in one request."""
    url = revision_url(repo, rev)
    data = get_json(transport, url)
    if (not isinstance(data, dict) or not HEX40.match(str(data.get("sha", "")))
            or not isinstance(data.get("siblings"), list)):
        raise FetchError("%s: not a revision with a file list" % url)
    files = {}
    for s in data["siblings"]:
        if not isinstance(s, dict) or not isinstance(s.get("rfilename"), str):
            continue
        lfs = s["lfs"] if isinstance(s.get("lfs"), dict) else {}
        size = lfs.get("size", s.get("size"))
        sha = lfs.get("sha256")
        files[s["rfilename"]] = (size if isinstance(size, int) else None,
                                 sha if isinstance(sha, str) else None)
    canonical = data.get("id")
    if not isinstance(canonical, str) or not REPO_ID.match(canonical):
        canonical = repo
    return canonical, data["sha"], files


# ── which files are what ─────────────────────────────────────────────────────


def is_encoder_file(path: str) -> bool:
    low = path.lower()
    return low.endswith(".gguf") and "mmproj" in low


def is_model_file(path: str) -> bool:
    """A chat model's .gguf: no encoder, drafter or imatrix, and only the first part of a split model."""
    low = path.lower()
    if not low.endswith(".gguf") or "mmproj" in low:
        return False
    if set(re.split(r"[^a-z0-9]+", low)) & COMPANION_TOKENS:
        return False
    shard = SHARD.search(low)
    return shard is None or int(shard.group(1)) == 1


def _looks_like_quant(s: str) -> bool:
    u = s.upper()
    return u.startswith(("Q", "IQ", "TQ", "MXFP")) or u in ("F16", "F32", "BF16")


def parse_quantization(path: str) -> str:
    """The quantisation tag in a file name, read as goose's parse_quantization reads it."""
    stem = path.rsplit("/", 1)[-1]
    if stem.lower().endswith(".gguf"):
        stem = stem[:-5]
    pos = stem.rfind("-of-")
    if pos >= 0 and "-" in stem[:pos]:
        stem = stem[:pos].rsplit("-", 1)[0]
    for sep in ("-", "."):
        if sep in stem:
            tail = stem.rsplit(sep, 1)[1]
            if _looks_like_quant(tail):
                return tail
    return "unknown"


def _parent(path: str):
    return [p for p in path.split("/")[:-1] if p]


def choose_encoder(model_path: str, encoders):
    """goose's select_best_mmproj without its quant-proximity step, since one encoder serves every
    quant here: same or nearest parent directory, then BF16, F16, F32, then the first name."""
    model_dir = _parent(model_path)
    best_key, best = None, None
    for enc in encoders:
        enc_dir = _parent(enc)
        if model_dir[:len(enc_dir)] != enc_dir:
            continue
        key = (len(enc_dir), PRECISION.get(parse_quantization(enc).upper(), 0))
        if best is None or key > best_key or (key == best_key and enc < best):
            best_key, best = key, enc
    return best


def group_files(files):
    """(encoders, {encoder: [model files]}, model files that no encoder applies to)."""
    encoders = sorted(p for p in files if is_encoder_file(p))
    groups, orphans = {}, []
    for model in sorted(p for p in files if is_model_file(p)):
        enc = choose_encoder(model, encoders)
        if enc is None:
            orphans.append(model)
        else:
            groups.setdefault(enc, []).append(model)
    return encoders, groups, orphans


# ── GGUF headers ─────────────────────────────────────────────────────────────

GGUF_U32, GGUF_BOOL, GGUF_STRING, GGUF_ARRAY = 4, 7, 8, 9
GGUF_SCALARS = {0: "<B", 1: "<b", 2: "<H", 3: "<h", 4: "<I", 5: "<i", 6: "<f", 7: "<?",
                10: "<Q", 11: "<q", 12: "<d"}
MAX_KVS = 4096
MAX_TENSORS = 1 << 16
MAX_DIMS = 4
MAX_NAME_BYTES = 4 << 20
U64_MAX = (1 << 64) - 1
# (elements, bytes) per block for the ggml types pond-core's gguf.rs can measure; it refuses others.
GGML_TYPES = {0: (1, 4), 1: (1, 2), 2: (32, 18), 3: (32, 20), 6: (32, 22), 7: (32, 24), 8: (32, 34),
              10: (256, 84), 11: (256, 110), 12: (256, 144), 13: (256, 176), 14: (256, 210),
              24: (1, 1), 25: (1, 2), 26: (1, 4), 27: (1, 8), 28: (1, 8), 30: (1, 2)}


class _Short(Exception):
    pass


class _Bad(Exception):
    pass


class _Reader:
    def __init__(self, buf: bytes):
        self.buf = buf
        self.pos = 0

    def skip(self, n: int) -> None:
        if self.pos + n > len(self.buf):
            raise _Short()
        self.pos += n

    def take(self, n: int) -> bytes:
        start = self.pos
        self.skip(n)
        return self.buf[start:self.pos]

    def scalar(self, fmt: str):
        return struct.unpack(fmt, self.take(struct.calcsize(fmt)))[0]

    def string(self) -> str:
        return self.take(self.scalar("<Q")).decode("utf-8", "replace")


class GgufHead:
    """What a GGUF file's first bytes say. `short` means they ran out before the walk finished."""

    def __init__(self):
        self.version = None
        self.tensor_count = 0
        self.kv = {}
        self.kv_complete = False
        self.alignment = 32
        self.data_end = None
        self.short = False
        self.problem = None
        self.bytes_read = 0

    def _get(self, key: str, kind: int):
        entry = self.kv.get(key)
        return entry[1] if entry is not None and entry[0] == kind else None

    def string(self, key: str):
        return self._get(key, GGUF_STRING)

    def u32(self, key: str):
        return self._get(key, GGUF_U32)

    def flag(self, key: str):
        return self._get(key, GGUF_BOOL)

    def embedding_length(self):
        """The last u32 key ending in .embedding_length, which is the one pond-core's reader keeps."""
        width = None
        for key, (kind, value) in self.kv.items():
            if kind == GGUF_U32 and key.endswith(".embedding_length"):
                width = value
        return width

    def projector(self):
        return self.string("clip.vision.projector_type") or self.string("clip.projector_type")


def _value(r: _Reader, kind: int):
    if kind == GGUF_STRING:
        return r.string()
    if kind == GGUF_ARRAY:
        elem = r.scalar("<I")
        count = r.scalar("<Q")
        if elem == GGUF_STRING:
            for _ in range(count):
                r.skip(r.scalar("<Q"))
        elif elem in GGUF_SCALARS:
            r.skip(struct.calcsize(GGUF_SCALARS[elem]) * count)
        else:
            raise _Bad("an array of value type %d" % elem)
        return None
    if kind not in GGUF_SCALARS:
        raise _Bad("value type %d" % kind)
    return r.scalar(GGUF_SCALARS[kind])


def _data_end(r: _Reader, count: int, alignment: int):
    """(end of the tensor data the table describes, None) as gguf.rs's tensor_data_end, or (None, why)."""
    if alignment <= 0 or alignment & (alignment - 1):
        return None, "general.alignment %d is not a power of two" % alignment
    end = 0
    for _ in range(count):
        name_len = r.scalar("<Q")
        if name_len > MAX_NAME_BYTES:
            return None, "a tensor name longer than 4 MiB"
        r.skip(name_len)
        dims = r.scalar("<I")
        if not 1 <= dims <= MAX_DIMS:
            return None, "a tensor with %d dimensions" % dims
        elements = 1
        for _ in range(dims):
            elements *= r.scalar("<Q")
        kind = r.scalar("<I")
        if kind not in GGML_TYPES:
            return None, "tensor type %d, which pond-core cannot measure" % kind
        offset = r.scalar("<Q")
        block, block_bytes = GGML_TYPES[kind]
        if elements > U64_MAX or elements % block:
            return None, "a tensor that does not divide into whole blocks"
        nbytes = elements // block * block_bytes
        if offset + nbytes > U64_MAX:
            return None, "a tensor past the end of a 64-bit file"
        end = max(end, offset + nbytes)
    return ((r.pos + alignment - 1) & ~(alignment - 1)) + end, None


def parse_gguf(buf: bytes) -> GgufHead:
    """Walk a GGUF v2/v3 header as far as `buf` goes; never raises on short or hostile bytes."""
    head = GgufHead()
    head.bytes_read = len(buf)
    r = _Reader(buf)
    try:
        if r.take(4) != b"GGUF":
            head.problem = "not a GGUF file"
            return head
        head.version = r.scalar("<I")
        if head.version not in (2, 3):
            head.problem = "GGUF version %d" % head.version
            return head
        head.tensor_count = r.scalar("<Q")
        kv_count = r.scalar("<Q")
        for _ in range(min(kv_count, MAX_KVS)):
            key = r.string()
            kind = r.scalar("<I")
            head.kv[key] = (kind, _value(r, kind))
        if kv_count > MAX_KVS:
            head.problem = "more than %d keys" % MAX_KVS
            return head
        head.kv_complete = True
        alignment = head.u32("general.alignment")
        if alignment is not None:
            head.alignment = alignment
        if head.tensor_count > MAX_TENSORS:
            head.problem = "more than %d tensors" % MAX_TENSORS
            return head
        head.data_end, head.problem = _data_end(r, head.tensor_count, head.alignment)
    except _Short:
        head.short = True
    except _Bad as e:
        head.problem = str(e)
    return head


def read_head(transport, repo: str, rev: str, path: str, done) -> GgufHead:
    """A file's GGUF header from a 1 MiB Range read, widened once to 8 MiB while `done(head)` is false."""
    url = resolve_url(repo, rev, path)
    size = HEAD_BYTES
    while True:
        buf = transport.get(url, limit=size)
        head = parse_gguf(buf)
        if done(head) or not head.short or len(buf) < size or size >= MAX_HEAD_BYTES:
            return head
        size = MAX_HEAD_BYTES


def encoder_problem(head: GgufHead, size):
    """Why pond-core's validate_encoder_header would refuse this encoder, or None."""
    if head.string("general.architecture") != "clip":
        return head.problem or "general.architecture is not clip"
    if head.flag("clip.has_vision_encoder") is not True:
        return "no vision tower (clip.has_vision_encoder)"
    if not head.projector() or not head.u32("clip.vision.projection_dim"):
        return "no vision projector type or clip.vision.projection_dim"
    if head.data_end is None:
        if head.short:
            return "its tensor table runs past the %d bytes read" % head.bytes_read
        return head.problem or "its tensor table cannot be read"
    if not isinstance(size, int):
        return "the file list gives no size"
    if head.data_end > size:
        return "its header describes %d bytes and the file has %d" % (head.data_end, size)
    padded = -(-head.data_end // head.alignment) * head.alignment
    if size > padded:
        return "the file is %d bytes longer than its header describes" % (size - head.data_end)
    return None


def model_problem(head: GgufHead):
    arch = head.string("general.architecture")
    if not arch:
        return head.problem or "no general.architecture in its header"
    if arch == "clip" or arch.endswith("-assistant"):
        return "general.architecture %s is a companion, not a chat model" % arch
    if not head.embedding_length():
        return "no embedding_length in its header"
    return None


# ── discovery and naming ─────────────────────────────────────────────────────


def author_of(repo: str) -> str:
    return repo.split("/", 1)[0]


def split_vendor(name: str):
    """("google", "gemma-3-4b-it") for a re-upload named google_gemma-3-4b-it, else (None, name)."""
    org, sep, rest = name.partition("_")
    if sep and org and rest and "-" not in org:
        return org, rest
    return None, name


def wanted(repo: str, term: str, upstream_orgs) -> bool:
    org, rest = split_vendor(repo.split("/", 1)[1])
    if org is not None and org.lower() not in upstream_orgs:
        return False
    return rest.lower().startswith(term.lower())


def bare_dir(repo: str) -> str:
    return re.sub(r"(?i)-gguf$", "", repo.split("/", 1)[1]).lower()


def derive_label(repo: str) -> str:
    """"Gemma 4 E4B" for unsloth/gemma-4-E4B-it-qat-GGUF: family and size, without it/qat/Instruct."""
    _, name = split_vendor(re.sub(r"(?i)-gguf$", "", repo.split("/", 1)[1]))
    words = []
    for token in name.split("-"):
        if not token or token.lower() in LABEL_DROP:
            continue
        if SIZE_TOKEN.match(token):
            words.append(token.upper())
        elif token.isalpha() and token.islower():
            words.append(token.capitalize())
        else:
            words.append(token)
    return " ".join(words) or name


def load_sources(path: str):
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError) as e:
        raise InputError("%s: %s" % (path, e)) from None
    if not isinstance(data, dict):
        raise InputError("%s: not a JSON object" % path)

    def strings(key):
        value = data.get(key)
        if not isinstance(value, list) or not all(isinstance(v, str) and v for v in value):
            raise InputError("%s: %s must be a list of strings" % (path, key))
        return value

    explicit = strings("explicit")
    bad = [r for r in explicit if not REPO_ID.match(r)]
    if bad:
        raise InputError("%s: not repository ids: %s" % (path, ", ".join(bad)))
    families = data.get("families")
    if not isinstance(families, list) or not all(
            isinstance(f, dict) and isinstance(f.get("search"), str) and f["search"] for f in families):
        raise InputError("%s: families must be a list of {\"name\", \"search\"}" % path)
    limit = data.get("search_limit")
    if not isinstance(limit, int) or isinstance(limit, bool) or not 1 <= limit <= 100:
        raise InputError("%s: search_limit must be a whole number from 1 to 100" % path)
    return {
        "explicit": explicit,
        "families": [f["search"] for f in families],
        "authors": strings("authors"),
        "upstream_orgs": [o.lower() for o in strings("upstream_orgs")],
        "search_limit": limit,
    }


def load_existing(path: str):
    """The committed table's lines; [] when there is none yet."""
    if not os.path.exists(path):
        return []
    lines = []
    try:
        with open(path, encoding="utf-8") as f:
            for n, raw in enumerate(f, 1):
                if not raw.strip():
                    continue
                try:
                    line = json.loads(raw)
                except ValueError as e:
                    raise InputError("%s:%d is not JSON: %s" % (path, n, e)) from None
                if (not isinstance(line, dict) or not isinstance(line.get("model_repo"), str)
                        or not isinstance(line.get("encoder"), dict)):
                    raise InputError("%s:%d has no model_repo and encoder" % (path, n))
                lines.append(line)
    except OSError as e:
        raise InputError("%s: %s" % (path, e)) from None
    return lines


def discover(transport, sources, existing):
    """[(repo, kind)]: the explicit list, the table's own lines, then search results, once each."""
    queue, seen = [], set()

    def add(repo, kind):
        if repo.lower() not in seen:
            seen.add(repo.lower())
            queue.append((repo, kind))

    authors = {a.lower() for a in sources["authors"]}
    for repo in sources["explicit"]:
        add(repo, "explicit")
    for repo in sorted(line["model_repo"] for line in existing):
        if REPO_ID.match(repo) and author_of(repo).lower() in authors:
            add(repo, "carried")
    found = set()
    for term in sources["families"]:
        for author in sources["authors"]:
            url = search_url(term, author, sources["search_limit"])
            try:
                results = get_json(transport, url)
            except Unavailable as e:
                raise FetchError(str(e)) from None
            if not isinstance(results, list):
                raise FetchError("%s: not a list of models" % url)
            for item in results:
                repo = item.get("id") if isinstance(item, dict) else None
                if (isinstance(repo, str) and REPO_ID.match(repo)
                        and author_of(repo).lower() == author.lower()
                        and wanted(repo, term, sources["upstream_orgs"])):
                    found.add(repo)
    for repo in sorted(found):
        add(repo, "found")
    return queue


# ── one repository ───────────────────────────────────────────────────────────


class Pairing:
    """A checked model/encoder pair, before it is pinned, named and dated."""

    def __init__(self, repo, commit, model_files, architecture, width, encoder, size, sha256,
                 projector, dim):
        self.repo = repo
        self.commit = commit
        self.model_files = model_files
        self.architecture = architecture
        self.width = width
        self.encoder = encoder
        self.size = size
        self.sha256 = sha256
        self.projector = projector
        self.dim = dim
        self.revision = commit


def scan(transport, repo: str, commit: str, files):
    """([Pairing], [reasons something here does not pair])."""
    encoders, groups, orphans = group_files(files)
    if not groups and not orphans:
        return [], ["no model files"]
    if not encoders:
        return [], ["text only: no mmproj file"]
    found, problems = [], []
    if orphans:
        problems.append("%d model file(s) have no mmproj in their directory or above" % len(orphans))
    for enc in sorted(groups):
        models = groups[enc]
        size, sha = files[enc]
        if not sha or not HEX64.match(sha):
            problems.append("%s: the file list gives no LFS sha256" % enc)
            continue
        ehead = read_head(transport, repo, commit, enc, lambda h: h.data_end is not None)
        why = encoder_problem(ehead, size)
        if why:
            problems.append("%s would be refused: %s" % (enc, why))
            continue
        mhead = read_head(transport, repo, commit, models[0],
                          lambda h: bool(h.string("general.architecture") and h.embedding_length()))
        why = model_problem(mhead)
        if why:
            problems.append("%s: %s" % (models[0], why))
            continue
        dim, width = ehead.u32("clip.vision.projection_dim"), mhead.embedding_length()
        if dim != width:
            problems.append("%s does not fit: projection_dim %d, embedding_length %d"
                            % (enc, dim, width))
            continue
        found.append(Pairing(repo, commit, models, mhead.string("general.architecture"), width,
                             enc, size, sha, ehead.projector(), dim))
    return found, problems


# ── pins, directories and dates ──────────────────────────────────────────────


def _encoder_of(line):
    return line.get("encoder") if isinstance(line.get("encoder"), dict) else {}


def _line_key(line):
    return line["model_repo"].lower(), _encoder_of(line).get("filename")


def _without_date(line):
    return {k: v for k, v in line.items() if k != "checked_at"}


def _usable_dir(value) -> bool:
    return isinstance(value, str) and bool(value) and "/" not in value and not value.startswith(".")


def pin_revision(transport, p: Pairing, old) -> str:
    """The old pin while it still serves these exact bytes, else the commit just read."""
    enc = _encoder_of(old) if old else {}
    rev = enc.get("revision")
    if enc.get("sha256") != p.sha256 or not isinstance(rev, str) or not HEX40.match(rev):
        return p.commit
    if rev == p.commit:
        return rev
    try:
        _, _, files = fetch_listing(transport, p.repo, rev)
    except Unavailable:
        log("  pin moved: %s %s, revision %s is gone" % (p.repo, p.encoder, rev[:12]))
        return p.commit
    if files.get(p.encoder) != (p.size, p.sha256):
        log("  pin moved: %s %s, revision %s holds other bytes" % (p.repo, p.encoder, rev[:12]))
        return p.commit
    return rev


def assign_dirs(pairings, existing, explicit_order, author_rank):
    """{id(p): dir}. A dir already in the table stays; explicit repositories come next, then the rest.
    Each dir names one encoder; a taken name gets the author in front."""
    by_file, by_repo = {}, {}
    for line in existing:
        d = _encoder_of(line).get("dir")
        if _usable_dir(d):
            by_file.setdefault(_line_key(line), d)
            by_repo.setdefault(line["model_repo"].lower(), d)

    def priority(p):
        repo = p.repo.lower()
        if (repo, p.encoder) in by_file:
            return (0, repo, p.encoder)
        if repo in by_repo:
            return (1, repo, p.encoder)
        if repo in explicit_order:
            return (2, explicit_order[repo], p.encoder)
        return (3, author_rank.get(author_of(repo), len(author_rank)), repo, p.encoder)

    dirs, taken = {}, set()
    for p in sorted(pairings, key=priority):
        repo = p.repo.lower()
        bare = bare_dir(p.repo)
        options = [by_file.get((repo, p.encoder)) or by_repo.get(repo) or bare,
                   "%s-%s" % (author_of(repo), bare)]
        n = 2
        while not any(o not in taken for o in options):
            options = ["%s-%s-%d" % (author_of(repo), bare, n)]
            n += 1
        choice = next(o for o in options if o not in taken)
        taken.add(choice)
        dirs[id(p)] = choice
    return dirs


def make_line(p: Pairing, label: str, enc_dir: str, revision: str, checked_at):
    return {
        "model_repo": p.repo,
        "model_files": sorted(p.model_files),
        "architecture": p.architecture,
        "embedding_length": p.width,
        "label": label,
        "encoder": {
            "dir": enc_dir,
            "repo": p.repo,
            "revision": revision,
            "filename": p.encoder,
            "size_bytes": p.size,
            "sha256": p.sha256,
            "projector": p.projector,
            "projection_dim": p.dim,
        },
        "checked_at": checked_at,
    }


def build_lines(pairings, existing, sources, today: str):
    old_by_key = {_line_key(line): line for line in existing}
    explicit_order = {r.lower(): i for i, r in enumerate(sources["explicit"])}
    author_rank = {a.lower(): i for i, a in enumerate(sources["authors"])}
    dirs = assign_dirs(pairings, existing, explicit_order, author_rank)
    lines = []
    for p in pairings:
        old = old_by_key.get((p.repo.lower(), p.encoder))
        line = make_line(p, derive_label(p.repo), dirs[id(p)], p.revision, None)
        kept = old is not None and _without_date(old) == _without_date(line)
        if kept and isinstance(old.get("checked_at"), str) and DATE.match(old["checked_at"]):
            line["checked_at"] = old["checked_at"]
        else:
            line["checked_at"] = today
        lines.append(line)
    return sorted(lines, key=lambda line: (line["model_repo"], line["encoder"]["filename"]))


def render(lines) -> str:
    return "".join(json.dumps(line, separators=(",", ":")) + "\n" for line in lines)


# ── the run ──────────────────────────────────────────────────────────────────


def _scan_repo(transport, repo: str, old_by_key):
    canonical, commit, files = fetch_listing(transport, repo)
    found, problems = scan(transport, canonical, commit, files)
    for p in found:
        p.revision = pin_revision(transport, p, old_by_key.get((p.repo.lower(), p.encoder)))
    return canonical, found, problems


def generate(transport, sources, existing, today: str, jobs: int = 1):
    """(lines, {repo id: [reasons]}, {scanned repo ids, lower case}). Raises FetchError on a failed request."""
    explicit = {r.lower() for r in sources["explicit"]}
    queue = discover(transport, sources, existing)
    kinds = {}
    for _, kind in queue:
        kinds[kind] = kinds.get(kind, 0) + 1
    log("scanning %d repositories: %d listed, %d already in the table, %d found by search"
        % (len(queue), kinds.get("explicit", 0), kinds.get("carried", 0), kinds.get("found", 0)))
    old_by_key = {_line_key(line): line for line in existing}
    pairings, skipped, seen = [], {}, set()
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        futures = [pool.submit(_scan_repo, transport, repo, old_by_key) for repo, _ in queue]
        try:
            for (repo, _), future in zip(queue, futures):
                try:
                    canonical, found, problems = future.result()
                except Unavailable as e:
                    if repo.lower() in explicit:
                        raise FetchError("%s is in the explicit list but answered HTTP %d"
                                         % (repo, e.status)) from None
                    canonical, found, problems = repo, [], ["unavailable (HTTP %d)" % e.status]
                if canonical.lower() in seen:
                    continue
                seen.add(canonical.lower())
                pairings.extend(found)
                for reason in problems:
                    log("  skip %s: %s" % (canonical, reason))
                if problems:
                    skipped[canonical] = problems
                for p in found:
                    log("  pair %s: %s (%s %d)" % (p.repo, p.encoder, p.architecture, p.width))
        except BaseException:
            for future in futures:
                future.cancel()
            raise
    return build_lines(pairings, existing, sources, today), skipped, seen


def summarize(old_lines, new_lines, skipped, scanned=(), form_differs=False) -> str:
    old = {_line_key(line): line for line in old_lines}
    new = {_line_key(line): line for line in new_lines}
    reasons = {repo.lower(): problems for repo, problems in skipped.items()}
    out = []
    for key in sorted(new.keys() - old.keys()):
        line = new[key]
        out.append("+ %s %s (%s: %s %d, %s)" % (line["model_repo"], key[1], line["label"],
                                               line["architecture"], line["embedding_length"],
                                               line["encoder"]["projector"]))
    changed = 0
    for key in sorted(new.keys() & old.keys()):
        a, b = old[key], new[key]
        if _without_date(a) == _without_date(b):
            continue
        changed += 1
        fields = []
        for field in sorted(set(a) | set(b)):
            if field in ("checked_at", "encoder") or a.get(field) == b.get(field):
                continue
            fields.append(field)
        ea, eb = _encoder_of(a), _encoder_of(b)
        fields += ["encoder." + f for f in sorted(set(ea) | set(eb)) if ea.get(f) != eb.get(f)]
        out.append("~ %s %s: %s" % (b["model_repo"], key[1], ", ".join(fields)))
    paired = {key[0] for key in new}
    for key in sorted(old.keys() - new.keys()):
        if key[0] in reasons:
            why = "; ".join(reasons[key[0]])
        elif key[0] in paired:
            why = "this repository pairs with another encoder now"
        elif key[0] in scanned:
            why = "nothing in this repository pairs now"
        else:
            why = "no longer scanned"
        out.append("- %s %s (%s)" % (old[key]["model_repo"], key[1], why))
    added, removed = len(new.keys() - old.keys()), len(old.keys() - new.keys())
    if not (added or changed or removed):
        if form_differs:
            out.append("vision-pairings.jsonl: same pairings, not in the generator's form "
                       "(order or formatting), %d pairings" % len(new))
        else:
            out.append("vision-pairings.jsonl: up to date, %d pairings" % len(new))
    else:
        out.append("vision-pairings.jsonl: %d added, %d changed, %d removed, %d unchanged"
                   % (added, changed, removed, len(new) - added - changed))
    return "\n".join(out)


def write_atomic(path: str, text: str) -> None:
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    os.replace(tmp, path)


def parse_args(argv):
    ap = argparse.ArgumentParser(
        prog="vision_pairings.py",
        description="Regenerate the vision pairing table from Hugging Face.",
        epilog="Exit status: 0 done or up to date, 1 stale (--check), 2 unusable input, "
               "3 a request failed and nothing was written.")
    ap.add_argument("--check", action="store_true",
                    help="write nothing; exit 1 if regenerating would change the table")
    ap.add_argument("--out", default=DEFAULT_OUT, metavar="PATH",
                    help="the table (default: crates/pond-core/data/vision-pairings.jsonl)")
    ap.add_argument("--sources", default=DEFAULT_SOURCES, metavar="PATH",
                    help="what to scan (default: scripts/models/vision-sources.json)")
    ap.add_argument("--fixtures", metavar="DIR",
                    help="replay the responses recorded in DIR/responses.json instead of the network")
    ap.add_argument("--today", metavar="YYYY-MM-DD",
                    help="the checked_at given to changed lines (default: today, UTC)")
    ap.add_argument("--jobs", type=int, default=JOBS, metavar="N",
                    help="repositories read at once, 1 to 16 (default: %(default)s)")
    return ap.parse_args(argv)


def main(argv=None) -> int:
    if sys.version_info < (3, 9):
        log("vision_pairings.py needs Python 3.9 or newer")
        return EXIT_INPUT
    args = parse_args(argv)
    today = args.today or datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    if not DATE.match(today):
        log("--today must be YYYY-MM-DD")
        return EXIT_INPUT
    if not 1 <= args.jobs <= 16:
        log("--jobs must be from 1 to 16")
        return EXIT_INPUT
    try:
        sources = load_sources(args.sources)
        existing = load_existing(args.out)
        transport = (FixtureTransport(args.fixtures) if args.fixtures
                     else HttpTransport(token=os.environ.get("HF_TOKEN")))
    except InputError as e:
        log(str(e))
        return EXIT_INPUT
    try:
        lines, skipped, scanned = generate(transport, sources, existing, today, args.jobs)
    except FetchError as e:
        log("a request failed, so nothing was written: %s" % e)
        return EXIT_FETCH
    log("%d repositories scanned: %d pairings, %d with something that does not pair"
        % (len(scanned), len(lines), len(skipped)))
    text = render(lines)
    try:
        with open(args.out, encoding="utf-8") as f:
            current = f.read()
    except FileNotFoundError:
        current = ""
    print(summarize(existing, lines, skipped, scanned, form_differs=text != current))
    if args.check:
        return EXIT_OK if text == current else EXIT_STALE
    if text != current:
        write_atomic(args.out, text)
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
