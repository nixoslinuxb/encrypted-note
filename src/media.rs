//! In-app playback for note attachments.
//!
//! Images are drawn inline by the app; this module handles **audio** and
//! **video** so they play without opening an external player window:
//!
//! * audio is decoded by `ffmpeg` to raw PCM, volume-scaled in-process, and
//!   piped to PipeWire (`pw-cat`) or ALSA (`aplay`). Because we own the PCM
//!   stream we get **live volume**, **seeking** (`-ss`) and simple pause/resume;
//! * video is decoded by `ffmpeg` into raw RGBA frames that the app uploads as
//!   an egui texture, with its soundtrack played through the same audio path.
//!
//! Everything runs in child processes found on `PATH`; nothing is linked into
//! the binary. Per-attachment image layout (size + position) and the playback
//! volume are persisted in `media-layout.json` under the config directory.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Frame rate we ask `ffmpeg` to emit for inline video.
const FPS: u32 = 30;
/// Width (in px) we downscale video to for inline playback.
pub const VIDEO_WIDTH: u32 = 640;
/// PCM format we ask for and feed to the sink.
const SAMPLE_RATE: &str = "48000";
const CHANNELS: &str = "2";

/// Media metadata from `ffprobe`.
#[derive(Clone, Copy, Default)]
pub struct Info {
    pub duration: f32,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
}

/// Probe `path` for duration, video dimensions, and whether it has audio.
pub fn probe(path: &Path) -> Info {
    let mut info = Info::default();
    if let Ok(out) = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut parts = line.trim().split(',');
            match parts.next() {
                Some("video") => {
                    info.width = parts
                        .next()
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0);
                    info.height = parts
                        .next()
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0);
                }
                Some("audio") => info.has_audio = true,
                _ => {}
            }
        }
    }
    if let Ok(out) = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
    {
        info.duration = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0.0);
    }
    info
}

/// A decoded RGBA video frame.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

// ------------------------------------------------------------------ audio

/// Audio (or a video's soundtrack) decoded to PCM and piped to a sink. The
/// volume is applied in-process so it changes instantly without a restart.
pub struct Sound {
    ff: Child,
    sink: Child,
    volume: Arc<AtomicU32>,
    path: PathBuf,
    duration: f32,
    started: Instant,
    offset: f32,
    paused: bool,
    paused_at: f32,
}

impl Sound {
    pub fn start(path: &Path, duration: f32, offset: f32, volume: f32) -> Option<Self> {
        let volume = Arc::new(AtomicU32::new(volume.to_bits()));
        let (ff, sink) = spawn_sound(path, offset, volume.clone())?;
        Some(Sound {
            ff,
            sink,
            volume,
            path: path.to_path_buf(),
            duration,
            started: Instant::now(),
            offset,
            paused: false,
            paused_at: 0.0,
        })
    }

    /// Set the volume (applied live by the pumping thread).
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn elapsed(&self) -> f32 {
        if self.paused {
            self.paused_at
        } else {
            clamp_time(
                self.offset + self.started.elapsed().as_secs_f32(),
                self.duration,
            )
        }
    }

    fn kill(&mut self) {
        let _ = self.ff.kill();
        let _ = self.ff.wait();
        let _ = self.sink.kill();
        let _ = self.sink.wait();
    }

    fn reopen(&mut self, pos: f32) {
        self.kill();
        if let Some((ff, sink)) = spawn_sound(&self.path, pos, self.volume.clone()) {
            self.ff = ff;
            self.sink = sink;
            self.offset = pos;
            self.started = Instant::now();
        }
    }

    pub fn pause(&mut self) {
        self.paused_at = self.elapsed();
        self.paused = true;
        self.kill();
    }

    pub fn resume(&mut self) {
        self.reopen(self.paused_at);
        self.paused = false;
    }

    pub fn seek(&mut self, pos: f32) {
        let pos = clamp_time(pos, self.duration);
        if self.paused {
            self.paused_at = pos;
        } else {
            self.reopen(pos);
        }
    }

    pub fn stop(&mut self) {
        self.kill();
    }

    pub fn finished(&mut self) -> bool {
        if self.paused {
            return false;
        }
        let ff_done = matches!(self.ff.try_wait(), Ok(Some(_)));
        let sink_done = matches!(self.sink.try_wait(), Ok(Some(_)));
        ff_done && sink_done
    }
}

// ------------------------------------------------------------------ video

/// Video playback: a stream of decoded frames from `ffmpeg`, plus its
/// soundtrack (when the file has one).
pub struct Video {
    child: Child,
    rx: Receiver<Frame>,
    sound: Option<Sound>,
    path: PathBuf,
    width: u32,
    height: u32,
    duration: f32,
    started: Instant,
    offset: f32,
    paused: bool,
    paused_at: f32,
    last: Option<Frame>,
}

impl Video {
    pub fn open(path: &Path, scale_w: u32, info: Info, offset: f32, volume: f32) -> Option<Self> {
        let (w, h) = scaled(info.width, info.height, scale_w);
        let (child, rx) = spawn_video(path, w, h, offset)?;
        let sound = if info.has_audio {
            Sound::start(path, info.duration, offset, volume)
        } else {
            None
        };
        Some(Video {
            child,
            rx,
            sound,
            path: path.to_path_buf(),
            width: w,
            height: h,
            duration: info.duration,
            started: Instant::now(),
            offset,
            paused: false,
            paused_at: 0.0,
            last: None,
        })
    }

    pub fn set_volume(&self, v: f32) {
        if let Some(s) = &self.sound {
            s.set_volume(v);
        }
    }

    pub fn elapsed(&self) -> f32 {
        if self.paused {
            self.paused_at
        } else {
            clamp_time(
                self.offset + self.started.elapsed().as_secs_f32(),
                self.duration,
            )
        }
    }

    /// Pull the newest available frame (draining any queue).
    pub fn frame(&mut self) -> Option<&Frame> {
        if !self.paused {
            while let Ok(f) = self.rx.try_recv() {
                self.last = Some(f);
            }
        }
        self.last.as_ref()
    }

    fn reopen(&mut self, pos: f32) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some((child, rx)) = spawn_video(&self.path, self.width, self.height, pos) {
            self.child = child;
            self.rx = rx;
            self.offset = pos;
            self.started = Instant::now();
        }
        if let Some(s) = &mut self.sound {
            s.reopen(pos);
        }
    }

    pub fn pause(&mut self) {
        self.paused_at = self.elapsed();
        self.paused = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(s) = &mut self.sound {
            s.pause();
        }
    }

    pub fn resume(&mut self) -> bool {
        let pos = self.paused_at;
        match spawn_video(&self.path, self.width, self.height, pos) {
            Some((child, rx)) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.child = child;
                self.rx = rx;
                self.offset = pos;
                self.started = Instant::now();
                self.paused = false;
                if let Some(s) = &mut self.sound {
                    s.resume();
                }
                true
            }
            None => false,
        }
    }

    pub fn seek(&mut self, pos: f32) {
        let pos = clamp_time(pos, self.duration);
        if self.paused {
            self.paused_at = pos;
            if let Some(s) = &mut self.sound {
                s.seek(pos);
            }
        } else {
            self.reopen(pos);
        }
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(s) = &mut self.sound {
            s.stop();
        }
    }

    pub fn finished(&mut self) -> bool {
        if self.paused {
            return false;
        }
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

/// Either an audio or a video playback session.
pub enum Player {
    Audio(Sound),
    Video(Video),
}

impl Player {
    pub fn elapsed(&self) -> f32 {
        match self {
            Player::Audio(a) => a.elapsed(),
            Player::Video(v) => v.elapsed(),
        }
    }

    pub fn duration(&self) -> f32 {
        match self {
            Player::Audio(a) => a.duration,
            Player::Video(v) => v.duration,
        }
    }

    pub fn is_paused(&self) -> bool {
        match self {
            Player::Audio(a) => a.paused,
            Player::Video(v) => v.paused,
        }
    }

    pub fn pause(&mut self) {
        match self {
            Player::Audio(a) => a.pause(),
            Player::Video(v) => v.pause(),
        }
    }

    pub fn resume(&mut self) {
        match self {
            Player::Audio(a) => a.resume(),
            Player::Video(v) => {
                v.resume();
            }
        }
    }

    pub fn seek(&mut self, pos: f32) {
        match self {
            Player::Audio(a) => a.seek(pos),
            Player::Video(v) => v.seek(pos),
        }
    }

    pub fn set_volume(&self, vol: f32) {
        match self {
            Player::Audio(a) => a.set_volume(vol),
            Player::Video(v) => v.set_volume(vol),
        }
    }

    pub fn stop(&mut self) {
        match self {
            Player::Audio(a) => a.stop(),
            Player::Video(v) => v.stop(),
        }
    }

    pub fn finished(&mut self) -> bool {
        match self {
            Player::Audio(a) => a.finished(),
            Player::Video(v) => v.finished(),
        }
    }
}

fn clamp_time(t: f32, duration: f32) -> f32 {
    if duration > 0.0 {
        t.clamp(0.0, duration)
    } else {
        t.max(0.0)
    }
}

// ------------------------------------------------------------- process spawn

fn null() -> Stdio {
    Stdio::null()
}

/// Spawn `ffmpeg` decoding `path`'s audio to raw PCM from `offset` seconds,
/// pipe it through `aplay`/`pw-cat` and scale samples live by `volume`.
fn spawn_sound(path: &Path, offset: f32, volume: Arc<AtomicU32>) -> Option<(Child, Child)> {
    let mut ff = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error"])
        .arg("-ss")
        .arg(format!("{offset:.3}"))
        .arg("-i")
        .arg(path)
        .args([
            "-vn",
            "-f",
            "s16le",
            "-ac",
            CHANNELS,
            "-ar",
            SAMPLE_RATE,
            "-",
        ])
        .stdin(null())
        .stdout(Stdio::piped())
        .stderr(null())
        .spawn()
        .ok()?;
    let mut ff_out = ff.stdout.take()?;
    let mut sink = spawn_sink()?;
    let mut sink_in = sink.stdin.take()?;

    std::thread::spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = match ff_out.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let v = f32::from_bits(volume.load(Ordering::Relaxed));
            if (v - 1.0).abs() > 1.0e-3 {
                for ch in buf[..n].as_chunks_mut::<2>().0 {
                    let s = i16::from_le_bytes([ch[0], ch[1]]) as f32;
                    let s = (s * v).clamp(-32768.0, 32767.0) as i16;
                    ch.copy_from_slice(&s.to_le_bytes());
                }
            }
            if sink_in.write_all(&buf[..n]).is_err() {
                break;
            }
        }
        let _ = sink_in.flush();
    });
    Some((ff, sink))
}

/// A windowless PCM sink: PipeWire (`pw-cat`) preferred, else ALSA (`aplay`).
fn spawn_sink() -> Option<Child> {
    if let Ok(c) = Command::new("pw-cat")
        .args([
            "-p",
            "-a",
            "--format",
            "s16",
            "--rate",
            SAMPLE_RATE,
            "--channels",
            CHANNELS,
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(null())
        .stderr(null())
        .spawn()
    {
        return Some(c);
    }
    Command::new("aplay")
        .args([
            "-t",
            "raw",
            "-f",
            "S16_LE",
            "-r",
            SAMPLE_RATE,
            "-c",
            CHANNELS,
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(null())
        .stderr(null())
        .spawn()
        .ok()
}

/// Spawn `ffmpeg` decoding `path` to raw RGBA frames of size `w`×`h`, starting
/// at `offset` seconds. Returns the child and a receiver of decoded frames.
fn spawn_video(path: &Path, w: u32, h: u32, offset: f32) -> Option<(Child, Receiver<Frame>)> {
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error"])
        .arg("-ss")
        .arg(format!("{offset:.3}"))
        .arg("-re")
        .arg("-i")
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-an"])
        .arg("-vf")
        .arg(format!("scale={w}:{h}"))
        .arg("-r")
        .arg(FPS.to_string())
        .arg("-")
        .stdin(null())
        .stdout(Stdio::piped())
        .stderr(null())
        .spawn()
        .ok()?;

    let mut out = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    let frame_bytes = (w as usize) * (h as usize) * 4;
    std::thread::spawn(move || {
        let mut buf = vec![0u8; frame_bytes];
        loop {
            let mut filled = 0;
            let mut eof = false;
            while filled < frame_bytes {
                match out.read(&mut buf[filled..]) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(k) => filled += k,
                    Err(_) => {
                        eof = true;
                        break;
                    }
                }
            }
            if eof {
                break;
            }
            let frame = Frame {
                width: w,
                height: h,
                rgba: buf.clone(),
            };
            if tx.send(frame).is_err() {
                break;
            }
        }
    });
    Some((child, rx))
}

/// Scale `w`×`h` down so its width is at most `target`, keeping it even.
fn scaled(w: u32, h: u32, target: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        let tw = target.max(2) & !1;
        return (tw, (tw * 9 / 16).max(2) & !1);
    }
    let tw = (target.min(w).max(2)) & !1;
    let th = ((h as u64 * tw as u64 / w as u64) as u32).max(2) & !1;
    (tw, th)
}

// ------------------------------------------------------------ image layout

/// Per-attachment image layout: width in points (0 = fit to width) plus a free
/// offset from the default top-left position.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Layout {
    pub w: f32,
    pub ox: f32,
    pub oy: f32,
}

/// Persisted map of attachment path → layout, plus the playback volume.
#[derive(Serialize, Deserialize)]
pub struct Layouts {
    #[serde(default)]
    pub items: HashMap<String, Layout>,
    #[serde(default = "default_volume")]
    pub volume: f32,
}

fn default_volume() -> f32 {
    1.0
}

impl Default for Layouts {
    fn default() -> Self {
        Self {
            items: HashMap::new(),
            volume: 1.0,
        }
    }
}

impl Layouts {
    fn path() -> PathBuf {
        crate::config::dir().join("media-layout.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, s);
        }
    }

    pub fn get(&self, key: &str) -> Layout {
        self.items.get(key).copied().unwrap_or_default()
    }

    pub fn set(&mut self, key: &str, layout: Layout) {
        self.items.insert(key.to_string(), layout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_keeps_aspect_and_evenness() {
        let (w, h) = scaled(1920, 1080, 640);
        assert_eq!(w, 640);
        assert_eq!(h, 360);
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
    }

    #[test]
    fn scaled_does_not_upscale() {
        let (w, h) = scaled(320, 240, 640);
        assert_eq!(w, 320);
        assert_eq!(h, 240);
    }

    #[test]
    fn clamp_time_bounds_and_passes_through_unknown_duration() {
        assert_eq!(clamp_time(5.0, 3.0), 3.0);
        assert_eq!(clamp_time(-1.0, 3.0), 0.0);
        assert_eq!(clamp_time(4.0, 0.0), 4.0);
    }

    #[test]
    fn layout_roundtrips_through_json() {
        let mut layouts = Layouts::default();
        layouts.set(
            "/vault/assets/a.png",
            Layout {
                w: 320.0,
                ox: 12.0,
                oy: -8.0,
            },
        );
        layouts.volume = 0.5;
        let json = serde_json::to_string(&layouts).unwrap();
        let back: Layouts = serde_json::from_str(&json).unwrap();
        let l = back.get("/vault/assets/a.png");
        assert_eq!(l.w, 320.0);
        assert_eq!(l.ox, 12.0);
        assert_eq!(l.oy, -8.0);
        assert_eq!(back.volume, 0.5);
        assert!(back.get("/missing").w == 0.0);
    }

    #[test]
    fn layouts_default_to_full_volume() {
        let back: Layouts = serde_json::from_str("{}").unwrap();
        assert_eq!(back.volume, 1.0);
        assert_eq!(Layouts::default().volume, 1.0);
    }
}
