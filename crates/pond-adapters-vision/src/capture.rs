//! Opens a camera by the cheapest path it allows: its own JPEGs through ffmpeg, else ffmpeg
//! decoding every frame.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use chrono::Utc;
use pond_core::user_data::domain::vision::{CapturedFrame, JpegFrame, FPS_RANGE};
use pond_core::user_data::ports::vision::FrameSource;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::ffmpeg_source::{checked_input, resolve_ffmpeg_bin, CaptureConfig, FfmpegFrameSource};
use crate::jpeg_stream::{JpegSplitter, RateGate};
use crate::live_feed::CapturePath;
use crate::process::CaptureProcess;

/// A path tried before a cheaper fallback must show a frame within this, or the next is tried.
pub const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// ffmpeg reads a network stream's header within this, or the stream is treated as unknown.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Bytes read from a capture pipe at a time; about one camera JPEG.
const READ_CHUNK: usize = 64 * 1024;

/// A camera opened for the pipeline, and the path its frames take.
pub struct OpenedSource {
    pub source: Box<dyn FrameSource>,
    pub capture: CapturePath,
}

pub type OpenFuture = Pin<Box<dyn Future<Output = Result<OpenedSource>> + Send>>;

/// Probes the camera, then starts the cheapest path that delivers a frame.
pub async fn open_capture(cfg: CaptureConfig) -> Result<OpenedSource> {
    let input = checked_input(&cfg)?.to_string();
    let ffmpeg = resolve_ffmpeg_bin();
    let kind = InputKind::of(&input);
    let probe = match kind {
        InputKind::V4l2 => Probe::MjpegSizes(probe_mjpeg_sizes(&ffmpeg, &input).await),
        _ => Probe::Stream(probe_stream(&ffmpeg, &input, kind).await),
    };
    let candidates = plan(&cfg, &input, kind, &probe);

    let last = candidates.len() - 1;
    for (i, candidate) in candidates.into_iter().enumerate() {
        let capture = candidate.path();
        let mut source = match start(&candidate, &cfg, &ffmpeg) {
            Ok(source) => source,
            Err(e) if i < last => {
                tracing::warn!(capture = capture.as_str(), error = %format!("{e:#}"), "capture path could not start; trying the next");
                continue;
            }
            Err(e) => return Err(e),
        };
        // The last path keeps today's behaviour: its failures surface as the stream ending.
        if i == last {
            return Ok(OpenedSource { source, capture });
        }
        let why = match tokio::time::timeout(FIRST_FRAME_TIMEOUT, source.next_frame()).await {
            Ok(Ok(Some(first))) => {
                tracing::info!(capture = capture.as_str(), "vision capture path chosen");
                return Ok(OpenedSource {
                    source: Box::new(Primed {
                        first: Some(first),
                        inner: source,
                    }),
                    capture,
                });
            }
            Ok(Ok(None)) => "the stream ended before a frame".to_string(),
            Ok(Err(e)) => format!("{e:#}"),
            Err(_) => format!("no frame within {}s", FIRST_FRAME_TIMEOUT.as_secs()),
        };
        source.close().await;
        tracing::warn!(capture = capture.as_str(), %why, "capture path did not start; trying the next");
    }
    Err(anyhow!("no capture path could start"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputKind {
    V4l2,
    Rtsp,
    Http,
    File,
    OtherUrl,
}

impl InputKind {
    fn of(input: &str) -> Self {
        let lower = input.to_ascii_lowercase();
        if input.starts_with("/dev/") {
            Self::V4l2
        } else if lower.starts_with("rtsp://") || lower.starts_with("rtsps://") {
            Self::Rtsp
        } else if lower.starts_with("http://") || lower.starts_with("https://") {
            Self::Http
        } else if input.contains("://") {
            Self::OtherUrl
        } else {
            Self::File
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Probe {
    /// The MJPEG sizes a V4L2 camera offers.
    MjpegSizes(Vec<(u32, u32)>),
    /// The first video stream of a URL or file, when ffmpeg could read it.
    Stream(Option<StreamInfo>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamInfo {
    codec: String,
    size: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq)]
enum Candidate {
    CameraJpeg { args: Vec<String> },
    Software,
}

impl Candidate {
    fn path(&self) -> CapturePath {
        match self {
            Candidate::CameraJpeg { .. } => CapturePath::CameraJpeg,
            Candidate::Software => CapturePath::Software,
        }
    }
}

/// Cheapest first; always ends with [`Candidate::Software`].
fn plan(cfg: &CaptureConfig, input: &str, kind: InputKind, probe: &Probe) -> Vec<Candidate> {
    let mut out = Vec::new();
    match probe {
        Probe::MjpegSizes(sizes) => {
            if let Some(size) = pick_size(sizes, (cfg.width, cfg.height)) {
                out.push(Candidate::CameraJpeg {
                    args: camera_jpeg_args(input, kind, Some(size)),
                });
            }
        }
        Probe::Stream(Some(info)) if info.codec == "mjpeg" => {
            out.push(Candidate::CameraJpeg {
                args: camera_jpeg_args(input, kind, None),
            });
        }
        Probe::Stream(_) => {}
    }
    out.push(Candidate::Software);
    out
}

fn start(
    candidate: &Candidate,
    cfg: &CaptureConfig,
    ffmpeg: &std::ffi::OsStr,
) -> Result<Box<dyn FrameSource>> {
    Ok(match candidate {
        Candidate::CameraJpeg { args } => {
            let process = CaptureProcess::spawn(ffmpeg, args, "ffmpeg", &cfg.input)?;
            Box::new(CameraJpegSource::new(process, cfg.fps))
        }
        Candidate::Software => Box::new(FfmpegFrameSource::spawn(cfg)?),
    })
}

/// The exact size, else the smallest that covers it, else the largest offered.
fn pick_size(sizes: &[(u32, u32)], target: (u32, u32)) -> Option<(u32, u32)> {
    let area = |s: &(u32, u32)| s.0 as u64 * s.1 as u64;
    if sizes.contains(&target) {
        return Some(target);
    }
    sizes
        .iter()
        .filter(|s| s.0 >= target.0 && s.1 >= target.1)
        .min_by_key(|s| area(s))
        .or_else(|| sizes.iter().max_by_key(|s| area(s)))
        .copied()
}

/// The camera is asked for its full rate: UVC cameras halve it in dim light to expose longer,
/// and [`RateGate`] drops whatever exceeds the setting.
fn camera_jpeg_args(input: &str, kind: InputKind, size: Option<(u32, u32)>) -> Vec<String> {
    let mut args: Vec<String> = vec!["-nostdin".into(), "-loglevel".into(), "error".into()];
    match kind {
        InputKind::V4l2 => {
            args.extend([
                "-f".into(),
                "v4l2".into(),
                "-input_format".into(),
                "mjpeg".into(),
            ]);
            if let Some((w, h)) = size {
                args.extend(["-video_size".into(), format!("{w}x{h}")]);
            }
            args.extend(["-framerate".into(), FPS_RANGE.end().to_string()]);
        }
        InputKind::Rtsp => args.extend(["-rtsp_transport".into(), "tcp".into()]),
        _ => {}
    }
    args.extend(["-i".into(), input.to_string()]);
    args.extend(
        [
            "-map",
            "0:v:0",
            "-c:v",
            "copy",
            "-f",
            "mjpeg",
            "-flush_packets",
            "1",
            "-",
        ]
        .map(String::from),
    );
    args
}

async fn probe_output(ffmpeg: &std::ffi::OsStr, args: &[String]) -> Option<String> {
    let run = Command::new(ffmpeg)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Ok(Ok(out)) => Some(String::from_utf8_lossy(&out.stderr).into_owned()),
        _ => None,
    }
}

async fn probe_mjpeg_sizes(ffmpeg: &std::ffi::OsStr, device: &str) -> Vec<(u32, u32)> {
    let args = [
        "-hide_banner",
        "-nostdin",
        "-f",
        "v4l2",
        "-list_formats",
        "compressed",
        "-i",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([device.to_string()])
    .collect::<Vec<_>>();
    probe_output(ffmpeg, &args)
        .await
        .map(|text| parse_mjpeg_sizes(&text))
        .unwrap_or_default()
}

async fn probe_stream(
    ffmpeg: &std::ffi::OsStr,
    input: &str,
    kind: InputKind,
) -> Option<StreamInfo> {
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-nostdin".into()];
    if kind == InputKind::Rtsp {
        args.extend(["-rtsp_transport".into(), "tcp".into()]);
    }
    args.extend(["-i".into(), input.to_string()]);
    probe_output(ffmpeg, &args)
        .await
        .and_then(|text| parse_stream_info(&text))
}

/// From `ffmpeg -list_formats`: `... Compressed:  mjpeg :  Motion-JPEG : 640x480 160x90 ...`.
fn parse_mjpeg_sizes(text: &str) -> Vec<(u32, u32)> {
    text.lines()
        .filter(|l| l.contains("Compressed:") && l.contains("mjpeg"))
        .filter_map(|l| l.rsplit(" : ").next())
        .flat_map(|sizes| sizes.split_whitespace().filter_map(parse_size))
        .collect()
}

/// From ffmpeg's input dump: `Stream #0:0: Video: h264 (High), yuv420p, 1920x1080 [SAR 1:1]`.
fn parse_stream_info(text: &str) -> Option<StreamInfo> {
    let line = text.lines().find(|l| l.contains("Video: "))?;
    let rest = &line[line.find("Video: ")? + "Video: ".len()..];
    let codec = rest
        .split([' ', ','])
        .next()
        .filter(|c| !c.is_empty())?
        .to_ascii_lowercase();
    let size = rest
        .split([' ', ',', '[', ']', '(', ')'])
        .find_map(parse_size);
    Some(StreamInfo { codec, size })
}

fn parse_size(token: &str) -> Option<(u32, u32)> {
    // `0x31637661` in a codec tag is hex, not a size.
    if token.starts_with("0x") {
        return None;
    }
    let (w, h) = token.split_once('x')?;
    let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
    (w >= 16 && h >= 16).then_some((w, h))
}

/// Hands out the frame a path proved itself with before reading on.
struct Primed {
    first: Option<CapturedFrame>,
    inner: Box<dyn FrameSource>,
}

#[async_trait]
impl FrameSource for Primed {
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
        match self.first.take() {
            Some(first) => Ok(Some(first)),
            None => self.inner.next_frame().await,
        }
    }

    async fn close(&mut self) {
        self.inner.close().await;
    }
}

/// Reads more of a capture pipe into `buf`; `Ok(0)` at its end.
async fn read_more(process: &mut CaptureProcess, buf: &mut Vec<u8>) -> Result<usize> {
    buf.reserve(READ_CHUNK);
    Ok(process.stdout.read_buf(buf).await?)
}

/// The camera's own JPEGs from `ffmpeg -c:v copy -f mjpeg`.
struct CameraJpegSource {
    process: CaptureProcess,
    splitter: JpegSplitter,
    gate: RateGate,
}

impl CameraJpegSource {
    fn new(process: CaptureProcess, fps: u32) -> Self {
        Self {
            process,
            splitter: JpegSplitter::default(),
            gate: RateGate::new(fps),
        }
    }
}

#[async_trait]
impl FrameSource for CameraJpegSource {
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
        loop {
            while let Some(jpeg) = self.splitter.next_jpeg()? {
                if self.gate.admit(Instant::now()) {
                    return Ok(Some(CapturedFrame::Jpeg(JpegFrame {
                        jpeg: Arc::new(jpeg),
                        luma: None,
                        captured_at: Utc::now(),
                    })));
                }
            }
            if read_more(&mut self.process, self.splitter.buffer_mut()).await? == 0 {
                bail!("camera stream ended ({})", self.process.exit_reason().await);
            }
        }
    }

    async fn close(&mut self) {
        self.process.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C920: &str = "[in#0 @ 0xaaab21335890] Raw       :     yuyv422 :           YUYV 4:2:2 : 640x480 160x90 1280x720 1920x1080\n\
        [in#0 @ 0xaaab21335890] Compressed:       mjpeg :          Motion-JPEG : 640x480 160x90 640x360 1280x720 1920x1080\n\
        Error opening input file /dev/video0.\n";

    fn cfg(input: &str) -> CaptureConfig {
        CaptureConfig {
            input: input.into(),
            fps: 15,
            ..Default::default()
        }
    }

    #[test]
    fn mjpeg_sizes_come_from_the_compressed_line_only() {
        assert_eq!(
            parse_mjpeg_sizes(C920),
            vec![(640, 480), (160, 90), (640, 360), (1280, 720), (1920, 1080)]
        );
        // Older ffmpeg names the demuxer instead of the input.
        let old = "[video4linux2,v4l2 @ 0x55] Compressed:       mjpeg :          Motion-JPEG : 1280x720 640x480";
        assert_eq!(parse_mjpeg_sizes(old), vec![(1280, 720), (640, 480)]);
        assert!(parse_mjpeg_sizes("Raw : yuyv422 : YUYV : 640x480").is_empty());
    }

    #[test]
    fn the_closest_mjpeg_size_is_chosen() {
        let target = (640, 360);
        assert_eq!(
            pick_size(&[(1280, 720), (640, 360)], target),
            Some((640, 360))
        );
        assert_eq!(
            pick_size(&[(1920, 1080), (640, 480), (320, 240)], target),
            Some((640, 480)),
            "smallest that covers it"
        );
        assert_eq!(
            pick_size(&[(320, 240), (160, 120)], target),
            Some((320, 240))
        );
        assert_eq!(pick_size(&[], target), None);
    }

    #[test]
    fn stream_info_reads_codec_and_size_and_skips_hex_tags() {
        let h264 = "  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 1920x1080 [SAR 1:1 DAR 16:9], 30 fps";
        assert_eq!(
            parse_stream_info(h264),
            Some(StreamInfo {
                codec: "h264".into(),
                size: Some((1920, 1080))
            })
        );
        let mjpeg = "  Stream #0:0: Video: mjpeg (Baseline), yuvj420p(pc, bt470bg/unknown/unknown), 640x360, 25 tbr";
        assert_eq!(parse_stream_info(mjpeg).unwrap().codec, "mjpeg");
        assert_eq!(parse_stream_info("Connection refused"), None);
    }

    #[test]
    fn a_usb_camera_with_mjpeg_passes_its_jpeg_through_ffmpeg() {
        let probe = Probe::MjpegSizes(parse_mjpeg_sizes(C920));
        let plan = plan(&cfg("/dev/video0"), "/dev/video0", InputKind::V4l2, &probe);
        let paths: Vec<_> = plan.iter().map(Candidate::path).collect();
        assert_eq!(paths, vec![CapturePath::CameraJpeg, CapturePath::Software]);
        let Candidate::CameraJpeg { args } = &plan[0] else {
            unreachable!()
        };
        assert_eq!(
            args.join(" "),
            "-nostdin -loglevel error -f v4l2 -input_format mjpeg -video_size 640x360 -framerate 30 \
             -i /dev/video0 -map 0:v:0 -c:v copy -f mjpeg -flush_packets 1 -"
        );

        let no_mjpeg = Probe::MjpegSizes(vec![]);
        let plan = plan_paths("/dev/video0", InputKind::V4l2, &no_mjpeg);
        assert_eq!(plan, vec![CapturePath::Software]);
    }

    fn plan_paths(input: &str, kind: InputKind, probe: &Probe) -> Vec<CapturePath> {
        plan(&cfg(input), input, kind, probe)
            .iter()
            .map(Candidate::path)
            .collect()
    }

    fn stream(codec: &str, size: Option<(u32, u32)>) -> Probe {
        Probe::Stream(Some(StreamInfo {
            codec: codec.into(),
            size,
        }))
    }

    #[test]
    fn network_streams_choose_by_codec() {
        let http = "http://cam.local/mjpeg";
        assert_eq!(
            plan_paths(http, InputKind::Http, &stream("mjpeg", Some((640, 480)))),
            vec![CapturePath::CameraJpeg, CapturePath::Software]
        );
        let rtsp = "rtsp://u:p@cam/live";
        assert_eq!(
            plan_paths(rtsp, InputKind::Rtsp, &stream("h264", None)),
            vec![CapturePath::Software]
        );
        assert_eq!(
            plan_paths(rtsp, InputKind::Rtsp, &Probe::Stream(None)),
            vec![CapturePath::Software],
            "an unreadable stream keeps today's path"
        );
    }

    #[test]
    fn inputs_are_classified_by_scheme() {
        assert_eq!(InputKind::of("/dev/video2"), InputKind::V4l2);
        assert_eq!(InputKind::of("RTSP://cam/x"), InputKind::Rtsp);
        assert_eq!(InputKind::of("https://cam/x"), InputKind::Http);
        assert_eq!(InputKind::of("udp://239.0.0.1:1234"), InputKind::OtherUrl);
        assert_eq!(InputKind::of("/home/nano/clip.mp4"), InputKind::File);
    }

    /// A stand-in for a capture process: replays a byte stream from a file on stdout.
    fn replay(stream: &[u8]) -> (tempfile::TempDir, CaptureProcess) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.bin");
        std::fs::write(&path, stream).unwrap();
        let process = CaptureProcess::spawn(
            std::ffi::OsStr::new("cat"),
            &[path.to_string_lossy().into_owned()],
            "ffmpeg",
            "/dev/video0",
        )
        .unwrap();
        (dir, process)
    }

    #[tokio::test]
    async fn camera_jpegs_are_split_and_gated() {
        let jpeg = |seed: u8| {
            crate::snapshot::frame_to_jpeg_bytes(&pond_core::user_data::domain::vision::Frame {
                width: 16,
                height: 8,
                rgb: vec![seed; 16 * 8 * 3],
                captured_at: Utc::now(),
            })
            .unwrap()
        };
        let (a, b) = (jpeg(10), jpeg(200));
        let (_dir, process) = replay(&[a.clone(), b].concat());
        // Both arrive at once: a 1 fps gate keeps the first.
        let mut source = CameraJpegSource::new(process, 1);
        match source.next_frame().await.unwrap() {
            Some(CapturedFrame::Jpeg(f)) => {
                assert_eq!(*f.jpeg, a);
                assert!(f.luma.is_none(), "decoding is left to the pipeline");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(source.next_frame().await.is_err(), "the stream then ends");
    }

    #[tokio::test]
    async fn primed_hands_out_the_proving_frame_first() {
        let frame = CapturedFrame::Raw(pond_core::user_data::domain::vision::Frame {
            width: 1,
            height: 1,
            rgb: vec![0; 3],
            captured_at: Utc::now(),
        });
        let (_dir, process) = replay(b"");
        let mut primed = Primed {
            first: Some(frame),
            inner: Box::new(CameraJpegSource::new(process, 5)),
        };
        assert!(matches!(
            primed.next_frame().await,
            Ok(Some(CapturedFrame::Raw(_)))
        ));
        assert!(primed.next_frame().await.is_err());
        primed.close().await;
    }
}
