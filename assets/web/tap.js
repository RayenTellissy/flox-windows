;(function () {
  var post = function (t, d) { try { window.chrome.webview.postMessage(JSON.stringify({ type: t, data: d })) } catch (e) {} }
  var scan = function (t, u) {
    if (typeof t !== "string") return
    if (u && String(u).indexOf("/api/") >= 0) post("FLOX_API", { url: String(u), body: t.slice(0, 4000) })
    if (t.indexOf("playlist") < 0 && t.indexOf("qualities") < 0) return
    try {
      var d = JSON.parse(t); var st = d && d.stream
      if (st && st.playlist) post("FLOX_PLAYLIST", { url: st.playlist, kind: st.type || "", headers: st.playlistHeaders || {}, meta: st.playbackMetadata || {} })
      else if (st && st.qualities) {
        var best = null, bestQ = 0
        Object.keys(st.qualities).forEach(function (k) { var q = parseInt(k, 10) || 0; if (q >= bestQ && st.qualities[k] && st.qualities[k].url) { bestQ = q; best = st.qualities[k] } })
        if (best) post("FLOX_PLAYLIST", { url: best.url, kind: "file", headers: best.headers || {}, meta: { resolutions: [String(bestQ)], codecName: "" } })
      }
    } catch (e) {}
  }
  var of = window.fetch
  window.fetch = function () { return of.apply(window, arguments).then(function (res) { try { var u = res.url; res.clone().text().then(function (t) { scan(t, u) }) } catch (e) {}; return res }) }
  var xs = XMLHttpRequest.prototype.send
  XMLHttpRequest.prototype.send = function () { var x = this; x.addEventListener("load", function () { try { scan(x.responseText, x.responseURL) } catch (e) {} }); return xs.apply(this, arguments) }
})()
