#!/usr/bin/env python3
"""Fill a development data folder with demo items, digital copies, accounts
and playlists, so the Player has something to play.

    scripts/dev-seed.py                       # uses library.dev.toml
    scripts/dev-seed.py --config other.toml --bin target/debug/library

It WIPES the config's data_dir first (it refuses a folder called "data",
which is where real catalogues live), then:

  * makes two Player accounts with the CLI: adult / password123 (adult) and
    kid / password123 (child);
  * starts the server on a spare port, unlocks editing with the admin
    password ("librarian"), and adds four items with generated files,
    uploading each file through the real upload studio endpoint:
      - "Sine Songs" (3-track album: FLAC, MP3, Opus)
      - "Late Night Tones" (2-track album, Restricted: adults only)
      - "Test Pattern" (a short film with English and French subtitles)
      - "A Tiny Book" (a one-page PDF)
  * makes playlists through the JSON API, including ones that mix in or
    consist only of restricted tracks, and a few favourites;
  * stops the server again.

Sample files are made with ffmpeg when it is installed; otherwise with the
Python standard library (WAV tones, and no film).
"""

from __future__ import annotations

import argparse
import http.cookiejar
import json
import math
import os
import shutil
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import uuid
import wave
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
ADMIN_PASSWORD = "librarian"
ACCOUNT_PASSWORD = "password123"
TONE_SECONDS = 30
FILM_SECONDS = 12


def say(message: str) -> None:
    print(f"seed: {message}", flush=True)


def fail(message: str) -> "None":
    print(f"seed: {message}", file=sys.stderr, flush=True)
    sys.exit(1)


# ---------- the server binary ----------


def find_binary(explicit: str | None) -> Path:
    if explicit:
        path = Path(explicit).resolve()
        if not path.is_file():
            fail(f"there is no library binary at {path}")
        return path
    say("building the server (cargo build)…")
    subprocess.run(["cargo", "build", "--quiet"], cwd=REPO, check=True)
    meta = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=REPO,
        check=True,
        capture_output=True,
        text=True,
    )
    target = Path(json.loads(meta.stdout)["target_directory"])
    return target / "debug" / "library"


def run_cli(binary: Path, config: Path, *args: str) -> None:
    env = dict(os.environ, LIBRARY_ACCOUNT_PASSWORD=ACCOUNT_PASSWORD)
    subprocess.run([str(binary), "--config", str(config), *args], cwd=REPO, env=env, check=True)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def port_in_use(host: str, port: int) -> bool:
    with socket.socket() as s:
        s.settimeout(0.3)
        return s.connect_ex((host, port)) == 0


def wait_for(base: str, process: subprocess.Popen, seconds: float = 60) -> None:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if process.poll() is not None:
            fail("the server stopped while starting; see its output above")
        try:
            with urllib.request.urlopen(f"{base}/healthz", timeout=1) as r:
                if r.status == 200:
                    return
        except OSError:
            time.sleep(0.2)
    fail("the server didn't start within a minute")


# ---------- sample files ----------


def have_ffmpeg() -> bool:
    return shutil.which("ffmpeg") is not None


def ffmpeg(*args: str) -> None:
    subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *args],
        check=True,
    )


def tone(out_dir: Path, stem: str, frequency: int, codec: str) -> Path:
    """A sine tone of TONE_SECONDS. `codec` is flac, mp3, opus or wav."""
    if not have_ffmpeg():
        codec = "wav"
    path = out_dir / f"{stem}.{codec}"
    if codec == "wav":
        rate = 22050
        with wave.open(str(path), "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(rate)
            frames = bytearray()
            for n in range(rate * TONE_SECONDS):
                sample = int(0.3 * 32767 * math.sin(2 * math.pi * frequency * n / rate))
                frames += struct.pack("<h", sample)
            w.writeframes(bytes(frames))
        return path
    source = ["-f", "lavfi", "-i", f"sine=frequency={frequency}:duration={TONE_SECONDS}"]
    encode = {
        "flac": ["-ar", "22050", "-c:a", "flac"],
        "mp3": ["-ar", "22050", "-c:a", "libmp3lame", "-b:a", "48k"],
        "opus": ["-c:a", "libopus", "-b:a", "24k"],
    }[codec]
    ffmpeg(*source, "-ac", "1", *encode, str(path))
    return path


def film(out_dir: Path) -> Path | None:
    if not have_ffmpeg():
        say("ffmpeg isn't installed, so there will be no film")
        return None
    path = out_dir / "Test Pattern.mp4"
    ffmpeg(
        "-f", "lavfi", "-i", f"testsrc=size=320x180:rate=24:duration={FILM_SECONDS}",
        "-f", "lavfi", "-i", f"sine=frequency=660:duration={FILM_SECONDS}",
        "-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "veryfast", "-crf", "32",
        "-c:a", "aac", "-b:a", "48k", "-shortest", "-movflags", "+faststart",
        str(path),
    )
    return path


def subtitles(out_dir: Path, language: str, lines: list[str]) -> Path:
    path = out_dir / f"Test Pattern.{language}.vtt"
    cues = []
    for n, line in enumerate(lines):
        start, end = n * 3, n * 3 + 3
        cues.append(f"00:00:{start:02d}.000 --> 00:00:{end:02d}.000\n{line}\n")
    path.write_text("WEBVTT\n\n" + "\n".join(cues), encoding="utf-8")
    return path


def pdf(out_dir: Path) -> Path:
    """A one-page PDF written by hand (with a correct cross-reference table)."""
    text = "A Tiny Book. Chapter one: the end."
    stream = f"BT /F1 24 Tf 72 720 Td ({text}) Tj ET".encode()
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
        b"/Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % number + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    for offset in offsets:
        out += b"%010d 00000 n \n" % offset
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (
        len(objects) + 1,
        xref,
    )
    path = out_dir / "A Tiny Book.pdf"
    path.write_bytes(bytes(out))
    return path


def cover(out_dir: Path, stem: str, top: tuple[int, int, int], bottom: tuple[int, int, int]) -> Path:
    """A 300x300 PNG with a vertical gradient and a pale disc in the middle."""
    size = 300
    rows = bytearray()
    for y in range(size):
        rows.append(0)
        t = y / (size - 1)
        base = [round(a + (b - a) * t) for a, b in zip(top, bottom)]
        for x in range(size):
            d = math.hypot(x - size / 2, y - size / 2)
            if d < 90 and d > 18:
                rows += bytes(min(255, c + 60) for c in base)
            else:
                rows += bytes(base)

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(rows), 9))
    png += chunk(b"IEND", b"")
    path = out_dir / f"{stem}.png"
    path.write_bytes(png)
    return path


# ---------- HTTP ----------


class Client:
    """A browser-like session: a cookie jar and same-origin headers."""

    def __init__(self, base: str) -> None:
        self.base = base
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))

    def request(self, method: str, path: str, body: bytes | None = None, headers: dict | None = None):
        all_headers = {"Origin": self.base, **(headers or {})}
        req = urllib.request.Request(self.base + path, data=body, method=method, headers=all_headers)
        try:
            return self.opener.open(req, timeout=60)
        except urllib.error.HTTPError as e:
            detail = e.read().decode("utf-8", "replace")[:400]
            fail(f"{method} {path} answered {e.code}: {detail}")

    def json(self, method: str, path: str, data=None):
        body = None if data is None else json.dumps(data).encode()
        headers = {"Accept": "application/json"}
        if body is not None:
            headers["Content-Type"] = "application/json"
        with self.request(method, path, body, headers) as r:
            text = r.read().decode()
            return json.loads(text) if text else None

    def form(self, path: str, fields: list[tuple[str, str]], files: list[tuple[str, Path]] = (), accept: str = "text/html"):
        """POSTs multipart/form-data with the text fields first, then files."""
        boundary = uuid.uuid4().hex
        body = bytearray()
        for name, value in fields:
            body += f"--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n".encode()
        for name, path_ in files:
            body += (
                f"--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; "
                f"filename=\"{path_.name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            ).encode()
            body += path_.read_bytes() + b"\r\n"
        body += f"--{boundary}--\r\n".encode()
        headers = {"Content-Type": f"multipart/form-data; boundary={boundary}", "Accept": accept}
        return self.request("POST", path, bytes(body), headers)


def unlock(base: str) -> Client:
    admin = Client(base)
    data = urllib.parse.urlencode({"password": ADMIN_PASSWORD, "next": "/"}).encode()
    admin.request("POST", "/unlock", data, {"Content-Type": "application/x-www-form-urlencoded"}).close()
    if not any(c.name for c in admin.jar):
        fail("unlocking with the admin password didn't give a session")
    return admin


def sign_in(base: str, login: str) -> Client:
    client = Client(base)
    client.json("POST", "/api/account/login", {"login": login, "password": ACCOUNT_PASSWORD})
    return client


def create_item(admin: Client, type_id: int, title: str, creator: str, year: str,
                description: str, photo: Path, restricted: bool = False) -> int:
    fields = [
        ("type_id", str(type_id)),
        ("title", title),
        ("creator", creator),
        ("year", year),
        ("description", description),
        ("keep_duplicate", "yes"),
    ]
    if restricted:
        fields.append(("restricted", "yes"))
    with admin.form("/admin/items", fields, [("photos", photo)]) as r:
        final = urllib.parse.urlparse(r.geturl()).path
    if not final.startswith("/items/"):
        fail(f"adding {title!r} ended at {final} instead of the item page")
    return int(final.removeprefix("/items/").split("/")[0])


def upload(admin: Client, item_id: int, path: Path, kind: str, title: str,
           track: int | None, duration_ms: int | None) -> int:
    fields = [
        ("kind", kind),
        ("title", title),
        ("disc", "1"),
        ("track", "" if track is None else str(track)),
        ("duration_ms", "" if duration_ms is None else str(duration_ms)),
    ]
    with admin.form(f"/admin/items/{item_id}/media/upload", fields, [("file", path)], "application/json") as r:
        if r.status != 201:
            fail(f"uploading {path.name} answered {r.status}")
        media_id = json.loads(r.read())["id"]
    say(f"  uploaded {path.name} (file {media_id})")
    return media_id


def add_subtitles(admin: Client, item_id: int, media_id: int, path: Path, language: str, label: str) -> None:
    fields = [("language", language), ("label", label)]
    with admin.form(f"/admin/items/{item_id}/media/{media_id}/subtitles", fields, [("file", path)], "application/json") as r:
        if r.status != 201:
            fail(f"adding subtitles {path.name} answered {r.status}")
    say(f"  added {label} subtitles")


def create_playlist(client: Client, name: str, media_ids: list[int]) -> int:
    playlist = client.json("POST", "/api/playlists", {"name": name})
    client.json("POST", f"/api/playlists/{playlist['id']}/entries", {"mediaIds": media_ids})
    return playlist["id"]


# ---------- main ----------


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--config", default=str(REPO / "library.dev.toml"), help="config file (default: library.dev.toml)")
    parser.add_argument("--bin", default=os.environ.get("LIBRARY_BIN"), help="library binary (default: cargo build)")
    args = parser.parse_args()

    config = Path(args.config).resolve()
    if not config.is_file():
        fail(f"there is no config file at {config}")
    settings = tomllib.loads(config.read_text())
    # Relative paths in the config are relative to where the server runs,
    # which for this script is the repository root.
    data_dir = (REPO / settings.get("data_dir", "data")).resolve()
    if data_dir.name == "data":
        fail(f"refusing to wipe {data_dir}: that looks like a real catalogue. Use a dev data_dir such as ./data-dev")
    if "admin_password_hash" not in settings and "admin_password" not in settings:
        fail("the config has no admin password; the seed needs one to upload files")

    listen = settings.get("listen", "127.0.0.1:8080")
    host, _, port = listen.rpartition(":")
    if port_in_use(host or "127.0.0.1", int(port)):
        fail(f"something is already listening on {listen}. If it's the Library on this data, stop it first: "
             "the seed replaces its data folder.")

    binary = find_binary(args.bin)

    if data_dir.exists():
        say(f"wiping {data_dir} to start fresh")
        shutil.rmtree(data_dir)

    say("making accounts adult and kid (password: password123)")
    run_cli(binary, config, "create-account", "adult", "adult@example.org", "--adult")
    run_cli(binary, config, "create-account", "kid", "kid@example.org")

    seed_port = free_port()
    base = f"http://127.0.0.1:{seed_port}"
    say(f"starting a temporary server on {base}")
    server = subprocess.Popen(
        [str(binary), "--config", str(config), "serve", "--listen", f"127.0.0.1:{seed_port}"],
        cwd=REPO,
        env=dict(os.environ, RUST_LOG="library=warn"),
    )
    try:
        wait_for(base, server)
        with tempfile.TemporaryDirectory(prefix="library-seed-") as tmp:
            seed(binary, config, base, data_dir, Path(tmp))
    finally:
        server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill()

    print()
    say("done. Start the dev server with:")
    shown = config.relative_to(REPO) if config.is_relative_to(REPO) else config
    print(f"    cargo run -- --config {shown} serve")
    print(f"  then open http://{listen}/player/  (adult / {ACCOUNT_PASSWORD}, kid / {ACCOUNT_PASSWORD})")
    print(f"  and http://{listen}/ (unlock editing with the admin password \"{ADMIN_PASSWORD}\").")


def seed(binary: Path, config: Path, base: str, data_dir: Path, tmp: Path) -> None:
    with sqlite3.connect(f"file:{data_dir / 'library.sqlite3'}?mode=ro", uri=True) as db:
        types = dict(db.execute("SELECT slug, id FROM media_types"))
    admin = unlock(base)
    tone_ms = TONE_SECONDS * 1000

    say('adding "Sine Songs" (music)')
    album = create_item(admin, types["music"], "Sine Songs", "The Test Tones", "2024",
                        "Three pure tones for testing the Player.", cover(tmp, "sine", (29, 85, 65), (12, 40, 30)))
    songs = [
        upload(admin, album, tone(tmp, "01 - Morning (A)", 440, "flac"), "audio", "Morning (A)", 1, tone_ms),
        upload(admin, album, tone(tmp, "02 - Noon (C)", 523, "mp3"), "audio", "Noon (C)", 2, tone_ms),
        upload(admin, album, tone(tmp, "03 - Evening (E)", 659, "opus"), "audio", "Evening (E)", 3, tone_ms),
    ]

    say('adding "Late Night Tones" (music, adults only)')
    late = create_item(admin, types["music"], "Late Night Tones", "The Test Tones", "2025",
                       "Two low tones. Marked Restricted, so only adult accounts see it.",
                       cover(tmp, "late", (60, 30, 70), (15, 10, 25)), restricted=True)
    late_tracks = [
        upload(admin, late, tone(tmp, "01 - Low Hum", 220, "mp3"), "audio", "Low Hum", 1, tone_ms),
        upload(admin, late, tone(tmp, "02 - Lower Hum", 196, "mp3"), "audio", "Lower Hum", 2, tone_ms),
    ]

    film_file = film(tmp)
    film_id = film_item = None
    if film_file is not None:
        say('adding "Test Pattern" (film)')
        film_item = create_item(admin, types["film"], "Test Pattern", "A. Camera", "2023",
                                "Colour bars and a tone, with English and French subtitles.",
                                cover(tmp, "film", (140, 90, 20), (60, 35, 10)))
        film_id = upload(admin, film_item, film_file, "video", "Test Pattern", None, FILM_SECONDS * 1000)
        add_subtitles(admin, film_item, film_id,
                      subtitles(tmp, "en", ["Hello.", "This is a test pattern.", "Goodbye."]), "en", "English")
        add_subtitles(admin, film_item, film_id,
                      subtitles(tmp, "fr", ["Bonjour.", "Ceci est une mire.", "Au revoir."]), "fr", "Français")

    say('adding "A Tiny Book" (book with a PDF)')
    book = create_item(admin, types["book"], "A Tiny Book", "P. D. Eff", "2022",
                       "A one-page book, to try the document viewer.",
                       cover(tmp, "book", (170, 60, 50), (90, 25, 20)))
    book_pdf = upload(admin, book, pdf(tmp), "document", "A Tiny Book", None, None)

    say("making playlists and favourites")
    adult = sign_in(base, "adult")
    mixed = [songs[0], late_tracks[0], songs[2]] + ([film_id] if film_id else [])
    create_playlist(adult, "Saturday mix", mixed)  # the kid sees only the allowed entries
    create_playlist(adult, "After hours", late_tracks)  # adults only: hidden from the kid
    adult.json("PUT", f"/api/favourites/{songs[0]}")
    adult.json("PUT", f"/api/favourites/{late_tracks[1]}")

    # The kid briefly has adult access, so their own playlist holds a
    # restricted track that they then no longer see (but which is kept).
    run_cli(binary, config, "set-access", "kid", "adult")
    kid = sign_in(base, "kid")
    create_playlist(kid, "Kid's picks", [songs[1], late_tracks[1], book_pdf])
    run_cli(binary, config, "set-access", "kid", "child")
    kid.json("PUT", f"/api/favourites/{songs[1]}")

    summary = {
        "items": {"sineSongs": album, "lateNightTones": late, "film": film_item, "book": book},
        "media": {"songs": songs, "lateTracks": late_tracks, "film": film_id, "pdf": book_pdf},
    }
    (data_dir / "seed.json").write_text(json.dumps(summary, indent=2) + "\n")
    say(f"wrote {data_dir / 'seed.json'} (ids, for the end-to-end tests)")


if __name__ == "__main__":
    main()
