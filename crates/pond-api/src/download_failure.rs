//! What the household reads when a download fails: what happened and what to do next, never a
//! URL, a path or a library's wording. The raw error goes to the log.

use pond_core::shared::services::egress::EgressDenied;

const DROPPED: &str = "The connection dropped before the file finished. Try again.";
const UNKNOWN: &str = "The download stopped before it finished. Try again.";

/// The download site answered with an error.
pub(crate) fn plain_status(status: u16) -> String {
    match status {
        401 | 403 => "The download site would not let the pond have this file; it may need an \
                      account."
            .to_string(),
        404 | 410 => {
            "The download site no longer has this file. It may have been moved or removed."
                .to_string()
        }
        429 => "The download site asked for a pause. Try again in a few minutes.".to_string(),
        500..=599 => {
            format!("The download site is having trouble (error {status}). Try again later.")
        }
        _ => format!("The download site refused the request (error {status})."),
    }
}

/// The network setting refused the request.
pub(crate) fn plain_denied(denied: &EgressDenied) -> String {
    format!(
        "This pond's network setting does not allow it to reach {}. Change the setting, then \
         try again.",
        denied.host
    )
}

/// A file could not be written.
pub(crate) fn plain_io(err: &std::io::Error) -> String {
    tracing::warn!(error = %err, "a download could not be written");
    io_reason(err)
}

/// A request or its body failed.
pub(crate) fn plain_request(err: &reqwest::Error) -> String {
    tracing::warn!(error = %err, "a download request failed");
    request_reason(err)
}

/// Any failure of a transfer through the Hugging Face cache, by the type behind it.
pub(crate) fn plain_failure(err: &anyhow::Error) -> String {
    use pond_hf_cache::TransferError as T;
    let raw = format!("{err:#}");
    tracing::warn!(error = %raw, "a download failed");
    for cause in err.chain() {
        if let Some(transfer) = cause.downcast_ref::<T>() {
            return match transfer {
                T::SizeMismatch { .. } | T::EtagMismatch { .. } => {
                    "The file on the download site is not the one this model was checked \
                     against, so nothing was saved. Try again later."
                        .to_string()
                }
                T::Short { .. } => "The connection dropped before the file finished. Resume to \
                                    carry on from where it stopped."
                    .to_string(),
                T::Overlong { .. } => "The download site sent more data than the file should \
                                       hold, so it was thrown away. Try again."
                    .to_string(),
                T::RangeMismatch { .. } => "The download site could not carry on where the \
                                            download stopped. Cancel it and start again."
                    .to_string(),
            };
        }
        if let Some(denied) = cause.downcast_ref::<EgressDenied>() {
            return plain_denied(denied);
        }
        if let Some(request) = cause.downcast_ref::<reqwest::Error>() {
            return request_reason(request);
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return io_reason(io);
        }
    }
    status_in(&raw).map_or_else(|| UNKNOWN.to_string(), plain_status)
}

fn io_reason(err: &std::io::Error) -> String {
    use std::io::ErrorKind;
    if err.kind() == ErrorKind::StorageFull || err.raw_os_error() == Some(28) {
        "There is no room left on this device to save the download. Free some space, then try \
         again."
            .to_string()
    } else if err.kind() == ErrorKind::PermissionDenied {
        "The pond is not allowed to write to its models folder. Check the folder's permissions, \
         then try again."
            .to_string()
    } else {
        "The file could not be saved to this device. Try again.".to_string()
    }
}

fn request_reason(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "The download site took too long to answer. Check the connection, then try again."
            .to_string()
    } else if err.is_connect() {
        "The download site could not be reached. Check the connection, then try again.".to_string()
    } else if let Some(status) = err.status() {
        plain_status(status.as_u16())
    } else {
        DROPPED.to_string()
    }
}

/// The status of a "... returned 404 Not Found" message, which the cache words as text.
fn status_in(message: &str) -> Option<u16> {
    let after = message.split(" returned ").nth(1)?;
    let code = after.get(..3)?;
    code.parse().ok().filter(|c| (400..600).contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::shared::services::egress::NetworkMode;
    use pond_hf_cache::TransferError;

    /// What no household message may carry: a place on disk or on the net, a hash, or a
    /// library's own words.
    fn is_plain(message: &str) -> bool {
        !message.is_empty()
            && message.is_ascii()
            && message.ends_with('.')
            && !message.contains('/')
            && !message.contains('[')
            && !message.contains('{')
            && ![
                "http",
                "etag",
                "sha",
                "os error",
                "incomplete",
                "Error:",
                "HEAD",
                "GET ",
            ]
            .iter()
            .any(|word| message.contains(word))
    }

    #[test]
    fn a_pin_that_does_not_match_says_nothing_was_saved() {
        for err in [
            TransferError::SizeMismatch {
                expected: 2,
                actual: 1,
            },
            TransferError::EtagMismatch {
                expected: "df0fd4ee".into(),
                found: vec!["abc".into()],
            },
        ] {
            let said = plain_failure(&anyhow::Error::new(err).context("HEAD https://x/y"));
            assert!(said.contains("nothing was saved"), "{said}");
            assert!(is_plain(&said), "{said}");
        }
    }

    #[test]
    fn every_transfer_error_reads_plainly() {
        for err in [
            TransferError::Short {
                received: 1,
                total: 2,
            },
            TransferError::Overlong {
                received: 3,
                total: 2,
            },
            TransferError::RangeMismatch {
                requested: 1,
                answered: None,
            },
        ] {
            let said = plain_failure(&anyhow::Error::new(err));
            assert!(is_plain(&said), "{said}");
        }
        assert!(plain_failure(&anyhow::Error::new(TransferError::Short {
            received: 1,
            total: 2,
        }))
        .contains("Resume"));
    }

    #[test]
    fn a_full_disk_and_a_locked_folder_say_what_to_do() {
        let full = std::io::Error::from_raw_os_error(28);
        assert!(
            plain_io(&full).contains("no room left"),
            "{}",
            plain_io(&full)
        );
        let full = std::io::Error::new(std::io::ErrorKind::StorageFull, "x");
        assert!(plain_io(&full).contains("Free some space"));
        let locked = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "x");
        assert!(plain_io(&locked).contains("permissions"));
        let odd = std::io::Error::other("write /Users/jerry/x.part: boom");
        assert!(is_plain(&plain_io(&odd)), "{}", plain_io(&odd));

        // Through the cache's context layers.
        let wrapped = anyhow::Error::new(std::io::Error::from_raw_os_error(28))
            .context("write /pond/hf_cache/blobs/abc.incomplete");
        let said = plain_failure(&wrapped);
        assert!(said.contains("no room left"), "{said}");
        assert!(is_plain(&said), "{said}");
    }

    #[test]
    fn a_refused_network_names_the_setting_and_the_site() {
        let denied = EgressDenied {
            host: "huggingface.co".into(),
            mode: NetworkMode::Offline,
            reason: "offline",
        };
        let said = plain_denied(&denied);
        assert!(said.contains("network setting") && said.contains("huggingface.co"));
        assert!(!said.contains("network_mode"), "{said}");
        let through = plain_failure(&anyhow::Error::new(denied).context("HEAD https://x"));
        assert_eq!(through, said);
    }

    #[test]
    fn a_status_the_cache_words_as_text_is_read_back() {
        let by_code = |code: &str| {
            plain_failure(&anyhow::anyhow!(
                "HEAD https://huggingface.co/a/b/resolve/rev/f.gguf returned {code}"
            ))
        };
        assert!(by_code("404 Not Found").contains("no longer has this file"));
        assert!(by_code("403 Forbidden").contains("may need an account"));
        assert!(by_code("429 Too Many Requests").contains("pause"));
        assert!(by_code("503 Service Unavailable").contains("error 503"));
        for code in ["404 Not Found", "403 Forbidden", "429 Too Many Requests"] {
            assert!(is_plain(&by_code(code)), "{code}");
        }
        assert_eq!(status_in("GET x returned 20"), None);
        assert_eq!(
            status_in("GET x returned 200 OK"),
            None,
            "a success is no failure"
        );
        assert_eq!(status_in("nothing here"), None);
    }

    #[test]
    fn an_error_nobody_foresaw_still_says_what_to_do() {
        let said = plain_failure(&anyhow::anyhow!("pointer task panicked: boom"));
        assert_eq!(said, UNKNOWN);
        assert!(is_plain(&said));
    }

    #[tokio::test]
    async fn an_unreachable_site_and_a_slow_one_read_differently() {
        let unreachable = reqwest::Client::new()
            .get("http://127.0.0.1:1/f.gguf")
            .send()
            .await
            .unwrap_err();
        let said = plain_request(&unreachable);
        assert!(said.contains("could not be reached"), "{said}");
        assert!(is_plain(&said), "{said}");

        // A server that takes the connection and never answers.
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/f.gguf", silent.local_addr().unwrap());
        let slow = reqwest::Client::new()
            .get(url)
            .timeout(std::time::Duration::from_millis(50))
            .send()
            .await
            .unwrap_err();
        let said = plain_request(&slow);
        assert!(said.contains("took too long"), "{said}");
        assert!(is_plain(&said), "{said}");
        let wrapped = plain_failure(&anyhow::Error::new(slow).context("GET https://x"));
        assert!(wrapped.contains("took too long"), "{wrapped}");
    }
}
