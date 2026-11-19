// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage 1 end to end, against the real library server.
//!
//! 1. builds the server in the repository root (`cargo build --bin
//!    library`) and starts it on a free port with a copy of
//!    `library.dev.toml` whose `data_dir` is a fresh temporary folder;
//! 2. makes a three-track simulated disc with a `jitter` and a
//!    `flaky_once` sector, and starts the mock MusicBrainz, Cover Art
//!    Archive, AccurateRip and CTDB for it ([`library_rip::mockservices`]);
//! 3. runs the real `library-rip headless --device-code --test-and-copy`
//!    binary, and when it prints the code, approves it as the librarian
//!    (`POST /unlock`, then `POST /connect/device` twice, as the browser
//!    would);
//! 4. checks what the library now holds, through the rip API (with the
//!    ripper's own key) and the Player API (with an account made by
//!    `library create-account`).
//!
//! The server build can take a few minutes the first time. Set
//! `LIBRARY_SERVER_BIN` to use a server binary you already built, or
//! `LIBRARY_RIP_SKIP_E2E=1` to skip. If the server cannot be built (or
//! `LIBRARY_SERVER_BIN` is missing) the test fails with the build error;
//! only `LIBRARY_RIP_SKIP_E2E` skips it.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use library_rip::fakedisc::{InjectedError, InjectedErrorKind};
use library_rip::mockservices;
use library_rip::model::{Lba, SampleOffset};
use library_rip::testsupport::{FakeDiscOptions, make_fake_disc};
use serde_json::Value;
use sha2::{Digest, Sha256};

const ADMIN_PASSWORD: &str = "librarian";
const READER_PASSWORD: &str = "listen-carefully";
const SECONDS_PER_TRACK: u32 = 4;

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

/// Plays the librarian's browser on the connect page the ripper printed:
/// opens it unlocked, presses **Yes, connect**, and follows the redirect
/// to the ripper's loopback listener.
async fn approve_in_browser(base: &str, session: &str, connect_url: &str) {
    let client = http();
    let page = client
        .get(connect_url)
        .header("cookie", session)
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200, "the connect page opens");
    let text = page.text().await.unwrap();
    assert!(text.contains("Yes, connect"), "{text}");

    let url = url::Url::parse(connect_url).unwrap();
    let mut form: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    form.push(("decision".into(), "yes".into()));
    let answer = client
        .post(format!("{base}/connect"))
        .header("origin", base)
        .header("cookie", session)
        .form(&form)
        .send()
        .await
        .unwrap();
    assert_eq!(
        answer.status(),
        302,
        "Yes sends the browser back to the ripper"
    );
    let location = answer
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_owned();
    let back = url::Url::parse(&location).unwrap();
    assert_eq!(back.host_str(), Some("127.0.0.1"), "{location}");
    let pairs: std::collections::HashMap<String, String> =
        back.query_pairs().into_owned().collect();
    assert!(pairs.contains_key("code"), "{location}");
    assert_eq!(
        pairs.get("state"),
        url.query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .as_ref()
    );
    let landing = client.get(location).send().await.unwrap();
    assert_eq!(landing.status(), 200, "the ripper's tab page");
}

/// A running `library-rip` and what it has printed so far.
struct Ripper {
    process: Running,
    lines: mpsc::Receiver<String>,
    output: Vec<String>,
}

impl Ripper {
    fn start(
        arguments: &[&std::ffi::OsStr],
        mock: &mockservices::MockServices,
        temp: &Path,
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_library-rip"));
        command
            .arg("headless")
            .args(arguments)
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("TMPDIR", temp)
            .stdout(Stdio::piped());
        for (name, value) in mock.environment() {
            command.env(name, value);
        }
        let mut process = Running(command.spawn().unwrap());
        let lines = lines_of(&mut process.0);
        Ripper {
            process,
            lines,
            output: Vec::new(),
        }
    }

    /// Waits for a line `pick` accepts and returns what it picked.
    fn wait_for<T>(&mut self, pick: impl Fn(&str) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match self.lines.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    let picked = pick(&line);
                    self.output.push(line);
                    if let Some(picked) = picked {
                        return picked;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    assert!(
                        Instant::now() < deadline,
                        "waited too long: {:?}",
                        self.output
                    );
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the ripper stopped early: {:?}", self.output)
                }
            }
        }
    }

    /// Waits for the ripper to exit successfully; returns all it printed.
    async fn finish(mut self) -> String {
        let status = wait_for_exit(&mut self.process.0, Duration::from_secs(180)).await;
        // The reader thread may still hold the last lines briefly.
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.output.extend(self.lines.try_iter());
        let text = self.output.join("\n");
        assert!(status.success(), "the ripper failed ({status}):\n{text}");
        text
    }
}

fn saved_key_id(path: &Path) -> i64 {
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    saved["keyId"].as_i64().unwrap()
}

fn sha256_hex(path: &Path) -> String {
    Sha256::digest(std::fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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

#[tokio::test(flavor = "multi_thread")]
async fn headless_rip_reaches_the_real_library() {
    if std::env::var_os("LIBRARY_RIP_SKIP_E2E").is_some() {
        eprintln!("e2e: skipped (LIBRARY_RIP_SKIP_E2E is set)");
        return;
    }
    // Only an explicit LIBRARY_RIP_SKIP_E2E skips: a server that does not
    // build is a failure, never a silent pass.
    let server_bin = server_binary().unwrap_or_else(|why| {
        panic!(
            "e2e: the library server is not available (set LIBRARY_RIP_SKIP_E2E=1 to skip this test): {why}"
        )
    });

    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let config = write_config(temp.path(), port, &data);

    // The library.
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

    // The disc, with one jittery sector in track 1 and one that reads
    // wrong once in track 2.
    let disc_dir = temp.path().join("disc");
    let sectors_per_track = SECONDS_PER_TRACK * 75;
    make_fake_disc(
        &disc_dir,
        &FakeDiscOptions {
            tracks: 3,
            seconds: SECONDS_PER_TRACK,
            read_offset: SampleOffset(6),
            errors: vec![
                InjectedError {
                    lba: Lba(100),
                    kind: InjectedErrorKind::Jitter,
                },
                InjectedError {
                    lba: Lba(sectors_per_track + 100),
                    kind: InjectedErrorKind::FlakyOnce,
                },
            ],
        },
    )
    .unwrap();

    // MusicBrainz, the Cover Art Archive, AccurateRip and CTDB.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = mockservices::start(&disc_dir, listener).await.unwrap();

    // The ripper, connecting with a code typed on the library.
    let key_file = temp.path().join("connection.json");
    let keep = temp.path().join("keep");
    let session = unlock(&base).await;
    let mut ripper = Ripper::start(
        &[
            "--library".as_ref(),
            base.as_ref(),
            "--device-code".as_ref(),
            "--test-and-copy".as_ref(),
            "--fake-disc".as_ref(),
            disc_dir.as_os_str(),
            "--key-file".as_ref(),
            key_file.as_os_str(),
            "--keep".as_ref(),
            keep.as_os_str(),
        ],
        &mock,
        temp.path(),
    );
    let code = ripper.wait_for(device_code);
    approve_device_code(&base, &session, &code).await;
    let text = ripper.finish().await;

    // What the ripper said.
    assert!(
        text.contains("Album: The Test Card - Sine Tones."),
        "{text}"
    );
    assert!(
        text.contains("Read offset: +6 samples (from the AccurateRip drive list)."),
        "{text}"
    );
    for track in 1..=3 {
        assert!(
            text.contains(&format!(
                "Track {track}: read without errors, test and copy match."
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "Track {track}: AccurateRip: Accurately ripped (confidence 12)"
            )),
            "{text}"
        );
    }
    assert!(
        text.contains("CUETools database: Accurately ripped (confidence 7 of 7)"),
        "{text}"
    );
    assert!(
        text.contains("Done: 6 files uploaded (0 already there)"),
        "{text}"
    );

    // The rip API, with the ripper's own key.
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&key_file).unwrap()).unwrap();
    let key = saved["apiKey"]
        .as_str()
        .expect("the key is saved")
        .to_owned();
    assert!(key.starts_with("lrk_"));
    let client = http();
    let bearer = format!("Bearer {key}");
    let found: Value = client
        .get(format!("{base}/api/rip/items"))
        .query(&[("q", "Sine Tones"), ("type", "music")])
        .header("authorization", &bearer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let items = found.as_array().unwrap();
    assert_eq!(items.len(), 1, "{found}");
    let item = &items[0];
    assert_eq!(item["title"], "Sine Tones");
    assert_eq!(item["creator"], "The Test Card");
    assert_eq!(item["year"], 2024);
    // The rip API counts every file, archive files included.
    assert_eq!(item["fileCount"], 6);
    let id = item["id"].as_i64().unwrap();
    assert!(text.contains(&format!("{base}/items/{id}")), "{text}");

    let media: Value = client
        .get(format!("{base}/api/rip/items/{id}/media"))
        .header("authorization", &bearer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let media = media.as_array().unwrap().clone();
    let summary: Vec<(String, String, Option<i64>)> = media
        .iter()
        .map(|m| {
            (
                m["kind"].as_str().unwrap().to_owned(),
                m["title"].as_str().unwrap().to_owned(),
                m["track"].as_i64(),
            )
        })
        .collect();
    let expected: Vec<(String, String, Option<i64>)> = [
        ("audio", "A 4", Some(1)),
        ("audio", "B flat 4", Some(2)),
        ("audio", "B 4", Some(3)),
        ("archive", "Rip log", None),
        ("archive", "Cue sheet", None),
        ("archive", "Rip report (rip.json)", None),
    ]
    .into_iter()
    .map(|(k, t, n)| (k.to_owned(), t.to_owned(), n))
    .collect();
    assert_eq!(summary, expected);

    // Every file's SHA-256 is recorded and matches the kept copy.
    let kept = keep.join("The Test Card").join("Sine Tones");
    let kept_files = [
        "01 - A 4.flac",
        "02 - B flat 4.flac",
        "03 - B 4.flac",
        "The Test Card - Sine Tones.log",
        "The Test Card - Sine Tones.cue",
        "rip.json",
    ];
    for (file, name) in media.iter().zip(kept_files) {
        assert_eq!(
            file["sha256"].as_str(),
            Some(sha256_hex(&kept.join(name)).as_str()),
            "{name}"
        );
    }
    // The log says what the rip found.
    let log = std::fs::read_to_string(kept.join("The Test Card - Sine Tones.log")).unwrap();
    assert!(log.contains("All tracks accurately ripped."), "{log}");
    // The injected jitter (track 1) and flaky sector (track 2) were caught
    // and corrected by re-reading, and the log says so.
    let report = library_rip::core::report_json::parse(
        &std::fs::read_to_string(kept.join("rip.json")).unwrap(),
    )
    .unwrap();
    let retried: Vec<u32> = report.tracks.iter().map(|t| t.retried_sectors).collect();
    assert!(retried[0] >= 1 && retried[1] >= 1, "{retried:?}");
    assert_eq!(retried[2], 0, "{retried:?}");
    assert!(report.read_without_errors());
    assert!(log.contains("came right on re-reading"), "{log}");

    // The Player, with an approved adult account.
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
    let player_item: Value = client
        .get(format!("{base}/api/items/{id}"))
        .header("cookie", &reader)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(player_item["title"], "Sine Tones");
    assert_eq!(player_item["kinds"], serde_json::json!(["audio"]));
    assert!(
        player_item["cover"]
            .as_str()
            .is_some_and(|c| c.starts_with("/photos/")),
        "the cover photo is attached: {player_item}"
    );
    let player_media = player_item["media"].as_array().unwrap();
    assert_eq!(player_media.len(), 3, "only the audio: {player_item}");
    for (index, file) in player_media.iter().enumerate() {
        assert_eq!(file["kind"], "audio");
        assert_eq!(file["mime"], "audio/flac");
        assert_eq!(file["track"].as_i64(), i64::try_from(index + 1).ok());
        assert_eq!(
            file["durationMs"].as_i64(),
            Some(i64::from(SECONDS_PER_TRACK) * 1000)
        );
    }
    let audio = media[0]["id"].as_i64().unwrap();
    let stream = client
        .get(format!("{base}/api/media/{audio}/stream"))
        .header("cookie", &reader)
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), 200);
    let flac = stream.bytes().await.unwrap();
    assert_eq!(flac.get(..4), Some(&b"fLaC"[..]));
    for archive in &media[3..] {
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

    // A second ripper connects through the browser handshake (loopback
    // listener + PKCE) and adds the disc again as a new entry.
    let second_key_file = temp.path().join("second-connection.json");
    let mut second = Ripper::start(
        &[
            "--library".as_ref(),
            base.as_ref(),
            "--fake-disc".as_ref(),
            disc_dir.as_os_str(),
            "--key-file".as_ref(),
            second_key_file.as_os_str(),
        ],
        &mock,
        temp.path(),
    );
    let connect_url = second.wait_for(|line| {
        line.trim()
            .starts_with(&format!("{base}/connect?"))
            .then(|| line.trim().to_owned())
    });
    approve_in_browser(&base, &session, &connect_url).await;
    let text = second.finish().await;
    assert!(text.contains("Done: 6 files uploaded"), "{text}");

    // Disconnecting revokes the key: the library refuses it afterwards.
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&second_key_file).unwrap()).unwrap();
    let second_key =
        library_rip::library::api::ApiKey::new(saved["apiKey"].as_str().unwrap()).unwrap();
    let library = library_rip::library::client::LibraryClient::new(
        http(),
        url::Url::parse(&base).unwrap(),
        Some(second_key),
    );
    let who = library.whoami().await.unwrap();
    assert_ne!(who.key_id, saved_key_id(&key_file), "a key per connection");
    library.revoke().await.unwrap();
    assert!(matches!(
        library.whoami().await,
        Err(library_rip::library::LibraryError::Unauthorised)
    ));
    // The first ripper's key still works.
    let still = client
        .get(format!("{base}/api/rip/whoami"))
        .header("authorization", &bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(still.status(), 200);

    drop(mock);
    drop(server);
}
