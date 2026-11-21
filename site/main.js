(function () {
  "use strict";

  var root = document.documentElement;
  root.classList.add("js");

  var reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  // Terminal: the scan appears line by line, secrets catch the lamp as found.
  var lines = Array.prototype.slice.call(document.querySelectorAll(".t-line"));
  function revealAll() {
    lines.forEach(function (l) { l.classList.add("shown"); });
  }
  if (reduce) {
    revealAll();
  } else {
    var i = 0;
    var step = function () {
      if (i >= lines.length) return;
      var line = lines[i++];
      line.classList.add("shown");
      var wait = line.classList.contains("found") ? 520 : 90;
      window.setTimeout(step, wait);
    };
    window.setTimeout(step, 450);
  }

  // Safety list: the rows open left to right once the list comes into view.
  // The list itself is observed, not the clipped rows.
  var list = document.querySelector(".core");
  var rows = Array.prototype.slice.call(document.querySelectorAll(".core li"));
  function openRows() {
    rows.forEach(function (r) { r.classList.add("in"); });
  }
  if (list && "IntersectionObserver" in window && !reduce) {
    var io = new IntersectionObserver(function (entries) {
      if (entries.some(function (e) { return e.isIntersecting; })) {
        openRows();
        io.disconnect();
      }
    }, { threshold: 0.2 });
    io.observe(list);
  } else {
    openRows();
  }

  // Parallax and depth gauge, one rAF pass per scroll.
  var copy = document.querySelector(".hero-copy");
  var term = document.querySelector(".term");
  var hero = document.querySelector(".hero");
  var bar = document.querySelector(".bar");
  var depth = document.getElementById("depth");
  var gauge = document.querySelector(".gauge");
  var dot = document.querySelector(".gauge-dot");
  var line = document.querySelector(".gauge-line");
  var edges = Array.prototype.slice.call(document.querySelectorAll(".edge"));
  var ticking = false;

  function update() {
    ticking = false;
    var y = window.scrollY || window.pageYOffset;
    var vh = window.innerHeight;
    var max = Math.max(1, document.documentElement.scrollHeight - vh);
    var p = Math.min(1, Math.max(0, y / max));

    if (depth) depth.textContent = String(Math.round(p * 38));
    if (dot && line) dot.style.top = "calc(" + (p * 100) + "% - 5px)";
    if (bar && hero) {
      var dark = y > hero.offsetHeight - 80;
      bar.classList.toggle("on-dark", dark);
      if (gauge) gauge.classList.toggle("on-dark", y > hero.offsetHeight - vh * 0.5);
    }

    if (reduce) return;

    if (y < vh * 1.4) {
      if (copy) copy.style.transform = "translate3d(0," + (y * 0.12) + "px,0)";
      if (term) term.style.transform = "translate3d(0," + (y * -0.05) + "px,0)";
    }

    edges.forEach(function (svg) {
      var r = svg.getBoundingClientRect();
      if (r.bottom < -200 || r.top > vh + 200) return;
      var offset = r.top + r.height / 2 - vh / 2;
      var paths = svg.querySelectorAll("path");
      for (var k = 0; k < paths.length; k++) {
        var d = parseFloat(paths[k].getAttribute("data-drift")) || 0;
        paths[k].style.transform = "translate3d(" + (offset * d * -2) + "px,0,0)";
      }
    });
  }

  function onScroll() {
    if (!ticking) {
      ticking = true;
      window.requestAnimationFrame(update);
    }
  }

  window.addEventListener("scroll", onScroll, { passive: true });
  window.addEventListener("resize", onScroll);
  update();
})();
