// Small progressive enhancements. Everything works without this file;
// it only makes a few things nicer (inline panels on wide screens,
// photo previews, barcode scanning).
(function () {
  "use strict";
  var wide = window.matchMedia("(min-width: 720px)");

  // Links that open an inline panel on wide screens and a page on phones.
  document.addEventListener("click", function (e) {
    var a = e.target.closest("[data-panel]");
    if (!a) return;
    var panel = document.getElementById(a.getAttribute("data-panel"));
    if (!panel) return;
    var always = a.hasAttribute("data-panel-always");
    if (!always && !wide.matches) return;
    e.preventDefault();
    var open = panel.hasAttribute("hidden");
    panel.toggleAttribute("hidden", !open);
    a.setAttribute("aria-expanded", open ? "true" : "false");
    var label = a.querySelector("[data-when-open]");
    if (label) a.classList.toggle("is-open", open);
    if (open) {
      var focus = panel.querySelector("input, select, textarea, button");
      if (focus) focus.focus();
    }
  });

  // Close buttons inside panels.
  document.addEventListener("click", function (e) {
    var b = e.target.closest("[data-close]");
    if (!b) return;
    var panel = document.getElementById(b.getAttribute("data-close"));
    if (panel) { e.preventDefault(); panel.setAttribute("hidden", ""); }
  });

  // Forms marked data-mobile-page do a normal page load on phones instead
  // of an inline htmx swap (long lists get their own page).
  document.addEventListener("htmx:before:request", function (e) {
    var ctx = e.detail && e.detail.ctx;
    var el = ctx && ctx.sourceElement;
    if (!el || wide.matches) return;
    var form = el.closest && el.closest("form[data-mobile-page]");
    if (form) {
      e.preventDefault();
      form.submit();
    }
  });

  // Preview photos picked for upload.
  document.addEventListener("change", function (e) {
    var input = e.target;
    if (!input.matches || !input.matches("input[type=file][data-preview]")) return;
    var box = document.getElementById(input.getAttribute("data-preview"));
    if (!box) return;
    box.textContent = "";
    Array.prototype.slice.call(input.files || [], 0, 12).forEach(function (f) {
      if (!/^image\//.test(f.type)) return;
      var img = document.createElement("img");
      img.alt = "";
      img.src = URL.createObjectURL(f);
      img.onload = function () { URL.revokeObjectURL(img.src); };
      box.appendChild(img);
    });
  });

  // ---- barcode scanning ----
  function fillAndLookup(code) {
    var input = document.getElementById("lookup-q");
    if (!input) return;
    input.value = code;
    var form = input.form;
    if (form) {
      if (form.requestSubmit) form.requestSubmit(); else form.submit();
    }
  }

  function scanFromPhoto(file, status) {
    var fd = new FormData();
    fd.append("photo", file);
    status.textContent = "Reading the barcode…";
    fetch("/admin/barcode", { method: "POST", body: fd, credentials: "same-origin" })
      .then(function (r) { return r.text().then(function (t) { return { ok: r.ok, text: t }; }); })
      .then(function (r) {
        if (r.ok) { status.textContent = ""; fillAndLookup(r.text.trim()); }
        else { status.textContent = r.text; }
      })
      .catch(function () { status.textContent = "Couldn't read that photo."; });
  }

  var stream = null;
  function stopCamera(panel) {
    if (stream) { stream.getTracks().forEach(function (t) { t.stop(); }); stream = null; }
    if (panel) panel.setAttribute("hidden", "");
  }

  document.addEventListener("click", function (e) {
    var btn = e.target.closest("[data-scan]");
    if (!btn) return;
    e.preventDefault();
    var panel = document.getElementById("scan-panel");
    var status = document.getElementById("scan-status");
    var fileInput = document.getElementById("scan-file");
    var live = "BarcodeDetector" in window && navigator.mediaDevices && navigator.mediaDevices.getUserMedia;
    if (!live) { fileInput.click(); return; }
    var detector;
    try {
      detector = new window.BarcodeDetector({ formats: ["ean_13", "ean_8", "upc_a", "upc_e"] });
    } catch (err) { fileInput.click(); return; }
    panel.removeAttribute("hidden");
    var video = panel.querySelector("video");
    status.textContent = "Point the camera at the barcode.";
    navigator.mediaDevices.getUserMedia({ video: { facingMode: "environment" }, audio: false })
      .then(function (s) {
        stream = s;
        video.srcObject = s;
        return video.play();
      })
      .then(function tick() {
        if (!stream) return;
        detector.detect(video).then(function (codes) {
          if (codes.length) { stopCamera(panel); fillAndLookup(codes[0].rawValue); }
          else { setTimeout(tick, 250); }
        }).catch(function () { setTimeout(tick, 500); });
      })
      .catch(function () { stopCamera(panel); fileInput.click(); });
  });

  document.addEventListener("click", function (e) {
    if (e.target.closest("[data-scan-stop]")) { e.preventDefault(); stopCamera(document.getElementById("scan-panel")); }
  });

  document.addEventListener("change", function (e) {
    if (e.target.id !== "scan-file" || !e.target.files.length) return;
    scanFromPhoto(e.target.files[0], document.getElementById("scan-status"));
    e.target.value = "";
  });

  // Confirm destructive buttons without inline JS (CSP-friendly).
  document.addEventListener("submit", function (e) {
    var msg = e.target.getAttribute("data-confirm");
    if (msg && !window.confirm(msg)) e.preventDefault();
  });
})();
