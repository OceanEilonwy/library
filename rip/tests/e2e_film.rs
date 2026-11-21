// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage 2 end to end, against the real library server: a film disc.
//!
//! A stand-in `makemkvcon` (`tests/data/fake-makemkvcon`) plays the drive
//! and writes a two-second MPEG-2 MKV; the real `library-rip headless-film`
//! binary scans it, copies the title, makes the WebM viewing copy with the
//! system `ffmpeg` (when it has AV1 and Opus), connects with a device code
//! the test approves as the librarian, and uploads. Then the test checks
//! the rip API and the Player API: the viewing copy plays (`video/webm`,
//! the right bytes, the right length), the lossless master, the log and
//! `rip.json` are archive files hidden from the Player.
//!
//! Needs a Unix shell for the stand-in. The server build and the skip
//! switch are as in `e2e_headless.rs` (`LIBRARY_SERVER_BIN`,
//! `LIBRARY_RIP_SKIP_E2E=1`).
#![cfg(unix)]
#![allow(clippy::too_many_lines)]

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};

const ADMIN_PASSWORD: &str = "librarian";
const READER_PASSWORD: &str = "listen-carefully";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_owned)
        .unwrap_or_else(|| PathBuf::from(".."))
}

/// The server binary: `LIBRARY_SERVER_BIN`, or built now. `Err` says why
/// the test cannot run.
fn server_binary() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("LIBRARY_SERVER_BIN") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "LIBRARY_SERVER_BIN={} does not exist",
                path.display()
            ))
        };
    }
    let root = repository_root();
    let target = std::env::var_os("LIBRARY_SERVER_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    eprintln!(
        "e2e: building the library server in {} (target {}); the first build takes a few minutes",
        root.display(),
        target.display()
    );
    let output = Command::new(cargo)
        .current_dir(&root)
        .args(["build", "--bin", "library", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        let tail = lines
            .get(lines.len().saturating_sub(60)..)
            .unwrap_or_default()
            .join("\n");
        return Err(format!(
            "`cargo build --bin library` failed ({}):\n{tail}",
            output.status
        ));
    }
    let binary = target
        .join("debug")
        .join(format!("library{}", std::env::consts::EXE_SUFFIX));
    if binary.is_file() {
        Ok(binary)
    } else {
        Err(format!("the build did not produce {}", binary.display()))
    }
}

/// Stops a child process when dropped.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// `library.dev.toml` with its own port and data folder.
fn write_config(dir: &Path, port: u16, data: &Path) -> PathBuf {
    let dev = std::fs::read_to_string(repository_root().join("library.dev.toml"))
        .expect("library.dev.toml is in the repository root");
    let mut config = String::new();
    for line in dev.lines() {
        if line.starts_with("listen ") || line.starts_with("listen=") {
            config.push_str(&format!("listen = \"127.0.0.1:{port}\"\n"));
        } else if line.starts_with("data_dir ") || line.starts_with("data_dir=") {
            // TOML basic strings: escape backslashes (Windows paths).
            let data = data.display().to_string().replace('\\', "\\\\");
            config.push_str(&format!("data_dir = \"{data}\"\n"));
        } else {
            config.push_str(line);
            config.push('\n');
        }
    }
    let path = dir.join("library.toml");
    std::fs::write(&path, config).unwrap();
    path
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}

async fn wait_for_server(base: &str, server: &mut Running) {
    let client = http();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(response) = client.get(format!("{base}/api/rip/info")).send().await
            && response.status().is_success()
        {
            return;
        }
        if let Ok(Some(status)) = server.0.try_wait() {
            panic!("the library server stopped early ({status})");
        }
        assert!(
            Instant::now() < deadline,
            "the library server did not answer"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The `name=value` part of a `Set-Cookie` header.
fn cookie(response: &reqwest::Response) -> String {
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Unlocks the site as the librarian; returns the session cookie.
async fn unlock(base: &str) -> String {
    let unlock = http()
        .post(format!("{base}/unlock"))
        .header("origin", base)
        .form(&[("password", ADMIN_PASSWORD), ("next", "/")])
        .send()
        .await
        .unwrap();
    assert_eq!(unlock.status(), 303, "unlocking as the librarian");
    let session = cookie(&unlock);
    assert!(!session.is_empty(), "the unlock sets a session cookie");
    session
}

/// Approves a device code as the librarian would on `/connect/device`.
async fn approve_device_code(base: &str, session: &str, code: &str) {
    let client = http();
    let confirm = client
        .post(format!("{base}/connect/device"))
        .header("origin", base)
        .header("cookie", session)
        .form(&[("code", code)])
        .send()
        .await
        .unwrap();
    assert_eq!(confirm.status(), 200, "the typed code is recognised");
    let page = confirm.text().await.unwrap();
    assert!(page.contains("Library Rip"), "{page}");

    let approve = client
        .post(format!("{base}/connect/device"))
        .header("origin", base)
        .header("cookie", session)
        .form(&[("code", code), ("decision", "yes")])
        .send()
        .await
        .unwrap();
    assert_eq!(approve.status(), 200, "pressing Yes, connect");
    let page = approve.text().await.unwrap();
    assert!(page.contains("is connected"), "{page}");
}

/// Reads the ripper's output line by line on a thread.
fn lines_of(child: &mut Child) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            println!("ripper: {line}");
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

/// The device code in a line such as "…type this code: XTMW-0166".
fn device_code(line: &str) -> Option<String> {
    let (_, code) = line.split_once("type this code: ")?;
    Some(code.trim().to_owned())
}

async fn wait_for_exit(child: &mut Child, limit: Duration) -> ExitStatus {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "the ripper did not finish");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The library server running on a free port with a fresh data folder.
struct Library {
    _server: Running,
    base: String,
}

async fn start_library(temp: &Path) -> Library {
    let server_bin = server_binary().unwrap_or_else(|why| {
        panic!(
            "e2e: the library server is not available (set LIBRARY_RIP_SKIP_E2E=1 to skip this test): {why}"
        )
    });
    let data = temp.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let config = write_config(temp, port, &data);
    let mut server = Running(
        Command::new(&server_bin)
            .arg("--config")
            .arg(&config)
            .arg("serve")
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for_server(&base, &mut server).await;
    Library {
        _server: server,
        base,
    }
}

fn sha256_hex(path: &Path) -> String {
    Sha256::digest(std::fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_film_disc_reaches_the_real_library_and_plays() {
    if std::env::var_os("LIBRARY_RIP_SKIP_E2E").is_some() {
        eprintln!("e2e: skipped (LIBRARY_RIP_SKIP_E2E is set)");
        return;
    }
    let viewing = library_rip::video::ffmpeg::Ffmpeg::discover().await.is_ok();
    if !viewing {
        eprintln!("e2e: no ffmpeg with AV1 and Opus here, so only the master is archived");
    }
    let temp = tempfile::tempdir().unwrap();
    let library = start_library(temp.path()).await;
    let base = library.base.clone();
    let session = unlock(&base).await;

    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let key_file = temp.path().join("connection.json");
    let keep = temp.path().join("keep");
    let mut command = Command::new(env!("CARGO_BIN_EXE_library-rip"));
    command
        .arg("headless-film")
        .args([
            "--library",
            &base,
            "--device-code",
            "--name",
            "The Library Card",
        ])
        .args(["--year", "1999"])
        .arg("--key-file")
        .arg(&key_file)
        .arg("--keep")
        .arg(&keep)
        .env("LIBRARY_RIP_MAKEMKVCON", data.join("fake-makemkvcon"))
        .env("FAKE_MAKEMKV_INFO", data.join("makemkv-info-short.txt"))
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("TMPDIR", temp.path())
        .stdout(Stdio::piped());
    if !viewing {
        command.arg("--master-only");
    }
    let mut process = Running(command.spawn().unwrap());
    let lines = lines_of(&mut process.0);
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    let code = loop {
        match lines.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                let code = device_code(&line);
                output.push(line);
                if let Some(code) = code {
                    break code;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                assert!(Instant::now() < deadline, "no code: {output:?}");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("stopped early: {output:?}"),
        }
    };
    approve_device_code(&base, &session, &code).await;
    let status = wait_for_exit(&mut process.0, Duration::from_secs(180)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    output.extend(lines.try_iter());
    let text = output.join("\n");
    assert!(status.success(), "the ripper failed ({status}):\n{text}");
    let expected = if viewing { 4 } else { 3 };
    assert!(
        text.contains(&format!("Done: {expected} files uploaded")),
        "{text}"
    );

    // What the library holds, through the ripper's own key.
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&key_file).unwrap()).unwrap();
    let key = saved["apiKey"].as_str().unwrap();
    let bearer = format!("Bearer {key}");
    let client = http();
    let found: Value = client
        .get(format!("{base}/api/rip/items"))
        .query(&[("q", "Library Card"), ("type", "film")])
        .header("authorization", &bearer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(found.as_array().unwrap().len(), 1, "{found}");
    let id = found[0]["id"].as_i64().unwrap();
    assert_eq!(found[0]["year"], 1999);
    let media: Value = client
        .get(format!("{base}/api/rip/items/{id}/media"))
        .header("authorization", &bearer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let media = media.as_array().unwrap();
    let summary: Vec<(String, String)> = media
        .iter()
        .map(|m| {
            (
                m["kind"].as_str().unwrap().to_owned(),
                m["title"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let mut want: Vec<(&str, &str)> = Vec::new();
    if viewing {
        want.push(("video", "The Library Card"));
    }
    want.extend([
        ("archive", "Master (lossless MKV): The Library Card"),
        ("archive", "Rip log"),
        ("archive", "Rip details (rip.json)"),
    ]);
    let want: Vec<(String, String)> = want
        .into_iter()
        .map(|(k, t)| (k.to_owned(), t.to_owned()))
        .collect();
    assert_eq!(summary, want);

    // Every SHA-256 is the one of the copy the ripper kept.
    let kept = keep.join("Films").join("The Library Card");
    let mut names = Vec::new();
    if viewing {
        names.push("The Library Card.webm");
    }
    names.extend([
        "The Library Card.mkv",
        "The Library Card.rip.log",
        "The Library Card.rip.json",
    ]);
    for (file, name) in media.iter().zip(&names) {
        assert_eq!(
            file["sha256"].as_str(),
            Some(sha256_hex(&kept.join(name)).as_str()),
            "{name}"
        );
    }
    let rip_json: Value = serde_json::from_str(
        &std::fs::read_to_string(kept.join("The Library Card.rip.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(rip_json["kind"], "film");
    assert_eq!(rip_json["disc"]["name"], "THE_LIBRARY_CARD");

    // The Player, with an approved adult account.
    let server_bin = server_binary().unwrap();
    let config = temp.path().join("library.toml");
    let made = Command::new(&server_bin)
        .arg("--config")
        .arg(&config)
        .args(["create-account", "reader", "reader@example.test", "--adult"])
        .env("LIBRARY_ACCOUNT_PASSWORD", READER_PASSWORD)
        .env("RUST_LOG", "warn")
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let login = client
        .post(format!("{base}/api/account/login"))
        .header("origin", &base)
        .json(&serde_json::json!({"login": "reader", "password": READER_PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let reader = cookie(&login);
    let item: Value = client
        .get(format!("{base}/api/items/{id}"))
        .header("cookie", &reader)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(item["title"], "The Library Card");
    let player_media = item["media"].as_array().unwrap();
    if viewing {
        assert_eq!(item["kinds"], serde_json::json!(["video"]));
        assert_eq!(player_media.len(), 1, "only the viewing copy: {item}");
        assert_eq!(player_media[0]["mime"], "video/webm");
        assert_eq!(player_media[0]["durationMs"].as_i64(), Some(2000));
        let video = media[0]["id"].as_i64().unwrap();
        let stream = client
            .get(format!("{base}/api/media/{video}/stream"))
            .header("cookie", &reader)
            .send()
            .await
            .unwrap();
        assert_eq!(stream.status(), 200);
        let bytes = stream.bytes().await.unwrap();
        // A WebM starts with the EBML header, and is what was uploaded.
        assert_eq!(bytes.get(..4), Some(&[0x1A, 0x45, 0xDF, 0xA3][..]));
        assert_eq!(
            Sha256::digest(&bytes).to_vec(),
            Sha256::digest(std::fs::read(kept.join("The Library Card.webm")).unwrap()).to_vec()
        );
    } else {
        assert!(
            player_media.is_empty(),
            "nothing to play without a viewing copy: {item}"
        );
    }
    for archive in media.iter().filter(|m| m["kind"] == "archive") {
        let archive = archive["id"].as_i64().unwrap();
        for path in [
            format!("{base}/api/media/{archive}"),
            format!("{base}/api/media/{archive}/stream"),
        ] {
            let response = client
                .get(&path)
                .header("cookie", &reader)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 404, "{path} is hidden from the Player");
        }
    }
    drop(library);
}
