// Prototype for conceptify-z9y.1: does WKWebView/AVFoundation play AND seek
// an mp4 served through a custom URL scheme, from inside a sandboxed
// opaque-origin iframe that is itself served through another custom scheme,
// under the production artifact CSP (+ media-src)?
//
// Topology mirrors production exactly:
//   shell (loadHTMLString)  ~ tauri://localhost app shell
//     <iframe sandbox="allow-scripts" src="artifact://localhost/doc">
//       (served by ArtifactSchemeHandler, with per-response CSP header)
//       <video src="cfy-asset://localhost/test.mp4">
//         (served by AssetSchemeHandler with HTTP Range semantics,
//          mirroring tauri's asset protocol incl. the 1 MiB chunk cap)
//
// Prints TEST: lines and exits 0 on full success, 1 on failure/timeout.

import AppKit
import WebKit

let videoPath = CommandLine.arguments.count > 1
  ? CommandLine.arguments[1]
  : FileManager.default.currentDirectoryPath + "/test.mp4"

// The production CSP from artifact_protocol.rs, plus the media-src carve-out
// under test. Try the scheme-source form first.
let mediaSrc = ProcessInfo.processInfo.environment["MEDIA_SRC"] ?? "cfy-asset:"
let csp = "default-src 'none'; "
  + "script-src 'unsafe-inline' https://cdn.jsdelivr.net; "
  + "style-src 'unsafe-inline' https://cdn.jsdelivr.net; "
  + "font-src data: https://cdn.jsdelivr.net; "
  + "img-src data:; "
  + "media-src \(mediaSrc); "
  + "connect-src 'none'"

let artifactHTML = """
<!doctype html>
<html><head><meta charset="utf-8"><title>video test</title></head>
<body>
<h1>video seek test</h1>
<video id="v" src="cfy-asset://localhost/test.mp4" preload="auto" muted playsinline></video>
<script>
(function () {
  const v = document.getElementById('v');
  const report = (step, ok, detail) =>
    parent.postMessage({ step, ok, detail: String(detail) }, '*');
  const fail = (step, detail) => report(step, false, detail);

  v.addEventListener('error', () => {
    const e = v.error;
    fail('video-error', 'code=' + (e && e.code) + ' msg=' + (e && e.message));
  });

  const once = (ev, timeoutMs) => new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(ev + ' timeout')), timeoutMs);
    v.addEventListener(ev, () => { clearTimeout(t); resolve(); }, { once: true });
  });

  (async () => {
    try {
      await once('loadedmetadata', 10000);
      report('loadedmetadata', true, 'duration=' + v.duration.toFixed(2)
        + ' seekable=' + (v.seekable.length ? v.seekable.end(0).toFixed(2) : 'none'));

      // Cold seek forward past what preload could plausibly have buffered.
      v.currentTime = 7.5;
      await once('seeked', 10000);
      report('seek-forward', Math.abs(v.currentTime - 7.5) < 0.5,
        'currentTime=' + v.currentTime.toFixed(2) + ' readyState=' + v.readyState);

      // Play for ~1.2s and confirm time advances.
      const before = v.currentTime;
      await v.play();
      await new Promise(r => setTimeout(r, 1200));
      report('playback', v.currentTime > before + 0.4,
        'advanced ' + (v.currentTime - before).toFixed(2) + 's');

      // Seek backward while playing.
      v.currentTime = 2.0;
      await once('seeked', 10000);
      report('seek-backward', Math.abs(v.currentTime - 2.0) < 0.6,
        'currentTime=' + v.currentTime.toFixed(2));

      report('done', true, 'all steps completed');
    } catch (err) {
      fail('exception', err && err.message);
    }
  })();
})();
</script>
</body></html>
"""

let shellHTML = """
<!doctype html>
<html><head><meta charset="utf-8"></head><body>
<iframe sandbox="allow-scripts" src="artifact://localhost/doc" width="800" height="500"></iframe>
<script>
window.addEventListener('message', (e) => {
  window.webkit.messageHandlers.test.postMessage(e.data);
});
</script>
</body></html>
"""

final class ArtifactSchemeHandler: NSObject, WKURLSchemeHandler {
  func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
    let data = artifactHTML.data(using: .utf8)!
    let resp = HTTPURLResponse(
      url: task.request.url!, statusCode: 200, httpVersion: "HTTP/1.1",
      headerFields: [
        "Content-Type": "text/html; charset=utf-8",
        "Content-Length": String(data.count),
        "Content-Security-Policy": csp,
      ])!
    task.didReceive(resp)
    task.didReceive(data)
    task.didFinish()
  }
  func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {}
}

/// Range-capable handler mirroring tauri's asset protocol semantics.
final class AssetSchemeHandler: NSObject, WKURLSchemeHandler {
  let fileData: Data
  // WKURLSchemeTask throws NSException if touched after stop; track liveness.
  var stopped = Set<ObjectIdentifier>()
  let maxChunk = 1024 * 1024 // tauri asset.rs MAX_LEN: 1000*1024; close enough

  init(path: String) {
    fileData = FileManager.default.contents(atPath: path)!
  }

  func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
    let id = ObjectIdentifier(task)
    let len = fileData.count
    let rangeHeader = task.request.value(forHTTPHeaderField: "Range")
    print("ASSET: request Range=\(rangeHeader ?? "<none>")")

    var headers: [String: String] = [
      "Content-Type": "video/mp4",
      "Accept-Ranges": "bytes",
    ]
    var status = 200
    var body = fileData

    if let r = rangeHeader, r.hasPrefix("bytes=") {
      let spec = r.dropFirst(6).split(separator: ",")[0]
      let parts = spec.split(separator: "-", omittingEmptySubsequences: false)
      var start = Int(parts[0]) ?? 0
      var end = parts.count > 1 && !parts[1].isEmpty ? Int(parts[1])! : len - 1
      if parts[0].isEmpty { // suffix form bytes=-N
        start = max(0, len - (Int(parts[1]) ?? 0)); end = len - 1
      }
      if start >= len || end < start {
        let resp = HTTPURLResponse(
          url: task.request.url!, statusCode: 416, httpVersion: "HTTP/1.1",
          headerFields: ["Content-Range": "bytes */\(len)"])!
        if !stopped.contains(id) { task.didReceive(resp); task.didFinish() }
        return
      }
      end = min(end, len - 1)
      // Mirror tauri's chunk cap: serve at most maxChunk bytes per response.
      end = min(end, start + maxChunk - 1)
      status = 206
      headers["Content-Range"] = "bytes \(start)-\(end)/\(len)"
      body = fileData.subdata(in: start..<(end + 1))
      print("ASSET: respond 206 Content-Range=\(headers["Content-Range"]!) bytes=\(body.count)")
    } else {
      print("ASSET: respond 200 full bytes=\(len)")
    }
    headers["Content-Length"] = String(body.count)

    let resp = HTTPURLResponse(
      url: task.request.url!, statusCode: status, httpVersion: "HTTP/1.1",
      headerFields: headers)!
    guard !stopped.contains(id) else { return }
    task.didReceive(resp)
    guard !stopped.contains(id) else { return }
    task.didReceive(body)
    guard !stopped.contains(id) else { return }
    task.didFinish()
  }

  func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {
    stopped.insert(ObjectIdentifier(task))
    print("ASSET: task stopped by WebKit (normal for media loads)")
  }
}

final class TestSink: NSObject, WKScriptMessageHandler {
  var results: [String: Bool] = [:]
  func userContentController(
    _ ucc: WKUserContentController, didReceive message: WKScriptMessage
  ) {
    guard let dict = message.body as? [String: Any],
          let step = dict["step"] as? String,
          let ok = dict["ok"] as? Bool else { return }
    let detail = dict["detail"] as? String ?? ""
    print("TEST: \(ok ? "PASS" : "FAIL") \(step) — \(detail)")
    results[step] = ok
    if step == "done" || !ok && (step == "video-error" || step == "exception") {
      let required = ["loadedmetadata", "seek-forward", "playback", "seek-backward", "done"]
      let allPass = required.allSatisfy { results[$0] == true }
      print(allPass ? "RESULT: SUCCESS" : "RESULT: FAILURE")
      exit(allPass ? 0 : 1)
    }
  }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)

let config = WKWebViewConfiguration()
config.setURLSchemeHandler(ArtifactSchemeHandler(), forURLScheme: "artifact")
config.setURLSchemeHandler(AssetSchemeHandler(path: videoPath), forURLScheme: "cfy-asset")
// Mirror wry's default autoplay=true.
config.mediaTypesRequiringUserActionForPlayback = []
let sink = TestSink()
config.userContentController.add(sink, name: "test")

let window = NSWindow(
  contentRect: NSRect(x: 0, y: 0, width: 850, height: 550),
  styleMask: [.titled], backing: .buffered, defer: false)
let webView = WKWebView(frame: window.contentView!.bounds, configuration: config)
window.contentView!.addSubview(webView)
window.orderFrontRegardless()

webView.loadHTMLString(shellHTML, baseURL: nil)

// Global timeout.
DispatchQueue.main.asyncAfter(deadline: .now() + 45) {
  print("RESULT: TIMEOUT (steps seen: \(sink.results))")
  exit(1)
}

app.run()
