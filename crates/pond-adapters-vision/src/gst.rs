//! gst-launch-1.0 pipelines that put a Jetson's NVDEC, NVJPG and VIC engines in the capture path,
//! and the check that this host has them.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Result};
use tokio::process::Command;
use tokio::sync::OnceCell;

/// Separates the JPEG and luma parts on the pipeline's stdout.
pub const BOUNDARY: &str = "giapframe";

/// Twice the motion grid's width, so VIC's filtered scale leaves a sample per grid cell and more.
pub const LUMA_WIDTH: u32 = 128;

/// RTSP jitter buffer; gst's 2 s default would delay every event by that much.
const RTSP_LATENCY_MS: u32 = 200;

/// Bounds each tee branch so a slow reader drops frames at the source instead of queueing them.
const QUEUE: [&str; 2] = ["queue", "max-size-buffers=4"];

/// The Jetson elements each hardware path needs; a host missing any of them never tries it.
const JPEG_ELEMENTS: &[&str] = &["nvv4l2decoder", "nvvidconv", "videorate", "multipartmux"];
const VIDEO_ELEMENTS: &[&str] = &[
    "nvv4l2decoder",
    "nvvidconv",
    "nvjpegenc",
    "videorate",
    "multipartmux",
];

const INSPECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
}

impl VideoCodec {
    /// From ffmpeg's codec name.
    pub fn from_ffmpeg(name: &str) -> Option<Self> {
        match name {
            "h264" => Some(Self::H264),
            "hevc" | "h265" => Some(Self::H265),
            _ => None,
        }
    }

    /// RTSP often carries the parameter sets only in its SDP; nvv4l2decoder output nothing until
    /// the parser repeated them before each keyframe.
    fn depay_parse(self) -> [&'static str; 4] {
        match self {
            Self::H264 => ["rtph264depay", "!", "h264parse", "config-interval=-1"],
            Self::H265 => ["rtph265depay", "!", "h265parse", "config-interval=-1"],
        }
    }
}

/// Where the hardware pipeline's pictures come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardwareHead {
    /// A USB camera's own MJPEG at one of its sizes; NVJPG decodes it for motion only.
    MjpegCamera {
        device: String,
        width: u32,
        height: u32,
    },
    /// H.264 or H.265 over RTSP, decoded by NVDEC.
    Rtsp { url: String, codec: VideoCodec },
    /// A recorded file, decoded by NVDEC at its own pace.
    File { path: String },
}

/// The gst-launch-1.0 argv for `head` at `fps`: a multipart stream on stdout alternating the
/// live JPEG (`live` size when re-encoded) and an 8-bit luma picture of `luma` size.
pub fn pipeline_args(
    head: &HardwareHead,
    fps: u32,
    live: (u32, u32),
    luma: (u32, u32),
) -> Result<Vec<String>> {
    let mut a: Vec<String> = vec!["-q".into()];
    let jpeg_decoder: &[&str] = match head {
        HardwareHead::MjpegCamera {
            device,
            width,
            height,
        } => {
            let device = format!("device={}", quoted(device)?);
            let caps = format!("image/jpeg,width={width},height={height}");
            push(&mut a, &["v4l2src", &device, "!", &caps]);
            // nvjpegdec spent a whole core on the C920's 4:2:2 frames; this reaches NVJPG for 2%.
            &["nvv4l2decoder", "mjpeg=1", "!"]
        }
        HardwareHead::Rtsp { url, codec } => {
            let location = format!("location={}", quoted(url)?);
            let latency = format!("latency={RTSP_LATENCY_MS}");
            push(
                &mut a,
                &["rtspsrc", &location, "protocols=tcp", &latency, "!"],
            );
            push(&mut a, &codec.depay_parse());
            push(&mut a, &["!", "nvv4l2decoder"]);
            &[]
        }
        HardwareHead::File { path } => {
            let location = format!("location={}", quoted(path)?);
            push(
                &mut a,
                &["filesrc", &location, "!", "parsebin", "!", "nvv4l2decoder"],
            );
            &[]
        }
    };

    let rate = format!("max-rate={}", fps.max(1));
    push(
        &mut a,
        &[
            "!",
            "videorate",
            "drop-only=true",
            &rate,
            "!",
            "tee",
            "name=t",
        ],
    );

    let luma_caps = format!(
        "video/x-raw,format=GRAY8,width={},height={}",
        luma.0, luma.1
    );
    push(&mut a, &["t.", "!"]);
    push(&mut a, &QUEUE);
    push(&mut a, &["!"]);
    push(&mut a, jpeg_decoder);
    // multipartmux names request pads in request order, so a named `mux.sink_1` fails to link.
    push(&mut a, &["nvvidconv", "!", &luma_caps, "!", "mux."]);

    push(&mut a, &["t.", "!"]);
    push(&mut a, &QUEUE);
    if !matches!(head, HardwareHead::MjpegCamera { .. }) {
        let live_caps = format!(
            "video/x-raw(memory:NVMM),format=I420,width={},height={}",
            live.0, live.1
        );
        push(
            &mut a,
            &["!", "nvvidconv", "!", &live_caps, "!", "nvjpegenc"],
        );
    }
    push(&mut a, &["!", "mux."]);

    // A file has no live clock, so the sink paces it; a camera paces itself.
    let sync = format!("sync={}", matches!(head, HardwareHead::File { .. }));
    let boundary = format!("boundary={BOUNDARY}");
    push(
        &mut a,
        &[
            "multipartmux",
            "name=mux",
            &boundary,
            "!",
            "fdsink",
            "fd=1",
            &sync,
        ],
    );
    Ok(a)
}

fn push(args: &mut Vec<String>, tokens: &[&str]) {
    args.extend(tokens.iter().map(|t| t.to_string()));
}

/// gst-launch reads its argv as pipeline text: a value is safe inside double quotes once it
/// has no quote, backslash, whitespace or control character.
fn quoted(value: &str) -> Result<String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| (0x21..=0x7E).contains(&b) && b != b'"' && b != b'\\')
    {
        bail!("this camera address cannot be passed to GStreamer");
    }
    Ok(format!("\"{value}\""))
}

/// Which Jetson capture paths this host can run, checked once per process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JetsonEngines {
    pub gst_launch: Option<PathBuf>,
    /// NVJPG decode and VIC scale for a camera's own JPEGs.
    pub jpeg: bool,
    /// NVDEC decode, VIC scale and NVJPG encode for H.264 and H.265.
    pub video: bool,
}

pub async fn jetson_engines() -> &'static JetsonEngines {
    static ENGINES: OnceCell<JetsonEngines> = OnceCell::const_new();
    ENGINES.get_or_init(detect).await
}

async fn detect() -> JetsonEngines {
    // The nv elements are Jetson-only; elsewhere skip the probe processes entirely.
    if !cfg!(target_os = "linux") {
        return JetsonEngines::default();
    }
    let (Some(launch), Some(inspect)) = (find_tool("gst-launch-1.0"), find_tool("gst-inspect-1.0"))
    else {
        return JetsonEngines::default();
    };
    let mut present = Vec::new();
    for element in JPEG_ELEMENTS.iter().chain(VIDEO_ELEMENTS) {
        if !present.contains(element) && has_element(&inspect, element).await {
            present.push(element);
        }
    }
    let all = |needed: &[&str]| needed.iter().all(|e| present.contains(e));
    let engines = JetsonEngines {
        jpeg: all(JPEG_ELEMENTS),
        video: all(VIDEO_ELEMENTS),
        gst_launch: Some(launch),
    };
    tracing::info!(
        jpeg = engines.jpeg,
        video = engines.video,
        "vision capture: Jetson hardware paths checked"
    );
    engines
}

async fn has_element(inspect: &Path, element: &str) -> bool {
    let status = Command::new(inspect)
        .arg(element)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(
        tokio::time::timeout(INSPECT_TIMEOUT, status).await,
        Ok(Ok(s)) if s.success()
    )
}

/// The service's PATH can be minimal, so the system location is tried first.
fn find_tool(name: &str) -> Option<PathBuf> {
    let system = Path::new("/usr/bin").join(name);
    if system.exists() {
        return Some(system);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|p| p.exists())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(args: &[String]) -> String {
        args.join(" ")
    }

    #[test]
    fn a_usb_camera_passes_its_jpeg_through_and_nvjpg_decodes_the_luma() {
        let head = HardwareHead::MjpegCamera {
            device: "/dev/video0".into(),
            width: 640,
            height: 360,
        };
        let args = pipeline_args(&head, 15, (640, 360), (128, 72)).unwrap();
        assert_eq!(
            text(&args),
            "-q v4l2src device=\"/dev/video0\" ! image/jpeg,width=640,height=360 \
             ! videorate drop-only=true max-rate=15 ! tee name=t \
             t. ! queue max-size-buffers=4 ! nvv4l2decoder mjpeg=1 ! nvvidconv \
             ! video/x-raw,format=GRAY8,width=128,height=72 ! mux. \
             t. ! queue max-size-buffers=4 ! mux. \
             multipartmux name=mux boundary=giapframe ! fdsink fd=1 sync=false"
        );
    }

    #[test]
    fn rtsp_is_decoded_by_nvdec_and_reencoded_by_nvjpg() {
        let head = HardwareHead::Rtsp {
            url: "rtsp://admin:pw@192.168.1.20:554/live".into(),
            codec: VideoCodec::H265,
        };
        let args = pipeline_args(&head, 30, (640, 360), (128, 72)).unwrap();
        assert_eq!(
            text(&args),
            "-q rtspsrc location=\"rtsp://admin:pw@192.168.1.20:554/live\" protocols=tcp latency=200 \
             ! rtph265depay ! h265parse config-interval=-1 ! nvv4l2decoder \
             ! videorate drop-only=true max-rate=30 ! tee name=t \
             t. ! queue max-size-buffers=4 ! nvvidconv \
             ! video/x-raw,format=GRAY8,width=128,height=72 ! mux. \
             t. ! queue max-size-buffers=4 ! nvvidconv \
             ! video/x-raw(memory:NVMM),format=I420,width=640,height=360 ! nvjpegenc ! mux. \
             multipartmux name=mux boundary=giapframe ! fdsink fd=1 sync=false"
        );
    }

    #[test]
    fn a_file_plays_at_its_own_pace() {
        let head = HardwareHead::File {
            path: "/tmp/clip.mp4".into(),
        };
        let joined = text(&pipeline_args(&head, 5, (640, 360), (128, 72)).unwrap());
        assert!(
            joined.starts_with("-q filesrc location=\"/tmp/clip.mp4\" ! parsebin ! nvv4l2decoder")
        );
        assert!(joined.ends_with("fdsink fd=1 sync=true"));
    }

    #[test]
    fn an_address_that_could_rewrite_the_pipeline_is_refused() {
        for url in [
            "rtsp://cam\" ! filesink location=\"/etc/x",
            "rtsp://cam/a b",
            "rtsp://cam/\\x",
            "",
            "rtsp://cam/\u{e9}",
        ] {
            let head = HardwareHead::Rtsp {
                url: url.into(),
                codec: VideoCodec::H264,
            };
            assert!(
                pipeline_args(&head, 5, (640, 360), (128, 72)).is_err(),
                "{url:?}"
            );
        }
        assert_eq!(
            quoted("rtsp://a!b@h/x?y=1&z").unwrap(),
            "\"rtsp://a!b@h/x?y=1&z\""
        );
    }

    #[test]
    fn codec_names_follow_ffmpeg() {
        assert_eq!(VideoCodec::from_ffmpeg("h264"), Some(VideoCodec::H264));
        assert_eq!(VideoCodec::from_ffmpeg("hevc"), Some(VideoCodec::H265));
        assert_eq!(VideoCodec::from_ffmpeg("mjpeg"), None);
    }
}
