// "Fill from MusicBrainz" in the upload studio (/admin/items/{id}/media).
// Loaded after media-upload.js, whose window.libraryStudio hooks give the
// waiting music files and apply the result.
//
// The server does the work (src/web/fill.rs, src/trackmatch.rs): it searches
// MusicBrainz, fetches the closest track lists, ranks the releases by how
// well they fit the files' lengths, and matches files to tracks. This script
// only draws the candidates and the preview, and fills the table on Apply.
(function () {
  "use strict";

  var panel = document.getElementById("mb-fill");
  var studio = window.libraryStudio;
  var studioEl = document.getElementById("studio");
  if (!panel || !studio || !studioEl) return;

  var $ = function (id) { return document.getElementById(id); };
  var base = "/admin/items/" + studioEl.getAttribute("data-item") + "/media/musicbrainz";
  var enabled = panel.getAttribute("data-enabled") === "1";
  var barcode = panel.getAttribute("data-barcode") || "";
  var form = $("mb-search");
  var queryEl = $("mb-query");
  var statusEl = $("mb-status");
  var listEl = $("mb-candidates");
  var previewEl = $("mb-preview");

  var searched = false;   // searched once for this batch of files
  var busy = false;
  var result = null;      // the match being previewed
  var filesByKey = {};

  // ---------- small helpers ----------

  function el(tag, attrs, children) {
    var e = document.createElement(tag);
    Object.keys(attrs || {}).forEach(function (k) {
      if (k === "text") e.textContent = attrs[k];
      else if (k === "className") e.className = attrs[k];
      else e.setAttribute(k, attrs[k]);
    });
    (children || []).forEach(function (c) {
      if (c === null || c === undefined) return;
      e.appendChild(typeof c === "string" ? document.createTextNode(c) : c);
    });
    return e;
  }

  function clock(ms) {
    if (!ms) return "—";
    var s = Math.round(ms / 1000), h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60;
    s = s % 60;
    return (h ? h + ":" + String(m).padStart(2, "0") : m) + ":" + String(s).padStart(2, "0");
  }

  function plural(n, one, many) { return n + " " + (n === 1 ? one : many); }

  function post(url, body) {
    return fetch(url, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify(body)
    }).then(function (r) {
      return r.json().catch(function () { return {}; }).then(function (j) {
        if (r.ok) return j;
        if (r.status === 403 && !j.error) throw new Error("Editing is locked: unlock it again and retry.");
        throw new Error(j.error || "MusicBrainz lookup failed (" + r.status + ").");
      });
    });
  }

  function say(text, isError) {
    statusEl.textContent = text;
    statusEl.classList.toggle("err", !!isError);
  }

  function files() {
    var list = studio.files();
    filesByKey = {};
    list.forEach(function (f) { filesByKey[f.key] = f; });
    return list;
  }

  // ---------- showing and hiding with the waiting files ----------

  studio.onChange(function () {
    var count = studio.files().length;
    panel.hidden = count === 0;
    if (count === 0) {
      searched = false;
      result = null;
      if (enabled) {
        listEl.textContent = "";
        previewEl.hidden = true;
        previewEl.textContent = "";
        say("");
      }
      return;
    }
    // Search once the lengths are known, so the ranking can use them.
    if (enabled && !searched && !busy && !studio.measuring()) {
      searched = true;
      search();
    }
  });

  if (!enabled) return;

  form.addEventListener("submit", function (e) {
    e.preventDefault();
    searched = true;
    search();
  });

  // ---------- searching ----------

  function search() {
    if (busy) return;
    var query = queryEl.value.trim();
    if (!query && !barcode) {
      say("Type an artist and album, a barcode, or a MusicBrainz link.", true);
      return;
    }
    busy = true;
    result = null;
    previewEl.hidden = true;
    listEl.setAttribute("aria-busy", "true");
    say("Searching MusicBrainz… It answers one question a second, so this can take a few seconds.");
    // The entry's barcode goes with the search it was prefilled for.
    var withBarcode = queryEl.value === queryEl.defaultValue ? barcode : "";
    post(base + "/search", { query: query, barcode: withBarcode, files: files() })
      .then(showCandidates)
      .catch(function (err) { listEl.textContent = ""; say(err.message, true); })
      .then(function () { busy = false; listEl.removeAttribute("aria-busy"); });
  }

  function fitClass(fit) {
    if (!fit) return "fit-none";
    if (fit.compared && fit.files && fit.good + fit.check === 0) return "fit-poor";
    if (fit.score >= 80) return "fit-good";
    if (fit.score >= 50) return "fit-check";
    return "fit-poor";
  }

  function showCandidates(data) {
    listEl.textContent = "";
    var cands = data.candidates || [];
    cands.forEach(function (c, i) {
      var meta = [c.artist, c.year, c.country, c.format].filter(Boolean);
      meta.push(c.discTracks.length > 1 ? c.discTracks.join(" + ") + " tracks" : plural(c.trackCount, "track", "tracks"));
      if (c.totalMs) meta.push(clock(c.totalMs));
      var badge = null;
      if (c.fit) {
        badge = el("span", {
          className: "mb-fit " + fitClass(c.fit),
          title: "Fit " + c.fit.score + "/100" + (c.fit.compared ? "" : " (lengths not compared: only the track count)"),
          text: c.fit.summary
        });
      }
      var pick = el("button", { type: "button", className: "mb-pick", "data-release": c.id }, [
        el("span", { className: "mb-cand-main" }, [
          el("span", { className: "mb-cand-title", text: c.title }),
          i === 0 && c.fit && c.fit.score >= 80 ? el("span", { className: "mb-best", text: "Best fit" }) : null,
          el("span", { className: "mb-cand-meta", text: meta.join(" · ") })
        ]),
        badge
      ]);
      var link = el("a", { className: "small mb-link", href: c.url, target: "_blank", rel: "noopener noreferrer", text: "On MusicBrainz ↗" });
      listEl.appendChild(el("li", { className: "mb-candidate" }, [pick, link]));
    });
    var notes = (data.notes || []).join(" ");
    if (!cands.length) say(notes || "MusicBrainz found nothing for that.", true);
    else say(plural(cands.length, "release", "releases") + " found. Pick the one you ripped to see how your files line up." + (notes ? " " + notes : ""));
  }

  listEl.addEventListener("click", function (e) {
    var btn = e.target.closest("button[data-release]");
    if (!btn || busy) return;
    Array.prototype.forEach.call(listEl.children, function (li) {
      li.classList.toggle("picked", li.contains(btn));
    });
    match(btn.getAttribute("data-release"));
  });

  // ---------- previewing a match ----------

  function match(id) {
    busy = true;
    say("Matching your files to the tracks…");
    post(base + "/match", { release: id, files: files() })
      .then(function (data) {
        result = data;
        showPreview();
        say("");
      })
      .catch(function (err) { say(err.message, true); })
      .then(function () { busy = false; });
  }

  var CONFIDENCE = {
    good: { text: "✓ Good", cls: "good" },
    check: { text: "Check", cls: "check" },
    none: { text: "No match", cls: "none" }
  };

  function showPreview() {
    var r = result.release;
    var matches = result.matches.slice().sort(function (a, b) {
      var an = a.confidence === "none", bn = b.confidence === "none";
      return (an - bn) || ((a.disc || 0) - (b.disc || 0)) || ((a.track || 0) - (b.track || 0));
    });
    var applying = matches.filter(function (m) { return m.confidence !== "none"; });
    var hasArtists = applying.some(function (m) { return m.artist; });
    var itemYear = panel.getAttribute("data-item-year");
    var itemCreator = panel.getAttribute("data-item-creator");
    var multiDisc = r.media.length > 1;

    previewEl.textContent = "";
    previewEl.appendChild(el("div", { className: "mb-preview-head" }, [
      el("h4", {}, [el("i", { text: r.title }), r.artist ? " — " + r.artist : ""]),
      el("span", { className: "mb-fit " + fitClass(result.fit), text: result.fit.summary })
    ]));

    var rows = matches.map(function (m) {
      var f = filesByKey[m.key] || { name: m.key };
      var c = CONFIDENCE[m.confidence];
      var none = m.confidence === "none";
      return el("tr", { className: "mb-row-" + c.cls, "data-key": m.key }, [
        el("td", { className: "mono small mb-file", text: f.name }),
        el("td", { className: "mono small", text: clock(f.durationMs) }),
        el("td", { className: "mono", text: none || !m.disc ? "" : String(m.disc) }),
        el("td", { className: "mono", text: none || !m.track ? "" : String(m.track) }),
        el("td", { className: "mb-title" }, [
          none ? el("span", { className: "muted", text: m.title ? "(" + m.title + "?)" : "—" }) : m.title,
          m.artist && !none ? el("span", { className: "small muted mb-artist", text: m.artist }) : null
        ]),
        el("td", { className: "mono small", text: clock(m.lengthMs) }),
        el("td", { className: "mb-conf-cell" }, [
          el("span", { className: "mb-conf " + c.cls, text: c.text }),
          m.note ? el("span", { className: "small mb-note", text: m.note }) : null
        ])
      ]);
    });
    previewEl.appendChild(el("div", { className: "table-scroll" }, [
      el("table", { className: "pending-table mb-preview-table" }, [
        el("thead", {}, [el("tr", {}, ["File", "Length", "Disc", "Track", "Title", "Track length", "Match"].map(function (h) {
          return el("th", { scope: "col", text: h });
        }))]),
        el("tbody", {}, rows)
      ])
    ]));

    if (result.missing.length) {
      previewEl.appendChild(el("p", {
        className: "small muted",
        text: "No file for " + result.missing.map(function (t) {
          return (multiDisc ? t.disc + "-" : "") + t.track + ". " + t.title + (t.lengthMs ? " (" + clock(t.lengthMs) + ")" : "");
        }).join(", ") + "."
      }));
    }
    var skipped = matches.length - applying.length;
    if (skipped) {
      previewEl.appendChild(el("p", {
        className: "small muted",
        text: plural(skipped, "file is", "files are") + " left as they are: nothing on this release fits them well enough."
      }));
    }

    var options = el("div", { className: "mb-options" });
    function option(id, label, checked) {
      var box = el("input", { type: "checkbox", id: id });
      box.checked = !!checked;
      options.appendChild(el("label", { className: "mb-option", for: id }, [box, " " + label]));
    }
    option("mb-overwrite", "Overwrite titles and numbers I've typed myself", false);
    if (hasArtists) option("mb-artists", "Add each track's artist to its title (“Title — Artist”)", false);
    if (!itemYear && r.year) option("mb-year", "Also set this entry's year to " + r.year, true);
    if (!itemCreator && r.artist) option("mb-creator", "Also set this entry's artist to " + r.artist, true);
    previewEl.appendChild(options);

    var apply = el("button", {
      type: "button", className: "btn accent", id: "mb-apply",
      text: applying.length ? "Apply to " + plural(applying.length, "file", "files") : "Nothing to apply"
    });
    apply.disabled = !applying.length;
    var back = el("button", { type: "button", className: "btn", id: "mb-cancel", text: "Cancel" });
    previewEl.appendChild(el("div", { className: "row-actions mb-actions" }, [back, apply]));
    previewEl.hidden = false;
    previewEl.scrollIntoView({ block: "nearest" });
  }

  previewEl.addEventListener("click", function (e) {
    if (e.target.id === "mb-cancel") {
      previewEl.hidden = true;
      result = null;
      Array.prototype.forEach.call(listEl.children, function (li) { li.classList.remove("picked"); });
    } else if (e.target.id === "mb-apply" && result) {
      applyResult();
    }
  });

  function checked(id) {
    var box = $(id);
    return !!(box && box.checked);
  }

  function applyResult() {
    var r = result.release;
    var withArtists = checked("mb-artists");
    var changes = result.matches
      .filter(function (m) { return m.confidence !== "none"; })
      .map(function (m) {
        return {
          key: m.key, disc: m.disc, track: m.track,
          title: withArtists && m.artist ? m.title + " — " + m.artist : m.title,
          confidence: m.confidence, note: m.note
        };
      });
    var count = studio.apply(changes, checked("mb-overwrite"));
    var done = "Filled " + plural(count, "file", "files") + " from “" + r.title + "”. Check the list below, then upload.";
    var item = {};
    if (checked("mb-year")) item.year = r.year;
    if (checked("mb-creator")) item.creator = r.artist;
    previewEl.hidden = true;
    result = null;
    say(done);
    if (item.year || item.creator) {
      post(base + "/item", item)
        .then(function (saved) {
          panel.setAttribute("data-item-year", saved.year ? String(saved.year) : "");
          panel.setAttribute("data-item-creator", saved.creator || "");
          say(done + " The entry's " + [item.creator ? "artist" : "", item.year ? "year" : ""].filter(Boolean).join(" and ") + " was filled in too.");
        })
        .catch(function (err) { say(done + " The entry couldn't be updated: " + err.message, true); });
    }
  }
})();
