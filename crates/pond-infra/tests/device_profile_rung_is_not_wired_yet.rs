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
//!
//! **All three of this file's claims have now been retired, in the order they
//! came true** (see the notes below). The file is kept because each retirement
//! records what the guard caught and what changed -- deleting it would delete
//! the only account of why the rung was unreached for a phase. What remains
//! executable is the control below, which is what stopped the retirement notes
//! from being written against files this test was not actually reading.
//!
//! The live guards are in `device_rung_wiring.rs`, next to this one. They assert
//! the wiring is PRESENT; this file only ever asserted it was absent.

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

// RETIRED 2026-08-11: `no_turn_resolves_a_paired_device_to_a_member_yet`.
//
// It asserted that every `paired_device_profile` in `routes.rs` was fed the literal `None`, and it
// failed the moment `resolve_turn_scope` began feeding it `device_rung.profile_id()`. Its failure
// message asked for the PAI-1 stamp to be rewritten and for the test to be deleted; the stamp is
// the coordinator's, and this note is the deletion.
//
// What changed is the missing half it named: a turn now CARRIES a device id. `session_tokens`
// always stored one and the auth layer never surfaced it. `Handshake::caller_for_token` returns a
// `TokenCaller { client_id, device_id }`, the auth middleware puts the device on the `Principal`,
// and `resolve_turn_scope` asks `DeviceAttribution::device_profile` about it. So
// `IdentificationSource::PairedDevice` -- which outranks face and explicit identification -- is
// reachable on every turn for the first time.
//
// Four properties are worth restating, because this test's whole point was that the rung going
// live is a decision rather than a discovery:
//
//   * The device id comes from the token THIS POND ISSUED and from nowhere else. It is carried by
//     `ProvenDevice`, whose only id-bearing constructor is `from_principal`, so there is no
//     signature a header or a body field can satisfy.
//   * An unattributed device (`devices.profile_id` NULL) falls THROUGH -- migration 0043's header
//     is explicit that the identity and delivery directions are not mirror images.
//   * A failed attribution read falls through too, loudly, and never assumes a member.
//   * The loopback dev bypass returns before a token is read, so it carries no device and resolves
//     nobody. That was the failure mode most worth getting wrong: "the device that paired most
//     recently" would have made every local request speak as whoever last paired a phone.
//
// The live guards moved to `crates/pond-infra/tests/device_rung_wiring.rs`, which asserts the
// wiring is PRESENT rather than absent, and to
// `pond_core::security::domain::proven_device`'s unit tests, which assert what each rung resolves.

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
