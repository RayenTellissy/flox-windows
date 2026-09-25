// Remote navigation over the player's own buttons. Injected after every page load.
(function () {
  if (window.__flox) return
  var SEL = 'button,[role=button],[role=menuitem],[role=menuitemradio],[role=option],[role=tab],a[href],input[type=range],[tabindex]:not([tabindex="-1"]),[onclick],.cursor-pointer'
  var current = null
  var active = false
  var keepTimer = null

  try {
    var style = document.createElement("style")
    style.textContent = ".flox-focus,.flox-focus:focus,.flox-focus:focus-visible{outline:2px solid #fafafa !important;outline-offset:-2px !important;border-radius:0 !important;box-shadow:none !important}"
    ;(document.head || document.documentElement).appendChild(style)
  } catch (e) {}

  function video() { return document.querySelector("video") }
  function rect(el) { return el.getBoundingClientRect() }
  function visible(el) {
    try {
      if (el.disabled) return false
      var r = rect(el)
      if (r.width < 4 || r.height < 4) return false
      if (r.bottom < 0 || r.right < 0 || r.top > innerHeight || r.left > innerWidth) return false
      var cs = getComputedStyle(el)
      return cs.visibility !== "hidden" && cs.opacity !== "0" && cs.pointerEvents !== "none"
    } catch (e) { return false }
  }
  function all(sel) { return Array.prototype.slice.call(document.querySelectorAll(sel)) }
  function onTop(el) {
    try {
      var r = rect(el)
      var hit = document.elementFromPoint(Math.min(innerWidth - 1, Math.max(0, (r.left + r.right) / 2)), Math.min(innerHeight - 1, Math.max(0, (r.top + r.bottom) / 2)))
      return !!hit && (hit === el || el.contains(hit))
    } catch (e) { return true }
  }
  function panels() { return all('[data-panel-open="true"],[role=dialog],[role=menu],[role=listbox]').filter(visible).filter(onTop) }
  var reactKey = null
  function reactClickable(el) {
    try {
      if (!reactKey) {
        var keys = Object.keys(el)
        for (var i = 0; i < keys.length; i++) if (keys[i].indexOf("__reactProps") === 0) { reactKey = keys[i]; break }
        if (!reactKey) return false
      }
      var p = el[reactKey]
      return !!(p && (p.onClick || p.onPointerDown || p.onMouseDown))
    } catch (e) { return false }
  }
  function collect() {
    var list = all(SEL)
    var seen = new Set(list)
    var every = document.body ? document.body.getElementsByTagName("*") : []
    for (var i = 0; i < every.length; i++) {
      var e = every[i]
      if (seen.has(e) || e.tagName === "VIDEO") continue
      if (reactClickable(e) && getComputedStyle(e).cursor === "pointer") { list.push(e); seen.add(e) }
    }
    return list
  }
  function candidates() {
    var list = collect().filter(visible).filter(onTop)
    list = list.filter(function (e) {
      if (e.tagName === "BUTTON" || e.tagName === "A" || e.tagName === "INPUT") return true
      var inner = e.querySelectorAll(SEL)
      if (!inner.length) return true
      var r = rect(e)
      var covered = 0
      for (var i = 0; i < inner.length; i++) covered += rect(inner[i]).width
      return covered < r.width * 0.6
    })
    var p = panels()
    if (p.length) {
      var top = p[p.length - 1]
      var inside = list.filter(function (e) { return top.contains(e) })
      if (inside.length) return inside
    }
    return list
  }
  function center(r) { return { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 } }
  function setFocus(el) {
    if (current && current !== el) current.classList.remove("flox-focus")
    current = el
    if (!el) return
    el.classList.add("flox-focus")
    try { el.focus({ preventScroll: true }) } catch (e) {}
    try { el.scrollIntoView({ block: "nearest", inline: "nearest" }) } catch (e) {}
  }
  function wake() {
    try {
      var ev = new MouseEvent("mousemove", { bubbles: true, clientX: innerWidth / 2, clientY: innerHeight - 40 })
      document.dispatchEvent(ev)
      var v = video()
      if (v && v.parentElement) v.parentElement.dispatchEvent(ev)
    } catch (e) {}
  }
  function first(list) {
    var best = null, br = null
    list.forEach(function (e) {
      var r = rect(e)
      if (!best || r.top > br.top + 8 || (Math.abs(r.top - br.top) <= 8 && r.left < br.left)) { best = e; br = r }
    })
    return best
  }
  function nav(dir) {
    var list = candidates()
    if (!list.length) return false
    if (!current || !visible(current) || list.indexOf(current) < 0) { setFocus(first(list)); return true }
    var c = center(rect(current))
    var best = null, bestScore = Infinity
    list.forEach(function (e) {
      if (e === current) return
      var o = center(rect(e))
      var dx = o.x - c.x, dy = o.y - c.y
      var primary, secondary
      if (dir === "left") { primary = -dx; secondary = Math.abs(dy) }
      else if (dir === "right") { primary = dx; secondary = Math.abs(dy) }
      else if (dir === "up") { primary = -dy; secondary = Math.abs(dx) }
      else { primary = dy; secondary = Math.abs(dx) }
      if (primary <= 2) return
      var score = primary + secondary * 2.5
      if (secondary > primary * 3 && secondary > 60) score += 1000
      if (score < bestScore) { bestScore = score; best = e }
    })
    if (best) { setFocus(best); return true }
    return false
  }
  function activate() {
    if (!current) return false
    var el = current
    try { el.click() } catch (e) {}
    setTimeout(function () { if (!visible(el)) { current = null; nav("down") } }, 250)
    return true
  }
  function panelOpen() { return panels().length > 0 }
  function key(k, code) {
    var target = document.activeElement && document.activeElement !== document.body ? document.activeElement : document.body
    var ev = new KeyboardEvent("keydown", { key: k, code: code || k, bubbles: true, cancelable: true })
    target.dispatchEvent(ev)
  }
  function closePanel() {
    var was = panelOpen()
    key("Escape", "Escape")
    var closer = all("button").filter(visible).filter(function (b) { return /close/i.test(b.getAttribute("aria-label") || "") })[0]
    if (closer) closer.click()
    if (current) current.classList.remove("flox-focus")
    current = null
    return was
  }
  function clickLabel(pattern) {
    var re = new RegExp(pattern, "i")
    wake()
    var b = all("button").filter(visible).filter(function (x) {
      var svg = x.querySelector("svg")
      return re.test((x.getAttribute("aria-label") || "") + " " + x.textContent + " " + (svg ? svg.getAttribute("class") || "" : ""))
    })[0]
    if (!b) return false
    b.click()
    setTimeout(function () { current = null; nav("down") }, 300)
    return true
  }
  function enter() {
    active = true
    wake()
    if (!keepTimer) keepTimer = setInterval(wake, 1500)
    // controls fade in after the wake event, so retry until something is focusable
    ;[150, 400, 800, 1500].forEach(function (ms) {
      setTimeout(function () { if (active && (!current || !visible(current))) nav("down") }, ms)
    })
  }
  function exit() {
    active = false
    if (keepTimer) { clearInterval(keepTimer); keepTimer = null }
    if (current) current.classList.remove("flox-focus")
    current = null
    try { if (document.activeElement) document.activeElement.blur() } catch (e) {}
  }
  function state() {
    var v = video()
    return v ? { currentTime: v.currentTime || 0, duration: v.duration || 0, paused: v.paused, ended: v.ended } : null
  }
  // the player autoplays muted without a user gesture; unmute once playback is running
  var unmuted = false
  setInterval(function () {
    try {
      var v = video()
      if (!unmuted && v && !v.paused && v.currentTime > 0 && v.muted) { v.muted = false; unmuted = true }
      var s = state()
      if (s && window.FloxBridge) window.FloxBridge.onMessage(JSON.stringify({ type: "FLOX_TICK", data: s }))
    } catch (e) {}
  }, 2000)

  window.__floxApplyStart = function (sec) {
    var tries = 0
    var t = setInterval(function () {
      var v = video()
      tries++
      if (v && v.readyState >= 1 && v.duration > 0) {
        if (sec > 0 && sec < v.duration - 5 && Math.abs(v.currentTime - sec) > 5) v.currentTime = sec
        clearInterval(t)
      } else if (tries > 60) clearInterval(t)
    }, 500)
  }

  window.__floxApplySpeed = function (rate) {
    var tries = 0
    var t = setInterval(function () {
      var v = video()
      tries++
      if (v && v.readyState >= 1) {
        v.playbackRate = rate
        clearInterval(t)
      } else if (tries > 60) clearInterval(t)
    }, 500)
  }

  window.__flox = {
    nav: nav, activate: activate, enter: enter, exit: exit, closePanel: closePanel, panelOpen: panelOpen,
    clickLabel: clickLabel, key: key, wake: wake, state: state,
    isActive: function () { return active },
    toggle: function () { var v = video(); if (!v) return; if (v.paused) v.play(); else v.pause() },
    seek: function (d) { var v = video(); if (v && v.duration) v.currentTime = Math.max(0, Math.min(v.duration - 1, v.currentTime + d)) }
  }
})()
