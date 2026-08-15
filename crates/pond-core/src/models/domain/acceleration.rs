//! Whether this binary can use the accelerator the host actually has.
//!
//! # The failure this exists to make impossible
//!
//! On a Jetson, CUDA is not a build convenience — it is the difference between
//! a usable pond and an unusable one. Measured on an Orin Nano with the same
//! model and the same prompt: **696 tokens/second of prefill with CUDA, 26
//! without**. A 1,200-token preamble is 1.7 seconds one way and 46 seconds the
//! other.
//!
//! And nothing tells you which one you have. CUDA reaches llama.cpp through a
//! five-link feature chain —
//! `pond-adapters-local-inference/cuda` -> `goose/cuda` -> `goose-providers/cuda`
//! -> `goose-local-inference/cuda` -> `llama-cpp-2/cuda` — that is passed on the
//! COMMAND LINE by one deploy script. Build the same source any other way and
//! every link silently evaluates to "off": the binary compiles, starts, loads
//! the model, answers correctly, and is roughly thirty times slower. There is no
//! error, no missing symbol, and no line in any log.
//!
//! That is not hypothetical. `scripts/jetson/build-docker.sh` builds with
//! `--features local-inference` and no `cuda`, so a container built from it is a
//! CPU binary that looks exactly like the right one.
//!
//! # Why a warning and not a refusal
//!
//! A CPU build on a Jetson is wrong, but it is not unsafe, and refusing to start
//! would turn a slow pond into no pond — including on a host where somebody is
//! deliberately running without the accelerator to test something. The
//! obligation this module discharges is that the situation is **stated**, in a
//! form nobody has to already suspect in order to notice.

/// What this binary can do with this host's accelerator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acceleration {
    /// Built with CUDA. Nothing to say.
    CudaBuild,
    /// A host with an accelerator, and a binary that cannot reach it. The one
    /// case worth shouting about.
    CpuOnAcceleratedHost,
    /// No accelerator expected here — a Mac, a laptop, a CI runner.
    CpuElsewhere,
}

/// Decide what this pair of facts means.
///
/// Deliberately takes both as plain `bool` rather than reading the host or a
/// `cfg!` itself: the host probe and the build flag live in two different crates
/// (one is `pond-server`, the other is the adapter that owns the feature), and a
/// policy that reached for either could not be tested at all.
pub fn classify(host_is_accelerated: bool, cuda_build: bool) -> Acceleration {
    match (host_is_accelerated, cuda_build) {
        (_, true) => Acceleration::CudaBuild,
        (true, false) => Acceleration::CpuOnAcceleratedHost,
        (false, false) => Acceleration::CpuElsewhere,
    }
}

/// What to tell the operator, or `None` when there is nothing wrong.
///
/// Carries the rebuild command, because a warning that says only "this is slow"
/// leaves the reader to rediscover a five-link feature chain that is passed on
/// one command line in one script.
pub fn warning(acceleration: Acceleration) -> Option<&'static str> {
    match acceleration {
        Acceleration::CudaBuild | Acceleration::CpuElsewhere => None,
        Acceleration::CpuOnAcceleratedHost => Some(
            "This host has an NVIDIA accelerator and this binary was built WITHOUT CUDA. \
             Local inference will run on the CPU: measured on an Orin Nano, 26 tok/s of \
             prefill against 696 with CUDA, so a single turn takes tens of seconds instead \
             of one or two. Nothing else will report this. Rebuild with: \
             cargo build --release -p pond-server \
             --features pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda \
             (this is what scripts/jetson/deploy.sh does; scripts/jetson/build-docker.sh \
             does NOT).",
        ),
    }
}

/// Whether a host looks like it has an NVIDIA accelerator GIAP should be using.
///
/// Takes the evidence rather than reading it, so the decision is testable
/// without a Jetson. `model` is the contents of `/proc/device-tree/model`, which
/// is what `scripts/giap.sh` already reads for the same question — matching it
/// deliberately, so the shell and the binary cannot disagree about what host
/// they are on.
///
/// `has_tegra_release` is `/etc/nv_tegra_release`, present on a JetPack install.
/// Either signal alone is enough: the device tree names the board on a Jetson,
/// and the release file survives on hosts whose device tree is unreadable in a
/// container.
pub fn host_is_accelerated(model: Option<&str>, has_tegra_release: bool) -> bool {
    if has_tegra_release {
        return true;
    }
    let Some(model) = model else {
        return false;
    };
    // Case-insensitive because the string is vendor-supplied and has varied:
    // "NVIDIA Jetson Orin Nano Developer Kit", "Jetson-AGX", "nvidia,p3768".
    let model = model.to_ascii_lowercase();
    model.contains("jetson") || model.contains("tegra") || model.contains("nvidia")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CUDA build is fine wherever it runs, and a CPU build is fine anywhere
    /// there is nothing to miss out on. Exactly one cell of this table is a
    /// problem, and stating it as a table is the point: an `if !cuda { warn }`
    /// would shout at every developer laptop and be muted within a week.
    #[test]
    fn only_a_cpu_build_on_an_accelerated_host_is_a_problem() {
        assert_eq!(classify(true, true), Acceleration::CudaBuild);
        assert_eq!(classify(false, true), Acceleration::CudaBuild);
        assert_eq!(classify(false, false), Acceleration::CpuElsewhere);
        assert_eq!(
            classify(true, false),
            Acceleration::CpuOnAcceleratedHost,
            "a Jetson running a CPU binary is the one case nothing else reports"
        );
    }

    #[test]
    fn only_the_problem_case_says_anything() {
        assert!(warning(Acceleration::CudaBuild).is_none());
        assert!(warning(Acceleration::CpuElsewhere).is_none());
        assert!(warning(Acceleration::CpuOnAcceleratedHost).is_some());
    }

    /// The warning has to carry the fix. The five-link feature chain is not
    /// something a reader can be expected to reconstruct from "CUDA is off", and
    /// the script that gets it wrong is worth naming next to the one that gets
    /// it right.
    #[test]
    fn the_warning_names_the_rebuild_and_the_script_that_omits_it() {
        let w = warning(Acceleration::CpuOnAcceleratedHost).expect("the problem case warns");
        assert!(
            w.contains("pond-adapters-local-inference/cuda"),
            "the warning must carry the feature flag that actually matters: {w}"
        );
        assert!(
            w.contains("build-docker.sh"),
            "the warning must name the build path that omits CUDA: {w}"
        );
    }

    #[test]
    fn a_jetson_is_recognised_however_its_device_tree_spells_it() {
        for model in [
            // Read off the actual device on 2026-08-16. The invented strings
            // below it are variants; THIS one is the deployment, and a matcher
            // that only ever saw hand-written examples is a matcher nobody has
            // checked against the hardware.
            "NVIDIA Jetson Orin Nano Engineering Reference Developer Kit Super",
            "NVIDIA Jetson Orin Nano Developer Kit",
            "Jetson-AGX",
            "nvidia,p3768-0000+p3767-0005",
            "NVIDIA Tegra",
        ] {
            assert!(
                host_is_accelerated(Some(model), false),
                "should recognise {model:?}"
            );
        }
    }

    /// The release file alone is enough. In a container the device tree is
    /// routinely absent, and that is precisely where a CPU-only image built by
    /// `build-docker.sh` would otherwise pass unnoticed.
    #[test]
    fn a_container_without_a_device_tree_is_still_recognised_by_jetpack() {
        assert!(host_is_accelerated(None, true));
        assert!(!host_is_accelerated(None, false));
    }

    /// The control, and without it the matcher could return `true` for anything.
    #[test]
    fn an_ordinary_host_is_not_mistaken_for_an_accelerated_one() {
        for model in [
            "Apple M4 Pro",
            "Raspberry Pi 5 Model B",
            "",
            "Generic x86_64",
        ] {
            assert!(
                !host_is_accelerated(Some(model), false),
                "should NOT claim {model:?} is accelerated"
            );
        }
    }
}
