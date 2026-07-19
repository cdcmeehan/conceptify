# Prototype: custom-scheme video playback + seeking in WKWebView

**Status: verification harness, not app code.** Built for decision bead
`conceptify-z9y.1` (video asset delivery architecture). Keep it — bead
`conceptify-z9y.6` (the app-side `cfy-asset://` protocol handler) should
re-run it after macOS/WebKit updates and mirror its Range semantics.

## What it proves

A standalone WKWebView harness replicating the production viewer topology
exactly:

```
shell page (≈ tauri://localhost)
  └─ <iframe sandbox="allow-scripts">            (opaque origin, as in prod)
       served via artifact:  custom scheme, with the production CSP
       + media-src carve-out
       └─ <video src="cfy-asset://localhost/test.mp4">
            served via a second custom scheme with HTTP Range semantics
```

Verified on macOS 26.4.1 (build 25E253), 2026-07-19, with a 12 s 720p
H.264/AAC mp4 (4.8 MB, faststart):

- `loadedmetadata` fires, full duration seekable.
- Cold forward seek (7.5 s), playback, and backward-seek-while-playing all
  succeed. **Range support is mandatory**: AVFoundation's first request is
  `Range: bytes=0-1`; without 206/`Content-Range` responses video does not
  play at all.
- AVFoundation issues **hundreds of small bounded GET requests** (713 for
  one playback with two seeks; many are 8-byte MP4 box-header reads, the
  largest ~30 KB). No open-ended `bytes=N-` requests, no HEAD requests.
  Implication for the real handler: open + seek + read only the requested
  range — never read the whole file per request.
- Short 206 responses are tolerated: capping every response at 4 KiB (and at
  tauri's asset-protocol-style 1 MiB) still plays and seeks — AVFoundation
  re-requests the remainder. A per-response chunk cap is therefore safe and
  keeps wry's fully-buffered response bodies bounded.
- **CSP is enforced on custom-scheme media inside the sandboxed iframe**:
  with the production CSP unchanged (`media-src` falling back to
  `default-src 'none'`) the video is blocked (MediaError code 4). Both
  `media-src cfy-asset:` and the tighter `media-src cfy-asset://localhost`
  unblock it (the host form is what the app adopts).
- WebKit calls `stop` on scheme tasks constantly during media loads
  (cancelled speculative requests). Touching a stopped task throws — the
  handler must track task liveness (wry already guards this with
  uuid-validity checks + exception catches).

The wry 0.55.1 / tauri 2.11.5 layer was verified **by source inspection**
(not end-to-end): wry forwards all request headers (incl. `Range`) into the
Rust `Request` and marshals response status + arbitrary headers
(`Content-Range`, `Accept-Ranges`) into `NSHTTPURLResponse`
(`wry/src/wkwebview/class/url_scheme_handler.rs`), and tauri core's own
`asset://` protocol (`tauri/src/protocol/asset.rs`) ships this exact Range
pattern as a supported media-streaming feature. Residual risk is low; z9y.6
should still smoke-test in the real app.

## Running it

```sh
# generate the test clip (any mp4 works; pass a path as argv[1])
ffmpeg -f lavfi -i "testsrc2=duration=12:size=1280x720:rate=30" \
       -f lavfi -i "sine=frequency=440:duration=12" \
       -c:v libx264 -profile:v high -pix_fmt yuv420p -c:a aac \
       -movflags +faststart test.mp4

swiftc -O main.swift -o videotest
./videotest ./test.mp4                          # expect RESULT: SUCCESS
MEDIA_SRC="'none'" ./videotest ./test.mp4       # CSP control: expect FAILURE
MEDIA_SRC="cfy-asset://localhost" ./videotest ./test.mp4  # host form: SUCCESS
```

Exit code 0 = all steps passed. `ASSET:` lines log every Range
request/response; `TEST:` lines report the in-page steps.
