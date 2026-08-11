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

// RETIRED 2026-08-11: `nothing_wires_the_device_attribution_repository_yet`.
//
// It asserted that neither `main.rs` nor `routes.rs` named `SqliteDeviceAttribution` or any of the
// four `DeviceAttribution` methods, and it failed the moment `main.rs` built one and handed it to
// `BroadcastNotificationSender::with_device_attribution`. Its message asked for the PAI-1 stamp to
// be rewritten first and then for its own deletion; both were done in that order.
//
// The DELIVERY direction of PAI-7 section 3.4 -- profile -> paired devices -> push tokens -- now
// has a production caller. It is worth being exact about what that does and does not mean:
// `send_to_profile` will resolve a member's devices instead of answering `AttributionUnavailable`,
// which it did on every pond until this wiring. Nothing yet CALLS `send_to_profile` outside a test;
// the producer that will is PAI-7 P4's reviewer, whose domain is landed and whose background loop
// is not. Functional and unreached are different claims and the stamp makes both.
//
// The IDENTITY direction is untouched and its guard is deliberately still here:
// `no_turn_resolves_a_paired_device_to_a_member_yet` still passes, because `resolve_turn_scope`
// still feeds `paired_device_profile: None`. That rung needs a device id at the turn, and a turn
// does not carry one -- `session_tokens.device_id` exists but the auth layer does not surface it.
// Until that is threaded, `IdentificationSource::PairedDevice` outranks face and explicit
// identification in a lattice nothing can reach.

// RETIRED 2026-08-11: `the_pairing_route_does_not_capture_a_member_yet`.
//
// It asserted that the loopback issuance route did not yet offer the operator a member to bind a
// pairing code to, and it failed the day `handshake_issue_pairing_code` began calling
// `issue_pairing_code_for`. Its failure message asked for two things and both were done: the PAI-1
// stamp was rewritten, and the route was re-checked for the property the whole security argument
// rests on. `if !peer.ip().is_loopback() { return FORBIDDEN }` is still the FIRST statement in that
// handler, before the body is read -- which is what makes capturing the member at issuance
// trustworthy, since the answer then comes from somebody standing at the pond rather than from a
// field a client can put in a request. `IdentificationSource::PairedDevice` outranks both face and
// explicit identification, so a client-asserted member would outrank every proof the pond can make.
//
// The other three assertions in this file are UNCHANGED and still true: no turn resolves a paired
// device to a member, nothing constructs the attribution repository, and no route reads a profile
// off a pairing request. The delivery half of the rung is still unreached, and that is PAI-7 P5's.
