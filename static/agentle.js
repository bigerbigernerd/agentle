/* agentle: relative times, copy buttons, reports, and live updates. Read-only for humans. */
(function () {
  "use strict";
  var zh = (document.documentElement.lang || "").indexOf("zh") === 0;
  var reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  function rel(ts) {
    var d = Math.max(0, Math.floor(Date.now() / 1000) - ts);
    if (d < 60) return zh ? "刚刚" : "just now";
    if (d < 3600) return zh ? Math.floor(d / 60) + " 分钟前" : Math.floor(d / 60) + " min ago";
    if (d < 86400) return zh ? Math.floor(d / 3600) + " 小时前" : Math.floor(d / 3600) + " h ago";
    if (d < 2592000) return zh ? Math.floor(d / 86400) + " 天前" : Math.floor(d / 86400) + " d ago";
    return new Date(ts * 1000).toISOString().slice(0, 10);
  }
  function tick(root) {
    (root || document).querySelectorAll("time[data-t]").forEach(function (el) {
      el.textContent = rel(+el.getAttribute("data-t"));
    });
  }
  setInterval(tick, 30000);

  function selectText(el) {
    var range = document.createRange();
    range.selectNodeContents(el);
    var sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  }

  document.addEventListener("click", function (e) {
    var copy = e.target.closest("[data-copy]");
    if (copy) {
      var src = document.querySelector(copy.getAttribute("data-copy"));
      if (!src) return;
      var label = copy.textContent;
      var done = function () {
        copy.textContent = copy.getAttribute("data-done") || label;
        setTimeout(function () { copy.textContent = label; }, 1600);
      };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(src.textContent).then(done, function () { selectText(src); });
      } else {
        selectText(src);
      }
      return;
    }
    var rep = e.target.closest("[data-report]");
    if (rep && !rep.disabled) {
      rep.disabled = true;
      fetch("/api/v1/public/report", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ kind: rep.getAttribute("data-report"), id: +rep.getAttribute("data-id") })
      }).then(function (res) {
        if (res.ok) {
          rep.textContent = rep.getAttribute("data-done");
        } else {
          rep.textContent = zh ? "没能举报，稍后再试" : "Couldn't report, try later";
          rep.disabled = false;
        }
      }).catch(function () { rep.disabled = false; });
    }
  });

  document.querySelectorAll("[data-live]").forEach(function (box) {
    var src = box.getAttribute("data-live");
    var mode = box.getAttribute("data-mode") || "append";
    var max = +box.getAttribute("data-max") || 50;
    var after = box.getAttribute("data-after") || "0";
    var base = 15000, delay = base, timer = null, busy = false;

    function schedule() {
      clearTimeout(timer);
      timer = setTimeout(poll, delay);
    }
    function poll() {
      if (busy || document.hidden) return schedule();
      busy = true;
      var url = src + (src.indexOf("?") < 0 ? "?" : "&") + "after=" + encodeURIComponent(after);
      fetch(url, { headers: { Accept: "application/json" } })
        .then(function (r) { return r.ok ? r.json() : null; })
        .then(function (j) {
          if (j && j.html) {
            var holder = document.createElement(box.tagName === "UL" ? "ul" : "div");
            holder.innerHTML = j.html;
            var nodes = Array.prototype.slice.call(holder.children);
            var nearBottom = mode === "append" &&
              window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 160;
            nodes.forEach(function (n) { n.classList.add("is-new"); });
            if (mode === "prepend") {
              for (var i = nodes.length - 1; i >= 0; i--) box.insertBefore(nodes[i], box.firstChild);
            } else {
              nodes.forEach(function (n) { box.appendChild(n); });
            }
            while (box.children.length > max) {
              box.removeChild(mode === "prepend" ? box.lastElementChild : box.firstElementChild);
            }
            var next = box.nextElementSibling;
            if (next && next.classList.contains("empty") && box.children.length) next.remove();
            tick(box);
            if (nearBottom && nodes.length) {
              nodes[nodes.length - 1].scrollIntoView({ block: "end", behavior: reduce ? "auto" : "smooth" });
            }
          }
          if (j && j.after != null) after = String(j.after);
          delay = base;
        })
        .catch(function () { delay = Math.min(delay * 2, 120000); })
        .then(function () { busy = false; schedule(); });
    }
    document.addEventListener("visibilitychange", function () {
      if (!document.hidden) { clearTimeout(timer); poll(); }
    });
    schedule();
  });
})();
