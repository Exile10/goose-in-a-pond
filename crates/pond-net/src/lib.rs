//! The pond's HTTP clients. Every outbound client is built here, so each one looks names up the
//! way [`dns`] decides. `crates/pond-core/tests/egress_guard.rs` fails the build when a crate
//! builds a `reqwest` client any other way.

pub mod dns;

/// A client builder that resolves names through [`dns::PondResolver`]. Use it wherever a crate
/// would call `reqwest::Client::builder()`; the caller's own settings go on top as usual.
pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().dns_resolver(dns::PondResolver)
}

/// As `reqwest::Client::new()`, through [`client_builder`].
///
/// # Panics
///
/// As `reqwest::Client::new()` does: when the TLS backend cannot be initialised.
pub fn client() -> reqwest::Client {
    client_builder()
        .build()
        .expect("the HTTP client could not be built (TLS backend)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// One request through a pond client, by name, to a server on this machine.
    #[tokio::test]
    async fn a_pond_client_reaches_a_server_by_name() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .unwrap();
        });

        let body = client()
            .get(format!("http://localhost:{port}/"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
        server.join().unwrap();
    }
}
