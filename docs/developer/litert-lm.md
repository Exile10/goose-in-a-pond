# The LiteRT-LM C API library

goose's `litert` backend runs `.litertlm` models through Google's
[LiteRT-LM](https://github.com/google-ai-edge/LiteRT-LM) C API, which it opens at runtime by
absolute path (`RTLD_LOCAL`). GIAP builds that library itself, per platform, and ships it with the
desktop app and to the Jetson. Nothing is downloaded at runtime.

Everything here is behind `scripts/giap.sh`; the logic is `scripts/lib/litert-setup.sh` and its
tests are `scripts/lib/litert-setup.test.sh`.

## What is shipped

| File | What it is |
|---|---|
| `liblitert-lm.{dylib,so}` | The C API (`//c:litert-lm`), built from the pinned commit |
| `libLiteRt`, `libGemmaModelConstraintProvider` | Linked by the C API; the constraint provider is needed even on CPU |
| `libLiteRtWebGpuAccelerator`, `libwebgpu_dawn`, `libLiteRtTopKWebGpuSampler` | The GPU path: WebGPU/Dawn over Metal on a Mac, over Vulkan on the Orin |
| `libLiteRtMetalAccelerator`, `libLiteRtTopKMetalSampler` | macOS only |
| `include/*.h` | The public C headers (not shipped in the app) |
| `LICENSE` | LiteRT-LM's Apache-2.0 licence |
| `MANIFEST.sha256` | Line 1 describes the build; then `sha256  ./path` for every other file |

Everything except `liblitert-lm` is LiteRT's own prebuilt binary from `prebuilt/<platform>/` at the
same commit. The library statically contains its own Rust std, allocator, abseil and protobuf, which
is why it stays behind `RTLD_LOCAL`. Their third-party notices are not collected into the package yet.

## The pin

`LITERT_COMMIT` and `LITERT_CAPI_VERSION` in `scripts/lib/litert-setup.sh`: LiteRT-LM
`3dbb23e1e31f085a2222515282419ed470af590f`, whose `version.bzl` says `VERSION = "0.18.0"` and
`C_API_VERSION = "1.0.0"`. C API 1.0.0 is on `main`, not yet in a release, so a `main` commit is
pinned rather than a tag.

To move it:

1. Pick the commit and read its `version.bzl`. A different `C_API_VERSION` means the goose binding
   has to be checked against `c/*.h` before anything else.
2. Set `LITERT_COMMIT` (and `LITERT_CAPI_VERSION`), then `bash scripts/lib/litert-setup.test.sh`.
3. Build both platforms. Packages live under `<capi>-<commit8>/`, so the old one stays until you
   delete it.
4. If the prebuilt set changed, `LITERT_REQUIRED_LIBS` and `mac.binaries` in
   `pond-desktop/electron-builder.yml` follow it; `scripts/verify-sidecar.sh` names a dylib that is
   staged but not listed.

## Building

```bash
bash scripts/giap.sh litert build macos-arm64      # on an Apple Silicon Mac
bash scripts/giap.sh litert build linux-arm64      # anywhere Docker runs linux/arm64; for the Jetson
bash scripts/giap.sh litert status                 # what is recorded, each verified; exit 1 on a failure
bash scripts/giap.sh --dry-run litert build linux-arm64   # every command, nothing run
```

The source is a clone this tool keeps at `~/.giap/litert-lm/src`, moved to the pin and given the
Git LFS objects of that one platform. `LITERT_LM_SRC=<dir>` uses a clone of your own instead, which
must already be at the pin; it is never moved.

- **macos-arm64** needs bazelisk, git-lfs and the Xcode command line tools, and runs
  `bazelisk build -c opt --config=macos_arm64 --define=litert_runtime_link_mode=dynamic //c:litert-lm`
  natively: about 22 minutes cold on an M4.
- **linux-arm64** runs the same target with `--config=linux_arm64` in `ubuntu:22.04` (glibc 2.35,
  the same as JetPack 6; not `rust:bookworm`, whose glibc is newer than the device's), with clang 15
  and a bazelisk checked against its published SHA-256. The source is mounted read-only, and Bazel's
  output root persists in the Docker volume `giap-litert-lm-linux-arm64`
  (`docker volume rm giap-litert-lm-linux-arm64` reclaims it). The container packages and verifies
  the result with the same script, including a `dlopen` of the library on that glibc.
  LiteRT-LM is never built on the Jetson itself.
- `LITERT_BAZEL_JOBS=N` caps Bazel's jobs, for a Docker VM that runs out of memory.

A build is staged beside its destination, verified, and only then moved into place and recorded, so
a failed build leaves the previous package as it was.

## Package layout and verification

```
~/.giap/litert-lm/
  .path-macos-arm64    .path-linux-arm64      the package each platform last built and verified
  1.0.0-3dbb23e1/macos-arm64/                 liblitert-lm.dylib, lib*.dylib, include/, LICENSE, MANIFEST.sha256
  1.0.0-3dbb23e1/linux-arm64/                 liblitert-lm.so, lib*.so, include/, LICENSE, MANIFEST.sha256
```

Packaging rewrites how the libraries find each other. On macOS: `liblitert-lm.dylib` is renamed
`@rpath/liblitert-lm.dylib`, Bazel's sandbox rpaths are removed, every dylib searches
`@loader_path`, and every dylib is signed again ad hoc, because Apple Silicon refuses a modified,
unsigned Mach-O. On Linux: every `.so` gets a RUNPATH of exactly `$ORIGIN`, replacing the
Google-internal paths the prebuilts carry. Neither needs `DYLD_LIBRARY_PATH` or `LD_LIBRARY_PATH`.

Verification, which `litert status`, `giap.sh doctor` and the stage, verify and deploy scripts all
run, checks every file against the manifest and fails on any file the manifest does not list or any
symlink. Only then does it inspect the libraries (Mach-O: arm64, dependencies inside the directory or
part of macOS, rpaths, signature; ELF: aarch64, RUNPATH, dependencies) and load one where the host
can. A package that fails its hashes is never loaded.

## Where it goes

| Target | Location | How |
|---|---|---|
| Desktop app | `Contents/Resources/litert-lm/`, beside `pond-server` | `npm run stage:server` copies the recorded macos-arm64 dylibs and LICENSE to `pond-desktop/resources/litert-lm/`; `scripts/verify-sidecar.sh` checks and loads them; electron-builder ships them |
| Jetson | `~/.local/share/goose-in-a-pond/lib/litert-lm/1.0.0-3dbb23e1/` | `bash scripts/jetson.sh deploy` copies the recorded linux-arm64 package and runs `sha256sum -c` and `ldd` there; `JETSON_DATA_DIR` if the pond's data dir is elsewhere |
| A pond run from `target/` | the recorded package directory | found through `~/.giap/litert-lm/.path-<platform>` |

At startup pond-server sets `GOOSE_LITERT_LIB_DIR`, unless it is already set, to the newest package
under `<data dir>/lib/litert-lm/` and otherwise to the recorded one
(`crates/pond-server/src/litert_runtime.rs`). Left unset, the backend looks in `litert-lm/` beside
the executable, which is where the app bundle carries it.

With nothing recorded, staging and deploy warn and carry on: the app or the device then runs without
the litert backend. A recorded package that fails verification stops them.

## When Bazel stalls on downloads

From some networks `storage.googleapis.com` resolves to front ends that serve Bazel at 5-30 KB/s and
never time out, and the TensorFlow-family dependencies list a Google Storage mirror first, so a build
can sit for hours on one archive. Fetch the archive yourself from a front end that is fast from where
you are (`curl -w '%{speed_download}'` to compare; `--resolve storage.googleapis.com:443:<ip>` to
pick one), check its SHA-256 against the `.bzl` that names it, put it in a directory, and pass that:

```bash
bash scripts/giap.sh litert build linux-arm64 --distdir ~/litert-distdir    # or LITERT_DISTDIR=...
```

Bazel takes an archive from there when its file name and checksum match. In Docker the directory is
mounted read-only at `/distdir`.

## Google's own prebuilts

LiteRT-LM's Python wheel already packages `prebuilt/<platform>/liblitert-lm.*` when the repository
carries it. Once a release ships C API 1.0.0 that way, the Bazel step can become a copy of that file:
the backend binds to the C API, not to this build. The packaging, manifest and verification stay as
they are, and Google's library needs the same rpath fixes as the other prebuilts.
