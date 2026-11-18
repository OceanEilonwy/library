#!/usr/bin/env python3
"""Import a book spreadsheet (.ods) into a running Library instance.

Reads the sheet with the column headings used in "Hollingworth Library.ods"
(Author/s, Title, Series, Sub-Title, Volume, Year of Publication, F/NF,
Genre, Recommended age, No. of Copies, Illustrator, Translator,
Photographer, Format, Last Known Location) and adds each row as a Book
through the web interface, so validation, search indexing and meaning
search all apply.

    python3 scripts/import_ods.py "Hollingworth Library.ods"            # dry run
    python3 scripts/import_ods.py "Hollingworth Library.ods" --apply    # import

Safe to re-run: rows the library flags as already present are skipped
(use --keep-duplicates to add them anyway). Standard library only.
"""

import argparse
import getpass
import html
import http.cookiejar
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid
import zipfile
import xml.etree.ElementTree as ET

T = "{urn:oasis:names:tc:opendocument:xmlns:table:1.0}"
P = "{urn:oasis:names:tc:opendocument:xmlns:text:1.0}p"

# Spreadsheet column -> custom Book field (created if missing).
CUSTOM_FIELDS = {
    "series": "Series",
    "volume": "Volume",
    "age": "Recommended age",
    "illustrator": "Illustrator",
    "translator": "Translator",
    "photographer": "Photographer",
    "category": "Category",
}

HEADINGS = {
    "author/s": "author",
    "author": "author",
    "title": "title",
    "series": "series",
    "sub-title": "subtitle",
    "subtitle": "subtitle",
    "volume": "volume",
    "year of publication": "year",
    "f/nf": "fnf",
    "genre": "genre",
    "recommended age": "age",
    "no. of copies": "copies",
    "illustrator": "illustrator",
    "translator": "translator",
    "photographer": "photographer",
    "format": "format",
    "last known location": "location",
}


# ---------- reading the sheet ----------

def read_ods(path):
    """Rows of the first sheet as lists of strings."""
    with zipfile.ZipFile(path) as z:
        root = ET.fromstring(z.read("content.xml"))
    sheet = next(root.iter(T + "table"))
    rows = []
    for row in sheet.iter(T + "table-row"):
        cells = []
        for cell in row:
            repeat = int(cell.get(T + "number-columns-repeated", "1"))
            text = "\n".join("".join(p.itertext()) for p in cell.findall(P))
            cells.extend([text] * min(repeat, 64))
        while cells and not cells[-1].strip():
            cells.pop()
        rows.append(cells)
    return rows


def find_columns(rows):
    """Locates the heading row. The Series heading may sit on the row above
    its column (as in the original sheet), so blanks are filled from there."""
    for i, row in enumerate(rows[:10]):
        names = [c.strip().lower() for c in row]
        if "title" in names:
            above = rows[i - 1] if i > 0 else []
            cols = {}
            for j, name in enumerate(names):
                if not name and j < len(above):
                    name = above[j].strip().lower()
                if name in HEADINGS:
                    cols[HEADINGS[name]] = j
            return i, cols
    sys.exit("Couldn't find a heading row with a 'Title' column.")


# ---------- turning a row into a Library entry ----------

def natural_name(raw):
    """'Tolkien, J. R. R.' -> 'J. R. R. Tolkien'; pairs for several authors:
    'Matthews, B. A., Winter, K. R.' -> 'B. A. Matthews, K. R. Winter'."""
    raw = raw.strip().strip(",")
    if not raw or "," not in raw:
        return raw
    parts = [p.strip() for p in raw.split(",") if p.strip()]
    if len(parts) % 2:
        return raw
    return ", ".join(f"{parts[k + 1]} {parts[k]}" for k in range(0, len(parts), 2))


def location_path(raw):
    """'Plastic box, in shed (1)' -> 'Shed › Plastic box 1';
    'On Bookshelf 1' -> 'Bookshelf 1'."""
    raw = raw.strip()
    m = re.fullmatch(r"(.+?),\s*in (?:the )?(.+?)(?:\s*\((\w+)\))?", raw, re.I)
    if m:
        thing, place, n = m.group(1).strip(), m.group(2).strip(), m.group(3)
        place = place[:1].upper() + place[1:]
        thing = thing[:1].upper() + thing[1:]
        return f"{place} › {thing}{' ' + n if n else ''}"
    m = re.fullmatch(r"(?:on|in) (?:the )?(.+)", raw, re.I)
    if m:
        return m.group(1).strip()
    return raw


def genre_tags(raw):
    raw = raw.replace("(?)", "").strip()
    return ", ".join(t.strip() for t in raw.split("/") if t.strip())


def to_entry(row, cols):
    get = lambda k: row[cols[k]].strip() if k in cols and cols[k] < len(row) else ""
    title = get("title")
    if get("subtitle"):
        title = f"{title}: {get('subtitle')}"
    year_raw = get("year")
    m = re.search(r"\b(\d{4})\b", year_raw)
    year = m.group(1) if m else ""
    fnf = get("fnf").upper()
    entry = {
        "title": title,
        "creator": natural_name(get("author")),
        "year": year,
        "format": get("format"),
        "tags": genre_tags(get("genre")),
        "location": location_path(get("location")),
        "fields": {
            "series": get("series"),
            "volume": get("volume"),
            "age": get("age"),
            "illustrator": get("illustrator"),
            "translator": get("translator"),
            "photographer": get("photographer"),
            "category": {"F": "Fiction", "NF": "Nonfiction"}.get(fnf, ""),
        },
        "notes": "",
    }
    if year_raw and year_raw != year:
        entry["notes"] = f"Year in spreadsheet: {year_raw}"
    copies = get("copies")
    if copies and copies not in ("1", ""):
        entry["notes"] = (entry["notes"] + f"\nCopies: {copies}").strip()
    return entry


# ---------- talking to the Library ----------

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class Library:
    def __init__(self, base):
        self.base = base.rstrip("/")
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar), NoRedirect)

    def request(self, path, data=None, headers=None):
        h = {"Origin": self.base, **(headers or {})}
        req = urllib.request.Request(self.base + path, data=data, headers=h)
        try:
            with self.opener.open(req, timeout=60) as r:
                return r.status, r.headers, r.read().decode()
        except urllib.error.HTTPError as e:
            return e.code, e.headers, e.read().decode()

    def form(self, path, fields, htmx=False):
        headers = {"Content-Type": "application/x-www-form-urlencoded"}
        if htmx:
            headers["HX-Request"] = "true"
        return self.request(path, urllib.parse.urlencode(fields).encode(), headers)

    def multipart(self, path, fields):
        b = uuid.uuid4().hex
        body = b"".join(
            f'--{b}\r\nContent-Disposition: form-data; name="{k}"\r\n\r\n{v}\r\n'.encode()
            for k, v in fields
        ) + f"--{b}--\r\n".encode()
        return self.request(path, body, {"Content-Type": f"multipart/form-data; boundary={b}"})

    def unlock(self, password):
        status, headers, body = self.form("/unlock", {"password": password, "next": "/admin"})
        if status != 303:
            msg = re.search(r'role="alert">([^<]+)<', body)
            sys.exit(f"Couldn't unlock: {html.unescape(msg.group(1)) if msg else status}")

    def book_type_id(self):
        _, _, body = self.request("/admin/items/new")
        for value, name in re.findall(r'<option value="(\d+)"[^>]*>([^<]+)</option>', body):
            if html.unescape(name).strip().lower() == "book":
                return int(value)
        sys.exit("The library has no 'Book' type.")

    def fields(self, type_id):
        _, _, body = self.request(f"/admin/items/type-fields?type_id={type_id}", headers={"HX-Request": "true"})
        found = {}
        for fid, name in re.findall(r'<label for="field_(\d+)">([^<]+?)(?: \(this item\))?</label>', body):
            found[html.unescape(name).strip().lower()] = int(fid)
        return found

    def ensure_field(self, type_id, name, kind="text", options=""):
        existing = self.fields(type_id)
        if name.lower() in existing:
            return existing[name.lower()]
        self.form(
            "/admin/items/add-field",
            {"type_id": type_id, "new_field_name": name, "new_field_kind": kind, "new_field_options": options},
            htmx=True,
        )
        existing = self.fields(type_id)
        if name.lower() not in existing:
            sys.exit(f"Couldn't create the field '{name}'.")
        return existing[name.lower()]

    def add(self, type_id, entry, field_ids, keep_duplicates):
        fields = [
            ("type_id", type_id),
            ("title", entry["title"]),
            ("creator", entry["creator"]),
            ("year", entry["year"]),
            ("format", entry["format"]),
            ("tags", entry["tags"]),
            ("location", entry["location"]),
            ("notes", entry["notes"]),
        ]
        for key, value in entry["fields"].items():
            if value and key in field_ids:
                fields.append((f"field_{field_ids[key]}", value))
        if keep_duplicates:
            fields.append(("keep_duplicate", "1"))
        status, headers, body = self.multipart("/admin/items", fields)
        if status == 303:
            return "added", headers.get("Location", "")
        if status == 422 and "possible duplicate" in body:
            return "duplicate", ""
        errors = [html.unescape(e) for e in re.findall(r'class="error"[^>]*>([^<]+)<', body)]
        return "error", "; ".join(errors) or f"HTTP {status}"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("ods", help="the .ods spreadsheet")
    ap.add_argument("--url", default="http://127.0.0.1:8080", help="the Library's address")
    ap.add_argument("--apply", action="store_true", help="actually import (otherwise just show what would happen)")
    ap.add_argument("--keep-duplicates", action="store_true", help="add rows even if they look like existing entries")
    args = ap.parse_args()

    rows = read_ods(args.ods)
    head, cols = find_columns(rows)
    entries = [to_entry(r, cols) for r in rows[head + 1:] if any(c.strip() for c in r)]
    entries = [e for e in entries if e["title"]]
    used = {k for e in entries for k, v in e["fields"].items() if v}

    print(f"{len(entries)} books in {os.path.basename(args.ods)}\n")
    for e in entries:
        extra = ", ".join(f"{CUSTOM_FIELDS[k]}: {v}" for k, v in e["fields"].items() if v)
        print(f"• {e['title']}" + (f" — {e['creator']}" if e["creator"] else ""))
        print(f"    {e['year'] or '(no year)'} · {e['location'] or '(no location)'}" + (f" · tags: {e['tags']}" if e["tags"] else ""))
        if extra:
            print(f"    {extra}")
        if e["notes"]:
            print(f"    notes: {e['notes']}")
    print(f"\nCustom Book fields used: {', '.join(CUSTOM_FIELDS[k] for k in CUSTOM_FIELDS if k in used)}")
    print("Locations: " + "; ".join(sorted({e["location"] for e in entries if e["location"]})))

    if not args.apply:
        print("\nDry run only. Add --apply to import.")
        return

    lib = Library(args.url)
    password = os.environ.get("LIBRARY_PASSWORD") or getpass.getpass("Admin password: ")
    lib.unlock(password)
    type_id = lib.book_type_id()
    field_ids = {}
    for key in CUSTOM_FIELDS:
        if key in used:
            if key == "category":
                field_ids[key] = lib.ensure_field(type_id, CUSTOM_FIELDS[key], "choice", "Fiction, Nonfiction")
            else:
                field_ids[key] = lib.ensure_field(type_id, CUSTOM_FIELDS[key])

    counts = {"added": 0, "duplicate": 0, "error": 0}
    for e in entries:
        outcome, detail = lib.add(type_id, e, field_ids, args.keep_duplicates)
        counts[outcome] += 1
        if outcome == "duplicate":
            print(f"  skipped (already in the library?): {e['title']}")
        elif outcome == "error":
            print(f"  FAILED: {e['title']}: {detail}")
    print(f"\nAdded {counts['added']}, skipped {counts['duplicate']} possible duplicates, {counts['error']} failed.")
    if counts["duplicate"]:
        print("Re-run with --keep-duplicates to add the skipped ones anyway.")


if __name__ == "__main__":
    main()
