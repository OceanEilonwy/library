// SPDX-License-Identifier: AGPL-3.0-or-later
//! The viewing copy: the user's `ffmpeg` turns the lossless MKV into a WebM
//! with AV1 video and Opus sound, which every current browser plays.
//!
//! `ffmpeg` is found through `LIBRARY_RIP_FFMPEG` or the `PATH`. It must
//! have an AV1 encoder (`libsvtav1`, else `libaom-av1`) and `libopus`; there
//! is no H.264 or MP4 fallback.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use tokio::process::Command;

use super::process;
use super::{Result, VideoError};

/// An `ffmpeg` that can make viewing copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ffmpeg {
    program: PathBuf,
    /// `libsvtav1` or `libaom-av1`.
    pub video_encoder: &'static str,
    /// Its first version line, for the rip log.
    pub version: String,
}

/// The tallest picture a viewing copy has (never scaled up).
pub const MAX_WIDTH: u32 = 1920;
/// The tallest picture a viewing copy has (never scaled up).
pub const MAX_HEIGHT: u32 = 1080;

impl Ffmpeg {
    /// Finds `ffmpeg` and checks its encoders.
    pub async fn discover() -> Result<Ffmpeg> {
        let program = find_program(
            std::env::var_os("LIBRARY_RIP_FFMPEG").map(PathBuf::from),
            std::env::var_os("PATH"),
        )
        .ok_or(VideoError::FfmpegMissing)?;
        Ffmpeg::check(program).await
    }

    /// Checks the encoders of the `ffmpeg` at `program`.
    pub async fn check(program: PathBuf) -> Result<Ffmpeg> {
        let output = Command::new(&program)
            .args(["-hide_banner", "-encoders"])
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|source| VideoError::Start {
                program: program.display().to_string(),
                source,
            })?;
        let encoders = String::from_utf8_lossy(&output.stdout).into_owned();
        let video_encoder = choose_encoder(&encoders)?;
        let version = Command::new(&program)
            .arg("-version")
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "ffmpeg".to_owned());
        Ok(Ffmpeg {
            program,
            video_encoder,
            version,
        })
    }

    /// The arguments that make `output` from `input` (everything but the
    /// program), for a title called `title`.
    pub fn arguments(&self, input: &Path, output: &Path, title: &str) -> Vec<String> {
        let mut args: Vec<String> = [
            "-hide_banner",
            "-nostdin",
            "-y",
            "-loglevel",
            "error",
            "-progress",
            "pipe:1",
            "-nostats",
            "-i",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        args.push(input.display().to_string());
        args.extend(
            ["-map", "0:v:0", "-map", "0:a?", "-sn", "-dn", "-map_metadata", "-1", "-vf"]
                .into_iter()
                .map(str::to_owned),
        );
        args.push(video_filter());
        if self.video_encoder == "libsvtav1" {
            args.extend(
                ["-c:v", "libsvtav1", "-crf", "30", "-preset", "6", "-g", "240"]
                    .into_iter()
                    .map(str::to_owned),
            );
        } else {
            args.extend(
                [
                    "-c:v", "libaom-av1", "-crf", "32", "-b:v", "0", "-cpu-used", "6", "-row-mt",
                    "1", "-g", "240",
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        args.extend(
            ["-pix_fmt", "yuv420p", "-c:a", "libopus", "-b:a", "128k", "-ac", "2", "-metadata"]
                .into_iter()
                .map(str::to_owned),
        );
        args.push(format!("title={title}"));
        args.push(output.display().to_string());
        args
    }

    /// Makes the viewing copy. `duration_secs` (the title's length) turns
    /// ffmpeg's position into progress.
    pub async fn make_viewing_copy(
        &self,
        input: &Path,
        output: &Path,
        title: &str,
        duration_secs: u64,
        cancel: &AtomicBool,
        progress: &mut (dyn FnMut(f32) + Send),
    ) -> Result<()> {
        let mut command = Command::new(&self.program);
        command.args(self.arguments(input, output, title));
        let done = process::run(command, "ffmpeg", cancel, |line| {
            if let Some(fraction) = progress_fraction(line, duration_secs) {
                progress(fraction);
            }
        })
        .await?;
        if !done.status.success() || !output.is_file() {
            let _ = std::fs::remove_file(output);
            let why = if done.stderr.is_empty() {
                format!("it stopped with {}", done.status)
            } else {
                done.stderr.join(" ")
            };
            return Err(VideoError::Encode(why));
        }
        Ok(())
    }
}

/// The filter that straightens a DVD's picture and keeps it within 1080p:
/// only frames the disc marks as interlaced are de-interlaced; non-square
/// pixels become square (a 720×480 16:9 DVD becomes 854×480); nothing is
/// scaled up; both sides stay even for the encoder.
pub fn video_filter() -> String {
    format!(
        "yadif=deint=interlaced,scale=trunc(iw*sar/2)*2:trunc(ih/2)*2,setsar=1,\
         scale=w=min(iw\\,{MAX_WIDTH}):h=min(ih\\,{MAX_HEIGHT}):\
         force_original_aspect_ratio=decrease:force_divisible_by=2"
    )
}

/// The best AV1 encoder in `ffmpeg -encoders`, if there is one and Opus too.
pub fn choose_encoder(encoders: &str) -> Result<&'static str> {
    let has = |name: &str| {
        encoders
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some(name))
    };
    if !has("libopus") {
        return Err(VideoError::NoEncoders(
            "it cannot write Opus sound (libopus)".to_owned(),
        ));
    }
    if has("libsvtav1") {
        Ok("libsvtav1")
    } else if has("libaom-av1") {
        Ok("libaom-av1")
    } else {
        Err(VideoError::NoEncoders(
            "it cannot write AV1 video (libsvtav1 or libaom-av1)".to_owned(),
        ))
    }
}

/// Progress from one line of `-progress pipe:1` output.
pub fn progress_fraction(line: &str, duration_secs: u64) -> Option<f32> {
    let micros = line
        .strip_prefix("out_time_us=")
        .or_else(|| line.strip_prefix("out_time_ms="))?
        .trim()
        .parse::<i64>()
        .ok()?;
    if duration_secs == 0 || micros < 0 {
        return None;
    }
    Some((micros as f64 / (duration_secs as f64 * 1e6)).clamp(0.0, 1.0) as f32)
}

fn find_program(env: Option<PathBuf>, path: Option<std::ffi::OsString>) -> Option<PathBuf> {
    if let Some(program) = env.filter(|p| !p.as_os_str().is_empty()) {
        return Some(program);
    }
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    std::env::split_paths(&path?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENCODERS: &str = " V....D libaom-av1           libaom AV1 (codec av1)\n \
        V....D libsvtav1            SVT-AV1(Scalable Video Technology for AV1) encoder (codec av1)\n \
        A....D libopus              libopus Opus (codec opus)\n";

    #[test]
    fn svt_is_preferred_and_opus_is_required() {
        assert_eq!(choose_encoder(ENCODERS).unwrap(), "libsvtav1");
        let aom_only: String = ENCODERS.lines().filter(|l| !l.contains("svtav1")).collect::<Vec<_>>().join("\n");
        assert_eq!(choose_encoder(&aom_only).unwrap(), "libaom-av1");
        let no_opus: String = ENCODERS.lines().filter(|l| !l.contains("libopus")).collect::<Vec<_>>().join("\n");
        assert!(matches!(choose_encoder(&no_opus), Err(VideoError::NoEncoders(_))));
        let no_av1: String = ENCODERS.lines().filter(|l| l.contains("libopus")).collect::<Vec<_>>().join("\n");
        assert!(matches!(choose_encoder(&no_av1), Err(VideoError::NoEncoders(_))));
    }

    #[test]
    fn progress_is_the_position_over_the_length() {
        assert_eq!(progress_fraction("out_time_us=30000000", 60), Some(0.5));
        assert_eq!(progress_fraction("out_time_ms=60000000", 60), Some(1.0));
        assert_eq!(progress_fraction("out_time_us=999999999999", 60), Some(1.0));
        assert_eq!(progress_fraction("out_time_us=-9223372036854775807", 60), None);
        assert_eq!(progress_fraction("out_time_us=5", 0), None);
        assert_eq!(progress_fraction("bitrate=1000kbits/s", 60), None);
    }

    #[test]
    fn the_command_makes_webm_av1_opus_never_larger_than_1080p() {
        let ff = Ffmpeg {
            program: "ffmpeg".into(),
            video_encoder: "libsvtav1",
            version: String::new(),
        };
        let args = ff.arguments(Path::new("in.mkv"), Path::new("out.webm"), "A Film");
        let joined = args.join(" ");
        assert!(joined.contains("-c:v libsvtav1"));
        assert!(joined.contains("-c:a libopus"));
        assert!(joined.contains("-sn"));
        assert!(joined.ends_with("title=A Film out.webm"));
        let filter = &args[args.iter().position(|a| a == "-vf").unwrap() + 1];
        assert!(filter.contains("setsar=1"));
        assert!(filter.contains("min(iw\\,1920)"));
        assert!(filter.contains("min(ih\\,1080)"));
        assert!(!joined.contains("libx264"));
    }
}
