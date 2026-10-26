//! Opens a camera by the cheapest path this host and camera allow: a Jetson's hardware engines,
//! then the camera's own JPEGs through ffmpeg, then ffmpeg decoding every frame.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use chrono::Utc;
use pond_core::user_data::domain::vision::{CapturedFrame, JpegFrame, LumaPlane, FPS_RANGE};
use pond_core::user_data::ports::vision::FrameSource;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::ffmpeg_source::{checked_input, resolve_ffmpeg_bin, CaptureConfig, FfmpegFrameSource};
use crate::gst::{self, HardwareHead, JetsonEngines, VideoCodec};
use crate::jpeg_stream::{JpegSplitter, MultipartParser, Part, RateGate};
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
    let engines = gst::jetson_engines().await;
    let candidates = plan(&cfg, &input, kind, &probe, engines);

    let last = candidates.len() - 1;
    for (i, candidate) in candidates.into_iter().enumerate() {
        let capture = candidate.path();
        let mut source = match start(&candidate, &cfg, &ffmpeg, engines) {
            Ok(source) => source,
            Err(e) if i < last => {
                tracing::warn!(capture = capture.as_str(), error = %format!("{e:#}"), "capture path could not start; trying the next");
                continue;
            }
            Err(e) => return Err(e),
        };
        // The last path is not trialled: its failures surface as the stream ending, with backoff.
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
    Hardware {
        head: HardwareHead,
        live: (u32, u32),
        luma: (u32, u32),
    },
    CameraJpeg {
        args: Vec<String>,
    },
    Software,
}

impl Candidate {
    fn path(&self) -> CapturePath {
        match self {
            Candidate::Hardware { .. } => CapturePath::Hardware,
            Candidate::CameraJpeg { .. } => CapturePath::CameraJpeg,
            Candidate::Software => CapturePath::Software,
        }
    }
}

/// Cheapest first; always ends with [`Candidate::Software`].
fn plan(
    cfg: &CaptureConfig,
    input: &str,
    kind: InputKind,
    probe: &Probe,
    engines: &JetsonEngines,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    match probe {
        Probe::MjpegSizes(sizes) => {
            if let Some((width, height)) = pick_size(sizes, (cfg.width, cfg.height)) {
                if engines.jpeg {
                    out.push(Candidate::Hardware {
                        head: HardwareHead::MjpegCamera {
                            device: input.to_string(),
                            width,
                            height,
                        },
                        live: (width, height),
                        luma: scaled((width, height), gst::LUMA_WIDTH),
                    });
                }
                out.push(Candidate::CameraJpeg {
                    args: camera_jpeg_args(input, kind, Some((width, height))),
                });
            }
        }
        Probe::Stream(Some(info)) if info.codec == "mjpeg" => {
            out.push(Candidate::CameraJpeg {
                args: camera_jpeg_args(input, kind, None),
            });
        }
        Probe::Stream(Some(info)) if engines.video => {
            let head = match (kind, VideoCodec::from_ffmpeg(&info.codec)) {
                (InputKind::Rtsp, Some(codec)) => Some(HardwareHead::Rtsp {
                    url: input.to_string(),
                    codec,
                }),
                (InputKind::File, Some(_)) => Some(HardwareHead::File {
                    path: input.to_string(),
                }),
                _ => None,
            };
            if let Some(head) = head {
                let source = info.size.unwrap_or((cfg.width, cfg.height));
                out.push(Candidate::Hardware {
                    head,
                    live: scaled(source, cfg.width),
                    luma: scaled(source, gst::LUMA_WIDTH),
                });
            }
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
    engines: &JetsonEngines,
) -> Result<Box<dyn FrameSource>> {
    Ok(match candidate {
        Candidate::Hardware { head, live, luma } => {
            let Some(launch) = engines.gst_launch.as_deref() else {
                bail!("gst-launch-1.0 is not installed");
            };
            let args = gst::pipeline_args(head, cfg.fps, *live, *luma)?;
            let process =
                CaptureProcess::spawn(launch.as_os_str(), &args, "gst-launch-1.0", &cfg.input)?;
            Box::new(HardwareSource::new(process, *luma, cfg.fps))
        }
        Candidate::CameraJpeg { args } => {
            let process = CaptureProcess::spawn(ffmpeg, args, "ffmpeg", &cfg.input)?;
            Box::new(CameraJpegSource::new(process, cfg.fps))
        }
        Candidate::Software => Box::new(FfmpegFrameSource::spawn(cfg)?),
    })
}

/// `width` wide at `source`'s aspect, with an even height as the scalers want.
fn scaled(source: (u32, u32), width: u32) -> (u32, u32) {
    let (sw, sh) = (source.0.max(1) as u64, source.1.max(1) as u64);
    let height = ((width as u64 * sh + sw / 2) / sw).max(2) as u32;
    (width, height + height % 2)
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

/// gst-launch's multipart stream: each live JPEG paired with the luma VIC scaled from it.
struct HardwareSource {
    process: CaptureProcess,
    parser: MultipartParser,
    luma_size: (u32, u32),
    // videorate let 20+ fps through at max-rate=5 on a live H.264 RTSP stream.
    gate: RateGate,
    jpeg: Option<Vec<u8>>,
    luma: Option<Vec<u8>>,
}

impl HardwareSource {
    fn new(process: CaptureProcess, luma_size: (u32, u32), fps: u32) -> Self {
        Self {
            process,
            parser: MultipartParser::new(gst::BOUNDARY),
            luma_size,
            gate: RateGate::new(fps),
            jpeg: None,
            luma: None,
        }
    }
}

#[async_trait]
impl FrameSource for HardwareSource {
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
        let (w, h) = self.luma_size;
        loop {
            while let Some(part) = self.parser.next_part()? {
                // A part whose partner never came is replaced by the next of its kind.
                match part {
                    Part::Jpeg(bytes) => self.jpeg = Some(bytes),
                    Part::Raw(bytes) if bytes.len() == (w * h) as usize => self.luma = Some(bytes),
                    Part::Raw(bytes) => bail!(
                        "the hardware pipeline sent {} luma bytes, expected {}x{}",
                        bytes.len(),
                        w,
                        h
                    ),
                }
                match (self.jpeg.take(), self.luma.take()) {
                    (Some(_), Some(_)) if !self.gate.admit(Instant::now()) => {}
                    (Some(jpeg), Some(pixels)) => {
                        return Ok(Some(CapturedFrame::Jpeg(JpegFrame {
                            jpeg: Arc::new(jpeg),
                            luma: Some(LumaPlane {
                                width: w,
                                height: h,
                                pixels,
                            }),
                            captured_at: Utc::now(),
                        })));
                    }
                    (jpeg, luma) => {
                        self.jpeg = jpeg;
                        self.luma = luma;
                    }
                }
            }
            if read_more(&mut self.process, self.parser.buffer_mut()).await? == 0 {
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

    fn jetson() -> JetsonEngines {
        JetsonEngines {
            gst_launch: Some("/usr/bin/gst-launch-1.0".into()),
            jpeg: true,
            video: true,
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
    fn a_usb_camera_on_a_jetson_tries_hardware_then_ffmpeg_jpeg_then_software() {
        let probe = Probe::MjpegSizes(parse_mjpeg_sizes(C920));
        let plan = plan(
            &cfg("/dev/video0"),
            "/dev/video0",
            InputKind::V4l2,
            &probe,
            &jetson(),
        );
        let paths: Vec<_> = plan.iter().map(Candidate::path).collect();
        assert_eq!(
            paths,
            vec![
                CapturePath::Hardware,
                CapturePath::CameraJpeg,
                CapturePath::Software
            ]
        );
        assert_eq!(
            plan[0],
            Candidate::Hardware {
                head: HardwareHead::MjpegCamera {
                    device: "/dev/video0".into(),
                    width: 640,
                    height: 360
                },
                live: (640, 360),
                luma: (128, 72),
            }
        );
        let Candidate::CameraJpeg { args } = &plan[1] else {
            unreachable!()
        };
        assert_eq!(
            args.join(" "),
            "-nostdin -loglevel error -f v4l2 -input_format mjpeg -video_size 640x360 -framerate 30 \
             -i /dev/video0 -map 0:v:0 -c:v copy -f mjpeg -flush_packets 1 -"
        );
    }

    #[test]
    fn elsewhere_a_usb_camera_uses_its_jpeg_through_ffmpeg() {
        let probe = Probe::MjpegSizes(parse_mjpeg_sizes(C920));
        let plan = plan(
            &cfg("/dev/video0"),
            "/dev/video0",
            InputKind::V4l2,
            &probe,
            &JetsonEngines::default(),
        );
        let paths: Vec<_> = plan.iter().map(Candidate::path).collect();
        assert_eq!(paths, vec![CapturePath::CameraJpeg, CapturePath::Software]);

        let no_mjpeg = Probe::MjpegSizes(vec![]);
        let plan = plan_paths("/dev/video0", InputKind::V4l2, &no_mjpeg, &jetson());
        assert_eq!(plan, vec![CapturePath::Software]);
    }

    fn plan_paths(
        input: &str,
        kind: InputKind,
        probe: &Probe,
        engines: &JetsonEngines,
    ) -> Vec<CapturePath> {
        plan(&cfg(input), input, kind, probe, engines)
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
        let rtsp = "rtsp://u:p@cam/live";
        assert_eq!(
            plan_paths(rtsp, InputKind::Rtsp, &stream("h264", None), &jetson()),
            vec![CapturePath::Hardware, CapturePath::Software]
        );
        assert_eq!(
            plan_paths(
                rtsp,
                InputKind::Rtsp,
                &stream("h264", None),
                &JetsonEngines::default()
            ),
            vec![CapturePath::Software]
        );
        assert_eq!(
            plan_paths(rtsp, InputKind::Rtsp, &stream("vp9", None), &jetson()),
            vec![CapturePath::Software]
        );
        assert_eq!(
            plan_paths(rtsp, InputKind::Rtsp, &Probe::Stream(None), &jetson()),
            vec![CapturePath::Software],
            "an unreadable stream gets the full-decode path"
        );
        let http = "http://cam.local/mjpeg";
        assert_eq!(
            plan_paths(
                http,
                InputKind::Http,
                &stream("mjpeg", Some((640, 480))),
                &jetson()
            ),
            vec![CapturePath::CameraJpeg, CapturePath::Software]
        );
        assert_eq!(
            plan_paths(http, InputKind::Http, &stream("h264", None), &jetson()),
            vec![CapturePath::Software],
            "no hardware head reads HTTP video"
        );
    }

    #[test]
    fn a_4_3_stream_keeps_its_shape() {
        let plan = plan(
            &cfg("/tmp/clip.mp4"),
            "/tmp/clip.mp4",
            InputKind::File,
            &stream("hevc", Some((1280, 960))),
            &jetson(),
        );
        assert_eq!(
            plan[0],
            Candidate::Hardware {
                head: HardwareHead::File {
                    path: "/tmp/clip.mp4".into()
                },
                live: (640, 480),
                luma: (128, 96),
            }
        );
        assert_eq!(scaled((1920, 1080), 640), (640, 360));
        assert_eq!(scaled((1366, 768), 128), (128, 72));
    }

    #[test]
    fn inputs_are_classified_by_scheme() {
        assert_eq!(InputKind::of("/dev/video2"), InputKind::V4l2);
        assert_eq!(InputKind::of("RTSP://cam/x"), InputKind::Rtsp);
        assert_eq!(InputKind::of("https://cam/x"), InputKind::Http);
        assert_eq!(InputKind::of("udp://239.0.0.1:1234"), InputKind::OtherUrl);
        assert_eq!(InputKind::of("/home/nano/clip.mp4"), InputKind::File);
    }

    /// A replayed stream arrives all at once; at this rate the gate's interval is zero.
    const NO_RATE_LIMIT: u32 = u32::MAX;

    /// A stand-in for gst-launch: replays a multipart stream from a file on stdout.
    fn replay(stream: &[u8]) -> (tempfile::TempDir, CaptureProcess) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.bin");
        std::fs::write(&path, stream).unwrap();
        let process = CaptureProcess::spawn(
            std::ffi::OsStr::new("cat"),
            &[path.to_string_lossy().into_owned()],
            "gst-launch-1.0",
            "/dev/video0",
        )
        .unwrap();
        (dir, process)
    }

    fn part(kind: &str, data: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "--{}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n",
            gst::BOUNDARY,
            data.len()
        )
        .into_bytes();
        out.extend_from_slice(data);
        out.extend_from_slice(b"\r\n");
        out
    }

    #[tokio::test]
    async fn the_hardware_stream_pairs_each_jpeg_with_its_luma_in_either_order() {
        let luma = |v: u8| vec![v; 4 * 2];
        let stream = [
            part("image/jpeg", b"j1"),
            part("video/x-raw", &luma(1)),
            part("video/x-raw", &luma(2)),
            part("image/jpeg", b"j2"),
            part("image/jpeg", b"lost"),
            part("image/jpeg", b"j3"),
            part("video/x-raw", &luma(3)),
        ]
        .concat();
        let (_dir, process) = replay(&stream);
        let mut source = HardwareSource::new(process, (4, 2), NO_RATE_LIMIT);
        let mut got = Vec::new();
        let ended = loop {
            match source.next_frame().await {
                Ok(Some(CapturedFrame::Jpeg(f))) => {
                    got.push((f.jpeg.to_vec(), f.luma.unwrap().pixels[0]));
                }
                Ok(other) => panic!("unexpected {other:?}"),
                Err(e) => break e,
            }
        };
        assert_eq!(
            got,
            vec![
                (b"j1".to_vec(), 1),
                (b"j2".to_vec(), 2),
                (b"j3".to_vec(), 3)
            ]
        );
        assert!(ended.to_string().contains("camera stream ended"), "{ended}");
    }

    #[tokio::test]
    async fn the_hardware_stream_is_held_to_the_setting_whatever_videorate_does() {
        let stream: Vec<u8> = (0..4u8)
            .flat_map(|i| [part("image/jpeg", &[i]), part("video/x-raw", &[i; 8])].concat())
            .collect();
        let (_dir, process) = replay(&stream);
        let mut source = HardwareSource::new(process, (4, 2), 1);
        match source.next_frame().await.unwrap() {
            Some(CapturedFrame::Jpeg(f)) => assert_eq!(*f.jpeg, vec![0]),
            other => panic!("unexpected {other:?}"),
        }
        assert!(
            source.next_frame().await.is_err(),
            "the three pairs that came within the second were dropped"
        );
    }

    #[tokio::test]
    async fn a_luma_part_of_the_wrong_size_is_an_error() {
        let stream = [part("image/jpeg", b"j1"), part("video/x-raw", &[0; 5])].concat();
        let (_dir, process) = replay(&stream);
        let mut source = HardwareSource::new(process, (4, 2), NO_RATE_LIMIT);
        let err = source.next_frame().await.unwrap_err().to_string();
        assert!(err.contains("expected 4x2"), "{err}");
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
