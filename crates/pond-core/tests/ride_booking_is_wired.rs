//! Asserts `main.rs` still switches ride booking on. CI only `cargo check`s pond-server, and an
//! unreached `pub fn` never warns, so a lost call would leave booking silently off everywhere.

const MAIN: &str = include_str!("../../pond-server/src/main.rs");
const STARTUP: &str = include_str!("../../pond-server/src/ride_booking.rs");

#[test]
fn the_server_starts_ride_booking() {
    assert!(
        MAIN.contains("ride_booking::start("),
        "main.rs no longer calls ride_booking::start, so book_ride and the phone's ride routes \
         answer that booking is not set up on every pond"
    );
}

#[test]
fn startup_installs_every_piece_that_booking_needs() {
    for piece in [
        "pond_api::rides::install(",
        "init_ride_accounts(",
        "tracking::track_once(",
    ] {
        assert!(
            STARTUP.contains(piece),
            "ride_booking.rs no longer calls {piece}; booking would be half on"
        );
    }
}
