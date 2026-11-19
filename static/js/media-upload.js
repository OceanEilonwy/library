// The upload studio (/admin/items/{id}/media). Loaded only on that page.
//
// Files and folders dropped or picked here are sorted out in the browser
// before anything is sent: kind from the extension, disc/track/title from
// the file name, length from a hidden media element, subtitles matched to
// their film. Each file then uploads on its own request with a progress bar
// (and a retry button if it fails). The server stores files exactly as sent.
(function () {
  "use strict";

  var studio = document.getElementById("studio");
  if (!studio) return;
  var itemId = studio.getAttribute("data-item");
  var maxBytes = Number(studio.getAttribute("data-max-bytes")) || Infinity;

  var AUDIO = ["mp3", "m4a", "aac", "flac", "ogg", "oga", "opus", "wav"];
  var VIDEO = ["mp4", "m4v", "mkv", "mov"];
  var DOCUMENT = ["pdf", "epub"];
  var SUBTITLE = ["srt", "vtt"];

  var LANGUAGES = {
    en: "English", fr: "French", de: "German", es: "Spanish", it: "Italian",
    pt: "Portuguese", nl: "Dutch", pl: "Polish", sv: "Swedish", da: "Danish",
    no: "Norwegian", fi: "Finnish", ja: "Japanese", zh: "Chinese", ko: "Korean",
    ru: "Russian", ar: "Arabic", he: "Hebrew", el: "Greek", tr: "Turkish",
    cs: "Czech", hu: "Hungarian", cy: "Welsh", ga: "Irish"
  };
  var LANGUAGE_ALIASES = {
    eng: "en", english: "en", fre: "fr", fra: "fr", french: "fr", ger: "de",
    deu: "de", german: "de", spa: "es", spanish: "es", ita: "it", italian: "it",
    por: "pt", portuguese: "pt", dut: "nl", nld: "nl", dutch: "nl", pol: "pl",
    polish: "pl", swe: "sv", swedish: "sv", dan: "da", danish: "da", nor: "no",
    nob: "no", nb: "no", norwegian: "no", fin: "fi", finnish: "fi", jpn: "ja",
    japanese: "ja", chi: "zh", zho: "zh", chinese: "zh", kor: "ko", korean: "ko",
    rus: "ru", russian: "ru", ara: "ar", arabic: "ar", heb: "he", hebrew: "he",
    gre: "el", ell: "el", greek: "el", tur: "tr", turkish: "tr", cze: "cs",
    ces: "cs", czech: "cs", hun: "hu", hungarian: "hu", wel: "cy", cym: "cy",
    welsh: "cy", gle: "ga", irish: "ga"
  };

  var pending = [];      // files waiting to upload (and their outcome)
  var looseSubs = [];    // subtitles with no film to go with
  var skipped = [];      // names of files that can't be used
  var nextId = 1;
  var uploading = false;

  var $ = function (id) { return document.getElementById(id); };
  var zone = $("drop-zone");
  var rowsEl = $("pending-rows");
  var pendingEl = $("pending");
  var summaryEl = $("pending-summary");
  var notesEl = $("pending-notes");
  var statusEl = $("upload-status");
  var uploadBtn = $("upload-all");
  var clearBtn = $("clear-pending");

  // ---------- reading names ----------

  function extensionOf(name) {
    var m = /\.([^./\\]+)$/.exec(name);
    return m ? m[1].toLowerCase() : "";
  }

  function stemOf(name) {
    var base = name.split(/[\\/]/).pop();
    return base.replace(/\.[^.]+$/, "") || base;
  }

  function kindOf(file) {
    var ext = extensionOf(file.name);
    if (AUDIO.indexOf(ext) >= 0) return "audio";
    if (VIDEO.indexOf(ext) >= 0) return "video";
    if (DOCUMENT.indexOf(ext) >= 0) return "document";
    if (SUBTITLE.indexOf(ext) >= 0) return "subtitle";
    if (ext === "webm") return /^audio\//.test(file.type) ? "audio" : "video";
    return null;
  }

  function tidy(title) {
    var t = title.replace(/_/g, " ");
    if (t.indexOf(" ") < 0 && t.indexOf(".") >= 0) t = t.replace(/\./g, " ");
    return t.replace(/\s+/g, " ").trim();
  }

  // Reads disc, track and a clean title from a path such as
  // "01 - Title.flac", "1-03 Title.mp3" or "CD2/04. Title.m4a".
  function parseName(path, kind) {
    var parts = path.split("/");
    var base = parts.pop();
    var stem = stemOf(base);
    var disc = null, track = null, title = stem, m;
    for (var i = parts.length - 1; i >= 0; i--) {
      m = /^(?:cd|disc|disk)\s*[-_.]?\s*(\d{1,2})\b/i.exec(parts[i]);
      if (m) { disc = Number(m[1]); break; }
    }
    if (kind === "audio") {
      if ((m = /^(\d{1,2})[-.](\d{1,3})(?:\s*[-.)_]\s*|\s+)(.+)$/.exec(stem)) && Number(m[2]) > 0) {
        if (disc === null) disc = Number(m[1]);
        track = Number(m[2]);
        title = m[3];
      } else if ((m = /^(\d{1,3})(?:\s*[-.)_]\s*|\s+)(.+)$/.exec(stem))) {
        track = Number(m[1]);
        title = m[2];
      } else if ((m = /^(.+?)\s+-\s+(\d{1,3})\s+-\s+(.+)$/.exec(stem))) {
        track = Number(m[2]);
        title = m[3];
      }
    }
    return { disc: disc || 1, track: track, title: tidy(title) || stem };
  }

  // "Film.en.srt" → { stem: "Film", language: "en", label: "English" }
  function parseSubtitle(name) {
    var stem = stemOf(name);
    var bits = stem.split(".");
    var language = "und", sdh = false;
    while (bits.length > 1) {
      var tag = bits[bits.length - 1].toLowerCase();
      if (/^(sdh|cc|hi|forced)$/.test(tag)) { sdh = sdh || tag !== "forced"; bits.pop(); continue; }
      var code = tag.split(/[-_]/)[0];
      code = LANGUAGE_ALIASES[code] || code;
      if (LANGUAGES[code]) { language = code; bits.pop(); }
      break;
    }
    var label = LANGUAGES[language] || "Subtitles";
    if (sdh) label += " (SDH)";
    return { stem: bits.join("."), language: language, label: label };
  }

  function naturalCompare(a, b) {
    return a.localeCompare(b, undefined, { numeric: true, sensitivity: "base" });
  }

  // ---------- collecting files ----------

  // Walks dropped folders (DataTransferItem.webkitGetAsEntry).
  function readEntry(entry, path, out) {
    return new Promise(function (resolve) {
      if (entry.isFile) {
        entry.file(function (f) { out.push({ file: f, path: path + f.name }); resolve(); },
                   function () { resolve(); });
      } else if (entry.isDirectory) {
        var reader = entry.createReader();
        var all = [];
        (function more() {
          reader.readEntries(function (batch) {
            if (!batch.length) {
              Promise.all(all.map(function (e) { return readEntry(e, path + entry.name + "/", out); }))
                .then(function () { resolve(); });
              return;
            }
            all = all.concat(Array.prototype.slice.call(batch));
            more();
          }, function () { resolve(); });
        })();
      } else {
        resolve();
      }
    });
  }

  function filesFromDrop(dt) {
    var items = dt.items ? Array.prototype.slice.call(dt.items) : [];
    var entries = items
      .filter(function (i) { return i.kind === "file"; })
      .map(function (i) { return i.webkitGetAsEntry ? i.webkitGetAsEntry() : null; });
    if (entries.length && entries.every(Boolean)) {
      var out = [];
      return Promise.all(entries.map(function (e) { return readEntry(e, "", out); }))
        .then(function () { return out; });
    }
    return Promise.resolve(Array.prototype.slice.call(dt.files || []).map(function (f) {
      return { file: f, path: f.name };
    }));
  }

  function filesFromInput(input) {
    return Array.prototype.slice.call(input.files || []).map(function (f) {
      return { file: f, path: f.webkitRelativePath || f.name };
    });
  }

  function add(found) {
    var subs = [];
    found.forEach(function (x) {
      var base = x.path.split("/").pop();
      if (!base || base.charAt(0) === ".") return;
      var kind = kindOf(x.file);
      if (!kind) { skipped.push(base); return; }
      if (kind === "subtitle") { subs.push(x); return; }
      if (pending.some(function (p) { return p.path === x.path && p.size === x.file.size && p.status !== "done"; })) return;
      var parsed = parseName(x.path, kind);
      pending.push({
        id: nextId++, file: x.file, path: x.path, kind: kind,
        disc: parsed.disc, track: kind === "audio" ? parsed.track : null, title: parsed.title,
        durationMs: null, size: x.file.size, subtitles: [],
        status: x.file.size > maxBytes ? "error" : "ready", progress: 0,
        error: x.file.size > maxBytes ? "Bigger than the upload limit." : ""
      });
    });
    pending.sort(function (a, b) {
      if (a.status === "done" || b.status === "done") return 0;
      return (a.disc - b.disc) ||
        ((a.track === null) - (b.track === null)) ||
        ((a.track || 0) - (b.track || 0)) ||
        naturalCompare(a.path, b.path);
    });
    subs.forEach(attachSubtitle);
    render();
    measureAll();
  }

  function attachSubtitle(x) {
    var info = parseSubtitle(x.file.name);
    var videos = pending.filter(function (p) { return p.kind === "video" && p.status !== "done"; });
    var match = videos.filter(function (v) {
      var stem = stemOf(v.file.name);
      return stem === info.stem || stem.toLowerCase() === info.stem.toLowerCase();
    })[0];
    if (!match && videos.length === 1) match = videos[0];
    var sub = { file: x.file, language: info.language, label: info.label, status: "ready" };
    if (match) match.subtitles.push(sub);
    else looseSubs.push(x.file.name);
  }

  // ---------- measuring length ----------

  var measuring = 0;
  function measureAll() {
    pending.forEach(function (p) {
      if (p.durationMs !== null || p.measuring || p.kind === "document" || measuring >= 3) return;
      p.measuring = true;
      measuring++;
      measure(p.file, p.kind).then(function (ms) {
        p.durationMs = ms;
        p.measured = true;
        measuring--;
        updateRow(p);
        renderSummary();
        measureAll();
      });
    });
  }

  function measure(file, kind) {
    return new Promise(function (resolve) {
      var el = document.createElement(kind === "video" ? "video" : "audio");
      var url = URL.createObjectURL(file);
      var done = false;
      function finish(ms) {
        if (done) return;
        done = true;
        URL.revokeObjectURL(url);
        el.removeAttribute("src");
        try { el.load(); } catch (e) { /* ignore */ }
        resolve(ms);
      }
      el.preload = "metadata";
      el.muted = true;
      el.onloadedmetadata = function () {
        finish(isFinite(el.duration) && el.duration > 0 ? Math.round(el.duration * 1000) : null);
      };
      el.onerror = function () { finish(null); };
      setTimeout(function () { finish(null); }, 15000);
      el.src = url;
    }).then(function (ms) { return ms === null ? 0 : ms; });
  }

  // ---------- showing the list ----------

  function duration(ms) {
    if (!ms) return "—";
    var s = Math.round(ms / 1000), h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60;
    s = s % 60;
    return (h ? h + ":" + String(m).padStart(2, "0") : m) + ":" + String(s).padStart(2, "0");
  }

  function size(bytes) {
    var units = ["bytes", "KB", "MB", "GB"], i = 0, v = bytes;
    while (v >= 1000 && i < units.length - 1) { v /= 1000; i++; }
    return i ? v.toFixed(1) + " " + units[i] : bytes + " bytes";
  }

  function el(tag, attrs, children) {
    var e = document.createElement(tag);
    Object.keys(attrs || {}).forEach(function (k) {
      if (k === "text") e.textContent = attrs[k];
      else if (k === "className") e.className = attrs[k];
      else e.setAttribute(k, attrs[k]);
    });
    (children || []).forEach(function (c) { if (c) e.appendChild(c); });
    return e;
  }

  function kindChoices(p) {
    var ext = extensionOf(p.file.name);
    if (ext === "webm") return ["audio", "video"];
    return [p.kind];
  }

  function rowFor(p, index) {
    var locked = p.status === "uploading" || p.status === "done";
    var tr = el("tr", { "data-id": p.id, className: "status-" + p.status, draggable: "false" });
    if (locked) tr.setAttribute("data-locked", "1");

    var order = el("td", { className: "order" }, [
      el("span", { className: "grip", "aria-hidden": "true", text: "⋮⋮" }),
      el("button", { type: "button", className: "btn small", "data-move": "up", "aria-label": "Move up", text: "↑" }),
      el("button", { type: "button", className: "btn small", "data-move": "down", "aria-label": "Move down", text: "↓" })
    ]);
    order.children[1].disabled = locked || index === 0;
    order.children[2].disabled = locked || index === pending.length - 1;
    tr.appendChild(order);

    function numberCell(field, label) {
      var input = el("input", {
        className: "u-input num", inputmode: "numeric", maxlength: field === "disc" ? "3" : "4",
        "data-field": field, "aria-label": label + " for " + p.title,
        value: p[field] === null ? "" : String(p[field])
      });
      input.disabled = locked;
      return el("td", {}, [input]);
    }
    tr.appendChild(numberCell("disc", "Disc"));
    tr.appendChild(numberCell("track", "Track"));

    var title = el("input", { className: "u-input", maxlength: "300", "data-field": "title", "aria-label": "Title", value: p.title });
    title.disabled = locked;
    tr.appendChild(el("td", { className: "title" }, [title, el("span", { className: "path small muted", text: p.path })]));

    var kinds = kindChoices(p);
    if (kinds.length > 1 && !locked) {
      var select = el("select", { className: "u-select", "data-field": "kind", "aria-label": "Kind" });
      kinds.forEach(function (k) {
        var o = el("option", { value: k, text: k === "audio" ? "Music" : "Video" });
        if (k === p.kind) o.selected = true;
        select.appendChild(o);
      });
      tr.appendChild(el("td", {}, [select]));
    } else {
      tr.appendChild(el("td", {}, [el("span", { className: "kind-badge kind-" + p.kind, text: { audio: "Audio", video: "Video", document: "Document" }[p.kind] })]));
    }

    tr.appendChild(el("td", { className: "mono small", "data-cell": "length", text: p.kind === "document" ? "—" : (p.measured ? duration(p.durationMs) : "…") }));
    tr.appendChild(el("td", { className: "mono small", text: size(p.size) }));

    var subs = el("td", { className: "subs" });
    if (p.kind === "video") {
      if (!p.subtitles.length) subs.appendChild(el("span", { className: "small muted", text: "None" }));
      p.subtitles.forEach(function (s, i) {
        var chip = el("span", { className: "sub-chip status-" + s.status, title: s.file.name }, [
          el("span", { text: s.label + " " }),
          el("span", { className: "mono small", text: "(" + s.language + ")" })
        ]);
        if (!locked) {
          chip.appendChild(el("button", { type: "button", className: "x", "data-remove-sub": String(i), "aria-label": "Remove " + s.label + " subtitles", text: "×" }));
        }
        subs.appendChild(chip);
      });
    } else {
      subs.appendChild(el("span", { className: "small muted", text: "—" }));
    }
    tr.appendChild(subs);

    var bar = el("progress", { max: "100", value: String(Math.round(p.progress * 100)) });
    var note = el("span", { className: "small", "data-cell": "note", text: statusText(p) });
    var progress = el("td", { className: "progress" }, [bar, note]);
    if (p.status === "error" && p.file.size <= maxBytes) {
      progress.appendChild(el("button", { type: "button", className: "btn small", "data-retry": "1", text: "Retry" }));
    }
    tr.appendChild(progress);

    var remove = el("button", { type: "button", className: "x", "data-remove": "1", "aria-label": "Don't upload " + p.title, text: "×" });
    remove.disabled = p.status === "uploading";
    tr.appendChild(el("td", {}, [p.status === "done" ? null : remove]));
    return tr;
  }

  function statusText(p) {
    switch (p.status) {
      case "uploading": return Math.round(p.progress * 100) + "%";
      case "done": return "Uploaded";
      case "error": return p.error || "Failed";
      default: return "Ready";
    }
  }

  function render() {
    rowsEl.textContent = "";
    pending.forEach(function (p, i) { rowsEl.appendChild(rowFor(p, i)); });
    pendingEl.hidden = pending.length === 0 && !looseSubs.length && !skipped.length && !statusEl.textContent;
    [".table-scroll", ".pending-hint"].forEach(function (sel) {
      var part = pendingEl.querySelector(sel);
      if (part) part.hidden = pending.length === 0;
    });
    renderSummary();
    var notes = [];
    if (looseSubs.length) notes.push("No film here to go with " + looseSubs.join(", ") + " — add subtitles to a film in the list below instead.");
    if (skipped.length) notes.push("Skipped " + skipped.length + " file" + (skipped.length === 1 ? "" : "s") + " the Player can't use (" + skipped.slice(0, 5).join(", ") + (skipped.length > 5 ? "…" : "") + ").");
    notesEl.textContent = notes.join(" ");
    var waiting = pending.some(function (p) { return p.status === "ready" || p.status === "error"; });
    uploadBtn.disabled = uploading || !waiting;
    clearBtn.disabled = uploading;
  }

  function updateRow(p) {
    var tr = rowsEl.querySelector('tr[data-id="' + p.id + '"]');
    if (!tr) return;
    var length = tr.querySelector('[data-cell="length"]');
    if (length) length.textContent = p.kind === "document" ? "—" : (p.measured ? duration(p.durationMs) : "…");
    var bar = tr.querySelector("progress");
    if (bar) bar.value = Math.round(p.progress * 100);
    var note = tr.querySelector('[data-cell="note"]');
    if (note) note.textContent = statusText(p);
  }

  function renderSummary() {
    var left = pending.filter(function (p) { return p.status !== "done"; });
    var bytes = left.reduce(function (s, p) { return s + p.size; }, 0);
    var ms = left.reduce(function (s, p) { return s + (p.durationMs || 0); }, 0);
    summaryEl.textContent = left.length
      ? left.length + " file" + (left.length === 1 ? "" : "s") + " · " + size(bytes) + (ms ? " · " + duration(ms) : "")
      : "";
  }

  function find(tr) {
    var id = Number(tr && tr.getAttribute("data-id"));
    return pending.filter(function (p) { return p.id === id; })[0];
  }

  // ---------- editing the list ----------

  rowsEl.addEventListener("input", function (e) {
    var field = e.target.getAttribute("data-field");
    var p = find(e.target.closest("tr"));
    if (!field || !p) return;
    var v = e.target.value;
    if (field === "title") p.title = v;
    else if (field === "disc") p.disc = Math.max(1, parseInt(v, 10) || 1);
    else if (field === "track") p.track = v.trim() === "" ? null : (parseInt(v, 10) || null);
  });

  rowsEl.addEventListener("change", function (e) {
    if (e.target.getAttribute("data-field") !== "kind") return;
    var p = find(e.target.closest("tr"));
    if (!p) return;
    p.kind = e.target.value;
    if (p.kind !== "audio") p.track = null;
    render();
  });

  function move(p, by) {
    var i = pending.indexOf(p), j = i + by;
    if (i < 0 || j < 0 || j >= pending.length) return;
    pending.splice(i, 1);
    pending.splice(j, 0, p);
    renumber();
    render();
  }

  // Keep track numbers in list order when every music file has one.
  function renumber() {
    var music = pending.filter(function (p) { return p.kind === "audio" && p.status !== "done"; });
    if (!music.length || music.some(function (p) { return p.track === null; })) return;
    var byDisc = {};
    music.forEach(function (p) {
      byDisc[p.disc] = (byDisc[p.disc] || 0) + 1;
      p.track = byDisc[p.disc];
    });
  }

  rowsEl.addEventListener("click", function (e) {
    var tr = e.target.closest("tr");
    var p = find(tr);
    if (!p) return;
    var btn = e.target.closest("button");
    if (!btn) return;
    if (btn.hasAttribute("data-move")) move(p, btn.getAttribute("data-move") === "up" ? -1 : 1);
    else if (btn.hasAttribute("data-remove")) {
      pending.splice(pending.indexOf(p), 1);
      render();
    } else if (btn.hasAttribute("data-remove-sub")) {
      p.subtitles.splice(Number(btn.getAttribute("data-remove-sub")), 1);
      render();
    } else if (btn.hasAttribute("data-retry")) {
      p.status = "ready";
      p.error = "";
      p.progress = 0;
      startUploads();
    }
  });

  // Drag rows by their grip to reorder (the rest of the row stays
  // selectable, so titles can be edited with the mouse).
  var dragged = null;
  rowsEl.addEventListener("mousedown", function (e) {
    var tr = e.target.closest("tr");
    if (!tr) return;
    tr.setAttribute("draggable", e.target.closest(".grip") && !tr.hasAttribute("data-locked") ? "true" : "false");
  });
  rowsEl.addEventListener("dragstart", function (e) {
    var tr = e.target.closest && e.target.closest("tr");
    if (!tr || tr.getAttribute("draggable") !== "true") return;
    dragged = find(tr);
    tr.classList.add("dragging");
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", String(dragged && dragged.id));
  });
  rowsEl.addEventListener("dragover", function (e) {
    if (!dragged) return;
    e.preventDefault();
    e.stopPropagation();
    var tr = e.target.closest("tr");
    var over = find(tr);
    if (!over || over === dragged || over.status === "done" || over.status === "uploading") return;
    var rect = tr.getBoundingClientRect();
    var after = e.clientY > rect.top + rect.height / 2;
    pending.splice(pending.indexOf(dragged), 1);
    pending.splice(pending.indexOf(over) + (after ? 1 : 0), 0, dragged);
    render();
    var again = rowsEl.querySelector('tr[data-id="' + dragged.id + '"]');
    if (again) again.classList.add("dragging");
  });
  rowsEl.addEventListener("drop", function (e) {
    if (!dragged) return;
    e.preventDefault();
    e.stopPropagation();
  });
  rowsEl.addEventListener("dragend", function () {
    if (!dragged) return;
    dragged = null;
    renumber();
    render();
  });

  clearBtn.addEventListener("click", function () {
    if (uploading) return;
    pending = [];
    looseSubs = [];
    skipped = [];
    statusEl.textContent = "";
    render();
  });

  // ---------- uploading ----------

  function errorFrom(xhr) {
    if (xhr.status === 0) return "The connection dropped.";
    if (xhr.status === 403) return "Editing is locked: unlock it again and retry.";
    if (xhr.status === 413) return "Too big for the server (check [media] max_upload_mb and the proxy's limit).";
    try {
      var j = JSON.parse(xhr.responseText);
      if (j && j.error) return j.error;
    } catch (e) { /* not JSON */ }
    return "Upload failed (" + xhr.status + ").";
  }

  function send(url, form, onProgress) {
    return new Promise(function (resolve, reject) {
      var xhr = new XMLHttpRequest();
      xhr.open("POST", url);
      xhr.setRequestHeader("Accept", "application/json");
      xhr.withCredentials = true;
      if (onProgress) {
        xhr.upload.onprogress = function (e) { if (e.lengthComputable) onProgress(e.loaded / e.total); };
      }
      xhr.onload = function () {
        if (xhr.status >= 200 && xhr.status < 300) {
          try { resolve(JSON.parse(xhr.responseText)); } catch (e) { resolve({}); }
        } else {
          reject(new Error(errorFrom(xhr)));
        }
      };
      xhr.onerror = function () { reject(new Error(errorFrom(xhr))); };
      xhr.onabort = function () { reject(new Error("Cancelled.")); };
      xhr.send(form);
    });
  }

  function uploadOne(p) {
    p.status = "uploading";
    p.progress = 0;
    render();
    var form = new FormData();
    form.append("kind", p.kind);
    form.append("title", p.title);
    form.append("disc", String(p.disc || 1));
    form.append("track", p.track === null ? "" : String(p.track));
    if (p.durationMs) form.append("duration_ms", String(p.durationMs));
    form.append("file", p.file, p.file.name);
    var url = "/admin/items/" + itemId + "/media/upload";
    return send(url, form, function (f) { p.progress = f; updateRow(p); })
      .then(function (created) {
        p.mediaId = created.id;
        return p.subtitles.reduce(function (chain, s) {
          return chain.then(function () {
            if (s.status === "done") return null;
            var sf = new FormData();
            sf.append("language", s.language);
            sf.append("label", s.label);
            sf.append("file", s.file, s.file.name);
            return send("/admin/items/" + itemId + "/media/" + created.id + "/subtitles", sf)
              .then(function () { s.status = "done"; })
              .catch(function (err) { s.status = "error"; p.subtitleError = err.message; });
          });
        }, Promise.resolve());
      })
      .then(function () {
        p.status = "done";
        p.progress = 1;
        if (p.subtitleError) p.error = "Uploaded, but subtitles failed: " + p.subtitleError;
      })
      .catch(function (err) {
        p.status = "error";
        p.error = err.message;
      })
      .then(render);
  }

  function startUploads() {
    if (uploading) return;
    uploading = true;
    render();
    var queue = pending.filter(function (p) { return p.status === "ready"; });
    var total = queue.length, done = 0;
    (function next() {
      var p = queue.shift();
      if (!p) return finished(total);
      statusEl.textContent = "Uploading " + (done + 1) + " of " + total + "…";
      uploadOne(p).then(function () { done++; refreshList(); next(); });
    })();
  }

  function finished(total) {
    uploading = false;
    var failed = pending.filter(function (p) { return p.status === "error"; }).length;
    statusEl.textContent = failed
      ? failed + " of " + total + " didn't upload — fix or retry them below."
      : (total ? "All " + total + " file" + (total === 1 ? "" : "s") + " uploaded." : "");
    pending = pending.filter(function (p) { return p.status !== "done" || p.error; });
    if (!failed) { skipped = []; looseSubs = []; }
    render();
    refreshList();
  }

  uploadBtn.addEventListener("click", startUploads);

  window.addEventListener("beforeunload", function (e) {
    if (!uploading) return;
    e.preventDefault();
    e.returnValue = "";
  });

  // Re-renders the list of files already in the library.
  function refreshList() {
    var list = $("media-list");
    if (!list) return;
    fetch("/admin/items/" + itemId + "/media/list", { credentials: "same-origin", headers: { "HX-Request": "true" } })
      .then(function (r) { return r.ok ? r.text() : null; })
      .then(function (html) {
        if (!html) return;
        var box = document.createElement("div");
        box.innerHTML = html;
        var fresh = box.firstElementChild;
        var current = $("media-list");
        if (!fresh || !current) return;
        current.replaceWith(fresh);
        if (window.htmx && typeof window.htmx.process === "function") window.htmx.process(fresh);
      })
      .catch(function () { /* the list will refresh on the next change */ });
  }

  // ---------- receiving files ----------

  ["dragenter", "dragover"].forEach(function (type) {
    zone.addEventListener(type, function (e) {
      if (dragged) return;
      e.preventDefault();
      zone.classList.add("over");
    });
  });
  ["dragleave", "drop"].forEach(function (type) {
    zone.addEventListener(type, function (e) {
      if (type === "dragleave" && zone.contains(e.relatedTarget)) return;
      zone.classList.remove("over");
    });
  });
  zone.addEventListener("drop", function (e) {
    if (dragged) return;
    e.preventDefault();
    filesFromDrop(e.dataTransfer).then(add);
  });
  // Dropping anywhere else on the page shouldn't open the file.
  document.addEventListener("dragover", function (e) { if (!dragged) e.preventDefault(); });
  document.addEventListener("drop", function (e) { if (!dragged) e.preventDefault(); });

  zone.addEventListener("keydown", function (e) {
    if (e.target !== zone || (e.key !== "Enter" && e.key !== " ")) return;
    e.preventDefault();
    $("pick-files").click();
  });

  ["pick-folder", "pick-files"].forEach(function (id) {
    $(id).addEventListener("change", function (e) {
      add(filesFromInput(e.target));
      e.target.value = "";
    });
  });

  // Exposed for the browser console and tests of the name rules.
  window.libraryStudio = { parseName: parseName, parseSubtitle: parseSubtitle, kindOf: kindOf };
})();
