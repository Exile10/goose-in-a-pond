//! PAI-1's device-to-profile rung exists and **nothing in production reaches
//! it**. This file is the proof of the second half, and it is written to fail
//! the day the first caller lands.
//!
//! A domain-and-repository phase that ends with an unreached domain is
//! legitimate -- PAI-6 P1 was one -- but only when it says so, and this
//! programme has three recorded cases of a phase stamping itself as working
//! while being inert. The usual defence, a `dead_code` warning, cannot fire
//! here: every symbol is `pub` in a library crate, so its absence proves
//! nothing at all.
//!
//! So the claim is made as an executable assertion instead. When the pairing
//! handler starts issuing member-bound codes, or PAI-7 P5 starts resolving a
//! profile's devices, these tests fail with a message naming the phase stamp
//! that has to be rewritten. A comment saying "not wired yet" rots silently;
//! this cannot.
//!
//! Reading a file with `include_str!` creates no dependency edge and needs no
//! link, so this runs in CI's fast pass -- `ci.yml` has no
//! `cargo test -p pond-server`, and a guard placed next to `main.rs` would
//! never fire on a pull request (PAI-2 P3's lesson).

const MAIN: &str = include_str!("../../pond-server/src/main.rs");
const ROUTES: &str = include_str!("../../pond-api/src/routes.rs");

/// Guard against the guard: if these two `include_str!` paths ever stop
/// pointing at the files this test believes they point at, every assertion
/// below passes by matching nothing. Four of this programme's recorded
/// vacuous-test incidents were exactly that shape.
#[test]
fn the_files_this_test_reads_are_the_ones_it_thinks_they_are() {
    assert!(
        MAIN.contains("async fn run_server"),
        "MAIN is not pond-server/src/main.rs -- and this control caught its own author \
         writing `async fn serve`, which no longer exists, on the first run"
    );
    assert!(
        MAIN.contains("issue_pairing_code"),
        "MAIN no longer issues a pairing code; this guard is reading the wrong file \
         or the startup banner has moved"
    );
    assert!(
        ROUTES.contains("async fn handshake_issue_pairing_code"),
        "ROUTES is not pond-api/src/routes.rs, or the loopback issuance route has moved"
    );
    assert!(
        ROUTES.contains("paired_device_profile"),
        "ROUTES no longer builds ResolutionInputs; the resolver call site has moved \
         and the assertions below would pass vacuously"
    );
}

/// The identity direction. `identity_resolution::resolve` has honoured
/// `paired_device_profile` since 2026-08-04 and every caller feeds it `None`.
/// That is still true, and the day it stops being true the resolver's strongest
/// rung goes live -- which changes what an unidentified speaker can reach and
/// must be stamped rather than discovered.
#[test]
fn no_turn_resolves_a_paired_device_to_a_member_yet() {
    let offenders: Vec<&str> = ROUTES
        .match_indices("paired_device_profile")
        .map(|(at, _)| ROUTES[at..].lines().next().unwrap_or("").trim())
        .filter(|line| !line.contains("paired_device_profile: None"))
        .collect();
    assert!(
        offenders.is_empty(),
        "PAI-1's paired-device rung is live: {offenders:?}. \
         `IdentificationSource::PairedDevice` outranks face and explicit, so this \
         changes every turn's scope. Update the phase stamp in \
         docs/architecture/pai/01-identity-and-profile-boundaries.md and delete this test."
    );
}

/// The delivery direction. PAI-7 section 3.4 wants profile -> paired devices ->
/// push tokens; `DeviceAttribution` is that chain and no production wiring
/// constructs it. `AppState` is built in `main.rs`, so if it is anywhere, it is
/// there.
#[test]
fn nothing_wires_the_device_attribution_repository_yet() {
    for (name, source) in [("main.rs", MAIN), ("routes.rs", ROUTES)] {
        for symbol in [
            "SqliteDeviceAttribution",
            "DeviceAttribution",
            "set_device_profile",
            "push_tokens_for_profile",
            "devices_for_profile",
        ] {
            assert!(
                !source.contains(symbol),
                "{name} now reaches {symbol}. PAI-1's device-to-profile rung has its first \
                 production caller, so the \"nothing reaches this\" stamp in \
                 docs/architecture/pai/01-identity-and-profile-boundaries.md is now false. \
                 Rewrite it, then delete this test."
            );
        }
    }
}

/// The capture point. `issue_pairing_code_for` is what binds a household member
/// to a pairing code; until the loopback issuance route offers the operator that
/// choice, every code is unattributed and so is every device that pairs with
/// one.
#[test]
fn the_pairing_route_does_not_capture_a_member_yet() {
    assert!(
        !ROUTES.contains("issue_pairing_code_for"),
        "the loopback issuance route now binds a household member to a pairing code. \
         That makes `devices.profile_id` reachable in production for the first time -- \
         update the phase stamp in \
         docs/architecture/pai/01-identity-and-profile-boundaries.md, and check the \
         route is still loopback-only, because the whole security argument for capturing \
         the member at issuance rests on it."
    );
    // And the argument for that design: the pairing client must not be able to
    // name its own member. `VerifyRequest` carries no profile field, and the
    // behavioural half of this is
    // `sqlite_handshake::tests::the_pairing_client_cannot_name_its_own_member`.
    assert!(
        !ROUTES.contains("request.profile_id") && !ROUTES.contains("verify.profile_id"),
        "the pairing request is being read for a member. `PairedDevice` outranks every \
         other rung of identity resolution, so a client-asserted profile outranks every \
         proof this pond can make -- the same defect PAI-1 P4 closed on \
         PUT /sessions/{{id}}/user. Capture the member at code issuance instead."
    );
}
