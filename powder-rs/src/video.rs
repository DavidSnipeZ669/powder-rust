//! Frame sampling through an ffmpeg subprocess (rawvideo RGB24 on stdout).
//! Using the ffmpeg executable keeps the build simple on Windows (no FFmpeg dev libs) and
//! lets users reuse the ffmpeg.exe that ships with Powder.

use anyhow::{bail, Context, Result};
use image::RgbImage;
use regex::Regex;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration_sec: Option<f64>,
    /// recording frame rate reported by the container
    pub fps: Option<f64>,
}

/// Frame-rate settings chosen by the user.
#[derive(Debug, Clone, Default)]
pub struct FpsSettings {
    /// analysis rate: None = native (every recorded frame)
    pub sample: Option<f64>,
    /// override the recording fps detected from the file (e.g. 60, 120, 144)
    pub recording: Option<f64>,
    /// average in-game fps; frames above this are duplicates and are skipped
    pub game: Option<f64>,
}

impl FpsSettings {
    /// Parse "native"/"max"/"all" or a number.
    pub fn parse_sample(s: &str) -> anyhow::Result<Option<f64>> {
        match s.trim().to_ascii_lowercase().as_str() {
            "native" | "max" | "all" | "full" => Ok(None),
            v => {
                let f: f64 = v.parse().map_err(|_| anyhow::anyhow!("fps must be a number or 'native'"))?;
                if !(0.05..=1000.0).contains(&f) { anyhow::bail!("fps out of range"); }
                Ok(Some(f))
            }
        }
    }

    /// The rate frames are actually sampled at: min(requested, recording, game).
    pub fn effective(&self, info: &VideoInfo, default_sample: f64) -> (f64, String) {
        let rec = self.recording.or(info.fps).unwrap_or(60.0);
        let requested = match self.sample { Some(f) => f, None => rec };
        let mut eff = requested.min(rec);
        let mut why = format!("requested {} | recording {rec:.2}{}", match self.sample { Some(f) => format!("{f}"), None => "native".into() }, if self.recording.is_some() { " (manual)" } else { " (detected)" });
        if let Some(g) = self.game {
            if g < eff { eff = g; }
            why += &format!(" | game {g}");
        }
        let _ = default_sample;
        why += &format!(" -> sampling {eff:.2} fps");
        (eff, why)
    }
}

pub fn probe(ffmpeg: &Path, input: &Path) -> Result<VideoInfo> {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-i"])
        .arg(input)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {}", ffmpeg.display()))?;
    let text = String::from_utf8_lossy(&out.stderr);
    let stream = Regex::new(r"Stream #\d+:\d+.*?Video:.*?(\d{2,5})x(\d{2,5})").unwrap();
    let caps = stream
        .captures(&text)
        .with_context(|| format!("could not find a video stream in {}:\n{}", input.display(), text))?;
    let fps_re = Regex::new(r"Stream #\d+:\d+.*?Video:.*?([\d.]+) (fps|tbr)").unwrap();
    let fps = fps_re.captures(&text).and_then(|c| c[1].parse::<f64>().ok()).filter(|f| *f > 0.0 && *f < 1000.0);
    let dur = Regex::new(r"Duration: (\d+):(\d+):(\d+(?:\.\d+)?)").unwrap();
    let duration_sec = dur.captures(&text).map(|c| {
        c[1].parse::<f64>().unwrap() * 3600.0 + c[2].parse::<f64>().unwrap() * 60.0 + c[3].parse::<f64>().unwrap()
    });
    Ok(VideoInfo { width: caps[1].parse()?, height: caps[2].parse()?, duration_sec, fps })
}

/// Iterator over frames sampled at a fixed fps. Frame i has time `start + i / fps`.
pub struct FrameReader {
    child: Child,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub start: f64,
    index: usize,
    buf: Vec<u8>,
    args: FfArgs,
    hw: bool,
}

struct FfArgs {
    ffmpeg: PathBuf,
    input: PathBuf,
    start: Option<f64>,
    end: Option<f64>,
    fps: f64,
    native: bool,
    w: u32,
    h: u32,
}

impl FfArgs {
    fn spawn(&self, hw: bool) -> Result<Child> {
        let mut cmd = Command::new(&self.ffmpeg);
        cmd.args(["-v", "error", "-nostdin", "-threads", "0"]);
        if hw {
            cmd.args(["-hwaccel", "d3d11va", "-hwaccel_output_format", "d3d11"]);
        }
        if let Some(s) = self.start {
            cmd.args(["-ss", &format!("{s:.3}")]);
        }
        cmd.arg("-i").arg(&self.input);
        if let Some(e) = self.end {
            let d = e - self.start.unwrap_or(0.0);
            if d <= 0.0 {
                bail!("--end must be after --start");
            }
            cmd.args(["-t", &format!("{d:.3}")]);
        }
        // fps filter keeps timing exact (drops/duplicates to the requested rate)
        let scale = format!("scale={}:{}:flags=area", self.w, self.h);
        let vf = match (self.native, hw) {
            (true, _) => scale,
            (false, true) => format!("fps={},hwdownload,format=nv12,{scale}", self.fps),
            (false, false) => format!("fps={},{scale}", self.fps),
        };
        cmd.args(["-an", "-sn", "-vf", &vf]);
        cmd.args(["-pix_fmt", "rgb24", "-f", "rawvideo", "-"]);
        cmd.stdout(Stdio::piped()).stderr(if hw { Stdio::null() } else { Stdio::inherit() }).stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        cmd.spawn().with_context(|| format!("spawning {}", self.ffmpeg.display()))
    }
}

pub struct SampleOptions {
    pub fps: f64,
    pub start: Option<f64>,
    pub end: Option<f64>,
    /// Downscale so the frame height is at most this (keeps aspect). Crop coordinates are
    /// normalised (0-1) so results are resolution-independent.
    pub max_height: Option<u32>,
}

impl FrameReader {
    pub fn open(ffmpeg: &Path, input: &Path, info: &VideoInfo, opt: &SampleOptions) -> Result<Self> {
        let (mut w, mut h) = (info.width, info.height);
        if let Some(mh) = opt.max_height {
            if h > mh {
                w = ((w as f64 * mh as f64 / h as f64) / 2.0).round() as u32 * 2;
                h = mh;
            }
        }
        let native = info.fps.map(|f| (f - opt.fps).abs() < 0.01).unwrap_or(false);
        let args = FfArgs { ffmpeg: ffmpeg.to_path_buf(), input: input.to_path_buf(), start: opt.start, end: opt.end, fps: opt.fps, native, w, h };
        // GPU decode first: high-res / high-fps recordings are mostly decode time (every frame is
        // decoded even though only a few per second are read). The frames stay on the GPU and only
        // the sampled ones are copied back. Falls back to the CPU if this ffmpeg / GPU can't.
        let hw = cfg!(windows) && std::env::var("POWDER_RS_NO_HWDEC").is_err() && !native;
        let child = args.spawn(hw)?;
        Ok(Self { child, width: w, height: h, fps: opt.fps, start: opt.start.unwrap_or(0.0), index: 0, buf: vec![0; (w * h * 3) as usize], args, hw })
    }

    pub fn next_frame(&mut self) -> Result<Option<(f64, RgbImage)>> {
        let out = self.child.stdout.as_mut().unwrap();
        let mut filled = 0;
        while filled < self.buf.len() {
            let n = out.read(&mut self.buf[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled < self.buf.len() {
            let _ = self.child.wait();
            if self.hw && self.index == 0 {
                // GPU decoding didn't work here: decode on the CPU instead
                self.hw = false;
                self.child = self.args.spawn(false)?;
                return self.next_frame();
            }
            return Ok(None);
        }
        let t = self.start + self.index as f64 / self.fps;
        self.index += 1;
        let img = RgbImage::from_raw(self.width, self.height, self.buf.clone()).unwrap();
        Ok(Some((t, img)))
    }
}

impl Drop for FrameReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Grab a single frame at `time` (full resolution).
pub fn grab_frame(ffmpeg: &Path, input: &Path, time: f64) -> Result<RgbImage> {
    let info = probe(ffmpeg, input)?;
    let mut r = FrameReader::open(
        ffmpeg,
        input,
        &info,
        &SampleOptions { fps: 1.0, start: Some(time), end: Some(time + 1.5), max_height: None },
    )?;
    r.next_frame()?.map(|f| f.1).context("no frame at that time")
}

/// Accept image files as single "videos" (handy for calibrating crops on a screenshot).
pub fn is_image(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp" | "bmp")
    )
}

pub fn load_image_or_frame(ffmpeg: Option<&PathBuf>, input: &Path, time: f64) -> Result<RgbImage> {
    if is_image(input) {
        return Ok(image::open(input).with_context(|| format!("opening {}", input.display()))?.to_rgb8());
    }
    let ff = crate::paths::ffmpeg(ffmpeg.map(|p| p.as_path()))?;
    grab_frame(&ff, input, time)
}
