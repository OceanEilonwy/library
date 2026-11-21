// SPDX-License-Identifier: AGPL-3.0-or-later
//! Finding and running the user's `makemkvcon`.
//!
//! MakeMKV is never bundled. It is found, in this order, through
//! `LIBRARY_RIP_MAKEMKVCON`, the `PATH` (`makemkvcon`, `makemkvcon64`), the
//! Flathub install (`flatpak run --command=makemkvcon com.makemkv.MakeMKV`),
//! and the places its installers use (`/usr/bin`, `/opt/makemkv/bin`, the
//! macOS app bundle, `Program Files` on Windows).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use tokio::process::Command;

use super::process;
use super::robot::{self, Line};
use super::scan::DiscScan;
use super::{Result, VideoError};

/// The Flathub application id.
pub const FLATPAK_ID: &str = "com.makemkv.MakeMKV";

/// A `makemkvcon` that can be run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MakeMkv {
    program: PathBuf,
    /// Arguments that come before MakeMKV's own (for `flatpak run …`).
    prefix: Vec<String>,
    /// Run through Flatpak: folders it writes to need to be shared.
    flatpak: bool,
}

/// One drive slot MakeMKV sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MakeMkvDrive {
    /// The index `disc:N` takes.
    pub index: u32,
    /// The drive's name.
    pub name: String,
    /// The disc in it.
    pub disc: String,
    /// The device.
    pub device: String,
}

/// Where a title's copy goes and how far along it is.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CopyProgress {
    /// 0.0–1.0 of this title.
    pub fraction: f32,
}

impl MakeMkv {
    /// A MakeMKV at `program` run directly.
    pub fn at(program: impl Into<PathBuf>) -> Self {
        MakeMkv {
            program: program.into(),
            prefix: Vec::new(),
            flatpak: false,
        }
    }

    /// How to say where it is, for the log.
    pub fn describe(&self) -> String {
        if self.flatpak {
            format!("Flatpak {FLATPAK_ID}")
        } else {
            self.program.display().to_string()
        }
    }

    fn command(&self, share: &[&Path]) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix);
        if self.flatpak {
            // MakeMKV reads the drive (/dev) and writes to the work folder.
            command.arg("--device=all");
            for folder in share {
                command.arg(format!("--filesystem={}", folder.display()));
            }
            command.args(["--command=makemkvcon", FLATPAK_ID]);
        }
        command
    }

    /// The drives MakeMKV sees (and the discs in them).
    pub async fn drives(&self, cancel: &AtomicBool) -> Result<Vec<MakeMkvDrive>> {
        let mut command = self.command(&[]);
        // Asking for a disc that cannot exist still makes it list the drives.
        command.args(["-r", "--cache=1", "info", "disc:9999"]);
        let mut found = Vec::new();
        process::run(command, "makemkvcon", cancel, |line| {
            if let Some(Line::Drive {
                index,
                visible,
                name,
                disc,
                device,
            }) = robot::parse_line(line)
                && visible < 256
                && !name.is_empty()
            {
                found.push(MakeMkvDrive {
                    index,
                    name,
                    disc,
                    device,
                });
            }
        })
        .await?;
        Ok(found)
    }

    /// Which `disc:N` is the drive at `device`: the drive whose device
    /// matches, else the only one with a disc in it.
    pub fn pick_drive<'a>(
        drives: &'a [MakeMkvDrive],
        device: &str,
    ) -> Result<&'a MakeMkvDrive> {
        let same = |a: &str, b: &str| {
            let a = a.trim_end_matches(['\\', '/']);
            let b = b.trim_end_matches(['\\', '/']);
            a.eq_ignore_ascii_case(b)
                || std::fs::canonicalize(a)
                    .ok()
                    .zip(std::fs::canonicalize(b).ok())
                    .is_some_and(|(x, y)| x == y)
        };
        if let Some(drive) = drives.iter().find(|d| same(&d.device, device)) {
            return Ok(drive);
        }
        let loaded: Vec<&MakeMkvDrive> = drives.iter().filter(|d| !d.disc.is_empty()).collect();
        match loaded.as_slice() {
            [only] => Ok(only),
            _ => Err(VideoError::NoMakeMkvDrive),
        }
    }

    /// Reads the disc's titles (this can take a minute).
    pub async fn scan(&self, index: u32, cancel: &AtomicBool) -> Result<DiscScan> {
        let mut command = self.command(&[]);
        command.args(["-r", "--cache=128", "info"]);
        command.arg(format!("disc:{index}"));
        let mut lines = Vec::new();
        let mut messages = Messages::default();
        let done = process::run(command, "makemkvcon", cancel, |line| {
            messages.see(line);
            lines.push(line.to_owned());
        })
        .await?;
        let scan = DiscScan::from_lines(lines.iter().map(String::as_str));
        if scan.titles.is_empty() {
            if !done.status.success() || !messages.problems().is_empty() {
                return Err(VideoError::MakeMkv(messages.problems()));
            }
            return Err(VideoError::NoTitles);
        }
        Ok(scan)
    }

    /// Copies one title into `folder` as an MKV, reporting progress. Returns
    /// the file MakeMKV wrote.
    pub async fn copy_title(
        &self,
        index: u32,
        title: usize,
        folder: &Path,
        cancel: &AtomicBool,
        progress: &mut (dyn FnMut(CopyProgress) + Send),
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(folder).map_err(|source| VideoError::Io {
            path: folder.to_owned(),
            source,
        })?;
        let before = mkv_files(folder);
        let mut command = self.command(&[folder]);
        command.args(["-r", "--progress=-same", "--cache=256", "mkv"]);
        command.arg(format!("disc:{index}"));
        command.arg(title.to_string());
        command.arg(folder);
        let mut messages = Messages::default();
        let done = process::run(command, "makemkvcon", cancel, |line| {
            messages.see(line);
            if let Some(Line::Progress { current, max, .. }) = robot::parse_line(line)
                && max > 0
            {
                progress(CopyProgress {
                    fraction: (current as f64 / max as f64).clamp(0.0, 1.0) as f32,
                });
            }
        })
        .await?;
        let wrote = mkv_files(folder)
            .into_iter()
            .find(|path| !before.contains(path));
        match wrote {
            Some(path) if done.status.success() || messages.saved => Ok(path),
            _ if !done.status.success() || !messages.problems().is_empty() => {
                Err(VideoError::MakeMkv(messages.problems()))
            }
            _ => Err(VideoError::NoOutput(title)),
        }
    }

    /// `makemkvcon`'s version line, for the rip log (best effort).
    pub async fn version(&self, cancel: &AtomicBool) -> Option<String> {
        let mut command = self.command(&[]);
        command.args(["-r", "--cache=1", "info", "disc:9999"]);
        let mut version = None;
        process::run(command, "makemkvcon", cancel, |line| {
            if let Some(Line::Msg { code: 1005, text }) = robot::parse_line(line) {
                version = Some(text.replace(" started", ""));
            }
        })
        .await
        .ok()?;
        version
    }
}

fn mkv_files(folder: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mkv")))
        .collect();
    files.sort();
    files
}

/// Messages worth showing when MakeMKV fails.
#[derive(Default)]
struct Messages {
    all: Vec<(u32, String)>,
    /// "Copy complete" was said.
    saved: bool,
}

impl Messages {
    fn see(&mut self, line: &str) {
        if let Some(Line::Msg { code, text }) = robot::parse_line(line) {
            // 5036: "Copy complete. N titles saved."
            if code == 5036 {
                self.saved = true;
            }
            self.all.push((code, text));
        }
    }

    /// Plain-English lines about what went wrong: everything MakeMKV said
    /// apart from its start-up chatter, the last few of them.
    fn problems(&self) -> Vec<String> {
        let noisy = [1005, 5074, 3007, 3002, 3025, 3028, 5011, 5036, 5037];
        let mut lines: Vec<String> = self
            .all
            .iter()
            .filter(|(code, text)| !noisy.contains(code) && *code >= 2000 && !text.is_empty())
            .map(|(_, text)| text.clone())
            .collect();
        lines.dedup();
        let skip = lines.len().saturating_sub(4);
        lines.split_off(skip)
    }
}

/// Finds `makemkvcon` on this computer.
pub async fn discover() -> Option<MakeMkv> {
    let env = std::env::var_os("LIBRARY_RIP_MAKEMKVCON").map(PathBuf::from);
    discover_with(env, std::env::var_os("PATH"), &standard_places(), flatpak_installed).await
}

/// [`discover`] with the places to look given, for tests.
pub async fn discover_with<F, Fut>(
    env: Option<PathBuf>,
    path: Option<std::ffi::OsString>,
    places: &[PathBuf],
    flatpak: F,
) -> Option<MakeMkv>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    if let Some(program) = env.filter(|p| !p.as_os_str().is_empty()) {
        return Some(MakeMkv::at(program));
    }
    if let Some(path) = path {
        for dir in std::env::split_paths(&path) {
            for name in executable_names() {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(MakeMkv::at(candidate));
                }
            }
        }
    }
    if flatpak().await {
        return Some(MakeMkv {
            program: PathBuf::from("flatpak"),
            prefix: vec!["run".to_owned()],
            flatpak: true,
        });
    }
    places
        .iter()
        .find(|p| p.is_file())
        .map(|p| MakeMkv::at(p.clone()))
}

fn executable_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["makemkvcon64.exe", "makemkvcon.exe"]
    } else {
        &["makemkvcon", "makemkvcon64"]
    }
}

fn standard_places() -> Vec<PathBuf> {
    let mut places = vec![
        PathBuf::from("/usr/bin/makemkvcon"),
        PathBuf::from("/usr/local/bin/makemkvcon"),
        PathBuf::from("/opt/makemkv/bin/makemkvcon"),
        PathBuf::from("/Applications/MakeMKV.app/Contents/MacOS/makemkvcon"),
    ];
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Some(dir) = std::env::var_os(var) {
            let dir = PathBuf::from(dir).join("MakeMKV");
            places.push(dir.join("makemkvcon64.exe"));
            places.push(dir.join("makemkvcon.exe"));
        }
    }
    places
}

async fn flatpak_installed() -> bool {
    if cfg!(windows) {
        return false;
    }
    let mut command = Command::new("flatpak");
    command
        .args(["info", FLATPAK_ID])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    matches!(command.status().await, Ok(status) if status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn no_flatpak() -> bool {
        false
    }

    #[tokio::test]
    async fn the_environment_wins() {
        let found = discover_with(Some("/x/mk".into()), Some("/nowhere".into()), &[], no_flatpak).await;
        assert_eq!(found, Some(MakeMkv::at("/x/mk")));
    }

    #[tokio::test]
    async fn the_path_is_searched_then_flatpak_then_the_usual_places() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join(executable_names()[0]);
        std::fs::write(&program, "").unwrap();
        let found = discover_with(None, Some(dir.path().into()), &[], no_flatpak).await;
        assert_eq!(found, Some(MakeMkv::at(&program)));

        let found = discover_with(None, None, &[], || async { true }).await.unwrap();
        assert!(found.flatpak);
        assert_eq!(found.describe(), "Flatpak com.makemkv.MakeMKV");

        let found = discover_with(None, None, std::slice::from_ref(&program), no_flatpak).await;
        assert_eq!(found, Some(MakeMkv::at(&program)));
        assert_eq!(discover_with(None, None, &[], no_flatpak).await, None);
    }

    #[test]
    fn flatpak_is_given_the_drive_and_the_work_folder() {
        let mk = MakeMkv {
            program: "flatpak".into(),
            prefix: vec!["run".into()],
            flatpak: true,
        };
        let command = mk.command(&[Path::new("/work/rip")]);
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "run",
                "--device=all",
                "--filesystem=/work/rip",
                "--command=makemkvcon",
                "com.makemkv.MakeMKV"
            ]
        );
    }

    fn drive(index: u32, disc: &str, device: &str) -> MakeMkvDrive {
        MakeMkvDrive {
            index,
            name: "DVD".into(),
            disc: disc.into(),
            device: device.into(),
        }
    }

    #[test]
    fn the_drive_is_matched_by_device_or_by_being_the_only_disc() {
        let drives = [drive(0, "", "/dev/sr1"), drive(1, "FILM", "/dev/sr0")];
        assert_eq!(MakeMkv::pick_drive(&drives, "/dev/sr0").unwrap().index, 1);
        // An unfamiliar device name: the one loaded drive is the answer.
        assert_eq!(MakeMkv::pick_drive(&drives, "/dev/cdrom").unwrap().index, 1);
        let two = [drive(0, "A", "/dev/sr1"), drive(1, "B", "/dev/sr2")];
        assert!(MakeMkv::pick_drive(&two, "/dev/sr0").is_err());
        assert_eq!(MakeMkv::pick_drive(&two, "/dev/sr2").unwrap().index, 1);
        let win = [drive(0, "FILM", "D:")];
        assert_eq!(MakeMkv::pick_drive(&win, "d:").unwrap().index, 0);
    }

    #[test]
    fn failures_are_quoted_without_the_start_up_chatter() {
        let mut m = Messages::default();
        for line in [
            r#"MSG:1005,0,1,"MakeMKV v2.0.0 started","x""#,
            r#"MSG:5074,0,0,"updates","x""#,
            r#"MSG:5010,0,0,"Failed to open disc","x""#,
            r#"MSG:5010,0,0,"Failed to open disc","x""#,
        ] {
            m.see(line);
        }
        assert_eq!(m.problems(), ["Failed to open disc"]);
        assert!(!m.saved);
        m.see(r#"MSG:5036,0,1,"Copy complete. 1 titles saved.","x""#);
        assert!(m.saved);
    }
}
