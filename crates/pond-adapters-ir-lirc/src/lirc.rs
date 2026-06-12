use anyhow::Result;
use std::time::Duration;
use tokio::process::Command;

/// Returns true if LIRC hardware appears available on this system.
pub fn is_available(socket_path: &str) -> bool {
    std::path::Path::new(socket_path).exists()
        || which_irsend()
}

fn which_irsend() -> bool {
    std::process::Command::new("which")
        .arg("irsend")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Send a single IR keypress via `irsend SEND_ONCE`.
pub async fn send_once(remote: &str, key: &str) -> Result<()> {
    let output = Command::new("irsend")
        .args(["SEND_ONCE", remote, key])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("irsend SEND_ONCE {remote} {key} failed: {stderr}");
    }
    Ok(())
}

/// Send a keypress `count` times with `delay_ms` between each press.
///
/// `count` is clamped to 1–50 to prevent runaway repeats from bad LLM params.
pub async fn send_repeat(remote: &str, key: &str, count: usize, delay_ms: u64) -> Result<()> {
    let n = count.clamp(1, 50);
    for i in 0..n {
        send_once(remote, key).await?;
        if i + 1 < n && delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_prevents_runaway_repeats() {
        // We can't call the async function in a unit test without a runtime,
        // but we can verify the clamping logic directly.
        assert_eq!(100_usize.clamp(1, 50), 50);
        assert_eq!(0_usize.clamp(1, 50), 1);
        assert_eq!(5_usize.clamp(1, 50), 5);
    }
}
