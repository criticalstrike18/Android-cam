# Android Webcam Project — Current Status & Issues

Last updated: 2026-10-02 (Phase 1+2 transport racing, UI polish part 2, release re-verified)
Test device: Galaxy M51 (SM-M515F, Snapdragon 730G), LineageOS 23.0 Unofficial (2025-12-25 build).
Host connection: ADB over USB (forwards `tcp:8080` HTTP control, `tcp:8554` RTSP, USB-pinned via
`adb -s`) + Wi-Fi ADB (`192.168.29.140:5555`, phone DHCP — may change) + root via `su -c` (Magisk).
Both transports stay attached simultaneously. Phone currently foreground at 1080p30 h264.

## 0. Product direction (agreed 2026-10-01)

- **Supported codec: H.264 only.** HEVC is **experimental** and explicitly not a priority until the
  H.264 path is productionized (reliable 30 fps output, call-grade stability, battery validated).
  The H.265 depacketizer, error surfacing, and rebuild logic stay in the tree as infrastructure,
  but HEVC issues (§3.1) are backlog, not blockers.
- **Transport bar: USB primary, Wi-Fi must carry 1440p30 minimum.** 4K-over-Wi-Fi is nice-to-have;
  1440p30 over Wi-Fi is the requirement.
- Target: phone as facecam (720p/1080p/1440p/4K @ 30 fps final output) for OBS/Meet/Zoom on
  Windows, minimal heat and battery, lighter than a Windows webcam pipeline.

> **Read this first.** The 2026-09-30 status report prioritised Media Foundation hardware
> decode as P0 and RTP padding as P2. That ordering was inverted. RTP padding turned out to be
> a real corruption bug and the most likely cause of the HEVC failure, while MF hardware decode
> is a Windows-only optimisation behind an env-var flag that no user touches. Both were revisited;
> see §3 for what changed and why. All numbers in §5 predate the depacketizer fix and **must be
> re-measured** — see §6.

## 1. Current status: WORKING (with caveats)

- **Stock camera fixed.** All camera apps showed black preview with CamX fence/buffer errors.
  Root cause was a wedged `cameraserver`/HAL after 62 days uptime — **fixed by a phone reboot**.
  This is a device-state fix, not a code fix; a wedged `cameraserver` remains a recurring failure
  mode for users and the only shipped mitigation is the in-app watchdog (see §4 caveat).
- **AWA (Android app) streams live.** RTSP H.264 verified end-to-end (DESCRIBE returns valid SDP
  with SPS/PPS; server passes the 5s SPS wait in ~60ms on a healthy HAL). HTTP/WS control plane
  (`/settings`, `/features`, `/control`, `/ws`) verified.
- **Desktop Rust client (`CLIENT/awc-gui`) works on software decode.** `test_live_4k` passes.
  MF video-processor rescale proven.
- **Headless desktop mode works.** `awc-gui.exe --headless [--phone-ip ...] [--mode usb|wifi]
  [--duration-secs N]` streams straight to the virtual camera with 5 s console status, no
  window. Verified live over Wi-Fi: 727 vcam frames @ 30 fps in 25 s. Release binary at
  `CLIENT/awc-gui/target/release/awc-gui.exe` (18.9 MB).
- **ADB forwards are USB-pinned.** `run_adb_forward()` uses `adb -s <usb-serial>` so USB + Wi-Fi
  transports can stay attached at once (bare `adb forward` fails then with "more than one
  device" and the client sat offline while the phone streamed). Verified: forwards created as
  `RZ8N90DNVXW tcp:8080/8554`, 567 vcam frames @ 30 fps over USB.
- APK currently installed on the phone = **current debug build** (StreamService + back-cam
  1440p force-enable included, installed over Wi-Fi ADB).
- **Transport-switch racing works (Phase 1+2, desktop, uncommitted at write time).**
  Make-before-break: background racer dials the new endpoint to its keyframe while the old
  session publishes, then atomic cutover — no teardown gap, no vcam rebuild. New
  `tests/test_transport_switch.rs` gate (USB→WiFi→USB, 24 s live): **gap 0 ms both cutovers**,
  100/220/207 frames per phase, keyframe locks 2.0 s / 1.0 s. Bounded 1.5 s RTSP dial,
  failover counters reset on explicit switches (counting logic untouched), real connection
  stages in header (`connecting…` / `waiting for keyframe…` / `reconnecting…`).
- **UI polish part 2 (desktop, in f3a9fd2).** Sliders were re-seeded from shared state every
  frame while the phone echoed async (thumbs snapped back mid-drag); exposure also guessed
  −12…+12 against the M51's real −20…+20. Now: `/features` fetched on connect, bounds from
  phone truth, drag-local thumbs committed throttled (150 ms) + flushed on release. Resolution
  ComboBox (scrolled with 5 items) → inline list. Mode highlight + IP field follow shared
  state after failover. ADB forward off UI thread. Preview flows even when vcam is held by
  another app (fps counter still counts only real publishes).

## 2. Fixes landed 2026-10-02 (transport racing + UI polish 2 — uncommitted at write time)

- `RtspSession::connect` uses `connect_timeout` 1.5 s (was unbounded OS timeout).
- `ControlStage`/`StreamStage` split ownership (sync/stream workers never fight one field);
  `switch_generation` tags explicit + failover flips, stale racers dropped latest-wins.
- `last_cutover_gap_ms` metric in state + log line per handoff.
- Proven as a side effect: the phone serves two parallel RTSP clients cleanly.
- UI part 2 as listed in §1. Files: `src/stream/{mod,rtsp_client}.rs`, `src/network/sync.rs`,
  `src/core/state.rs`, `src/ui/{app,controls,header}.rs`, `tests/test_transport_switch.rs` (new).

## 2b. Fixes landed 2026-10-01 (evening session)

### Background / screen-off design (Android) — the significant one

Attempted true screen-off streaming (hold the session under a foreground service) and proved
it impossible with the display-bound GL pipeline, then shipped the safe design:

- `StreamService` (camera-type FGS, partial wake lock, return/stop notification) keeps the
  process alive; the activity **always stops the session cleanly first** on background/screen-off
  and restarts fresh on resume. Verified: sleep → wake → 111–112 frames, no wedge, ~12 s.
- Evidence that holding is impossible: `OpenGlView.setForceRender(true, fps)` does not save the
  session (surface is destroyed, not just un-vsynced) — 0 frames dark *and* after wake; the
  half-dead session wedged CamX past in-app recovery (forced restart still 0 frames; only a
  `cameraserver` kill cleared it). Clean-stop never wedges.
- `screen_off_streaming` setting (default on, in `/settings` + `/control`): with it, backgrounding
  parks under the service for instant resume; without it, bare release as before.
- Stop-from-notification works via `StreamServiceStop` flag consumed on resume.
- Side validation: user swiping the app away mid-session produced exactly the designed trace
  (`pauseCamera` → `RtspServer: Server finished` → FGS up → HTTP alive → resume restarts).

Files: `StreamService.kt` (new), `CameraView.kt` (pause/resume), `MainActivity.kt`
(POST_NOTIFICATIONS), `AndroidManifest.xml` (service + permissions), `VideoStreamServer.kt`
(setting plumbing).

### Back-cam 1440p force-enable (Android)

`updateSupportedResolutions` filters the `StreamResolution` enum against sensor JPEG sizes, which
omit 2560x1440, so 2K was never offered. The enum already had `P1440`; it is now force-included
for the back camera (same treatment 4K had). Encode size verified from SPS bits in the RTSP
handshake (2560 wide; Samsung writes a loose `level_idc: 10` tag, decoders don't care). Verified
over Wi-Fi: 70 + 67 frames stable. New `tests/test_live_wifi.rs` oracle (phone_ip =
`192.168.29.140`) kept for all future Wi-Fi tests.

### USB-pinned ADB forwards + headless CLI (desktop)

See §1. Files: `src/platform/adb.rs` (`usb_device_serial`, `run_single_forward`),
`src/main.rs` (`--headless`, `--phone-ip`, `--wifi-ip`, `--mode`, `--duration-secs`,
console attach for release `windows_subsystem` builds, stdin-terminal guard so piped stdin
can't EOF-kill a run), `Cargo.toml` (`Win32_System_Console`), `tests/test_live_wifi.rs` (new).

## 2c. Fixes landed 2026-10-01 (earlier)

### Depacketizer correctness (desktop) — the significant one

The depacketizer previously assumed every RTP header was exactly 12 bytes and ignored the padding
bit entirely. Both are real corruption sources:

- **Padding was never stripped.** The padding count lives in the *last* byte of the packet, so
  leaving it in place appended pad bytes to the tail of every single-NAL packet and injected them
  into reassembled fragments. The old note calling this "harmless (decoders skip)" was wrong.
- **Fixed 12-byte header assumption** mis-read the NAL header whenever a sender used CSRCs or an
  RTP header extension.
- **No sequence tracking.** A fragment lost mid-FU produced a corrupt NALU fed downstream as if
  complete. Now a sequence gap invalidates the partial NALU. This is the most likely cause of
  `rusty_h265`'s `slice without a first slice segment` — a continuation slice arriving with no
  picture in flight.
- **Unbounded buffers.** `fu_buffer` and the aggregation overflow queue are now capped; an
  over-cap fragment invalidates rather than exhausting memory.
- **RTP v2 validated** (the old parser accepted any version byte).

Files: `src/stream/rtsp_client.rs`. Covered by **28 new unit tests** in
`tests/test_depacketizer.rs` — including regressions for each bug above, the STAP-A/AP fan-out
that previously shipped with zero coverage, and sequence wraparound.

### HEVC diagnostics (desktop)

`rusty_h265` errors and caught panics both collapsed to `Ok(None)`, indistinguishable from "needs
more data" — which is why HEVC looked mysteriously broken. H.264 surfaced errors; H.265 did not.

- `next_frame()` errors now propagate as `Err(String)`; panics are counted and reported.
- Zero-dimension frames are rejected rather than producing a bogus buffer size.
- After 32 consecutive failures the decoder is rebuilt and re-primed from the retained parameter
  sets, so a wedged decoder recovers instead of streaming black forever.
- New `RtpStats` / `DecoderHealth` counters, logged by the worker whenever they change and at
  least every 10s.

**These counters are the first thing to look at when diagnosing HEVC.**

### Android

- **`rotation=auto` never reached the encoder.** The camera is built with the `OpenGlView`
  constructor, so `glInterface` is that view — which implements `GlInterface`, *not*
  `GlStreamInterface`. Every `as? GlStreamInterface` in the rotation path was therefore
  permanently `null`. Since `auto` is the client default, out-of-the-box video was landscape-locked
  while `/settings` reported the device's real orientation. Verified against the library bytecode
  (`javap`): `OpenGlView implements GlInterface, OnFrameAvailableListener, SurfaceHolder.Callback`.
  Rotation now goes through `OpenGlView.setStreamRotation()`, and the orientation listener drives
  it live while in `auto` mode. Dead `attachPreview`/`deAttachPreview` code and the unused
  `GlStreamInterface` import were removed.
- **`POST /settings` acknowledged stale state.** The update handler launched a coroutine on the
  main thread and returned immediately, so the 200 response *and* the WebSocket broadcast both
  carried the pre-change state, and `GET /control` re-read pre-change values. The handler's error
  branches were unreachable. It is now a `suspend` callback that completes only after the change is
  applied, so responses, broadcasts, and `/control` reads all reflect reality.
- **`camera` + `switch_camera` cancelled out** — `?camera=front&switch_camera=true` applied both
  and toggled back. The toggle is now ignored when an explicit camera is supplied.
- **Unauthenticated remote crash vector.** `setExposure` forwarded a raw, unclamped index to
  Camera2 on the main thread with no try/catch, reachable from any LAN client via
  `POST /settings {"exposure_index":9999}` (CORS is `anyHost()`). Values are now clamped to the
  reported `CONTROL_AE_COMPENSATION_RANGE` before use, and both it and `setZoom` handle driver
  rejection instead of crashing.
- **Dependency surface trimmed.** Four CameraX artifacts were pulled in for two int constants
  (`CameraSelector.LENS_FACING_*`); only `camera-core` is kept. The unused `kotlin-parcelize`
  plugin is gone. Hardcoded dependency versions that silently contradicted `libs.versions.toml`
  now use the catalog. **Debug APK: 25.19 MB → 23.40 MB (-1.79 MB).**
- Stale comments corrected: `CameraView.kt` claimed a "same-instance warm restart" that the code
  never did (STATUS.md §2 of the 2026-09-30 report had this right; the code comment did not).
- `HelpActivity.kt` and `static/help.html` no longer document a `/video` MJPEG endpoint that was
  deleted, or claim zoom is unsupported.

### Repo hygiene

- Deleted 2.7 MB of extracted dependency bytecode dumped in `AWA/` module root
  (`classes.jar`, `com/`, `META-INF/`); `.gitignore` now covers them.
- All phone-dependent integration tests are `#[ignore]`d, so `cargo test` is meaningful without
  hardware. Run them with `cargo test -- --ignored`.

## 3. Open issues (re-prioritised)

13. **Resolution lost across background cycle (P1, new).** After a background → resume cycle the
   phone restarts at 1280x720 even when 1440p was set (`resumeCamera` logs `streamRequested=false`).
   Root cause not yet isolated (settings persistence vs restart path using defaults). Repro: set
   1440p via `/control`, swipe app away, foreground, read `src=` — 720p.
14. **Front camera not offered 1440p (P1, new).** Same filter as §2 back-cam fix, but the
   force-include is back-only. Hardware encodes it (STATUS §5 measured front-1440p 118 frames;
   `/control?resolution_str=2560x1440` already works on any camera via `fromString` — only the
   advertised list + dropdowns hide it). Fix = extend force-include to front + SPS-verify.
   Deliberately not done in this session; one-line change, needs a front-cam test run.
15. **Slow connection establishment on mode switch — IMPLEMENTED 2026-10-02 (this batch).**
    Racing + split stages + 1.5 s dial + counter resets as designed (see §1/§2): 0 ms gaps
    measured. Auto-failover counting untouched per deferral. Remaining: click-spam soak
    untested, IDR-assist (Phase 3) pending encoder-API check.
16. **Minor UI issues — sliders/dropdown FIXED 2026-10-02 (this batch), rest open.** Fixed:
    drag-local thumbs, real `/features` bounds, inline resolution list, mode/IP desync, ADB off
    UI thread, preview-without-vcam. Still open candidates: app permission-denied text blames
    camera when only notifications refused; app IP label is `remember`-once (stale after Wi-Fi
    change); `screen_off_streaming` has no app Settings toggle (only via `/control`).
1. **HEVC verification (P0) — needs hardware.** The RTP padding and fragment-loss fixes above are
   the leading candidate explanation for the HEVC failure, but this is **unverified**: it was
   reasoned from the spec and proven by unit tests, not observed on the wire. Run
   `cargo test --test test_fps_probe -- --ignored --nocapture --test-threads=1` against the phone
   and read the new `health:` line. If HEVC still yields 0 frames, the counters will now say
   whether the problem is RTP (`gaps` / `dropped_partial` / `malformed`) or the library
   (`hevc errors` / `hevc panics`). Note `rusty_h265` is known to have deblocking panics; the
   decoder now rebuilds rather than dying silently.
2. **Media Foundation hardware decode — deprioritised, not fixed.** `test_mf_roundtrip` fails on
   a **network-free** harness: encoder produced 43 packets, decoder emitted 0 frames. This
   contradicts the earlier report's claim that MF "decodes chunked-file bytes of the same stream
   fine", and suggests the decoder MFT is silent generally rather than only on the live NALU feed
   — which would explain why ~25 feed-format experiments all failed. Proven pre-existing 2026-10-02
   via `git stash`: identical failure (43 packets, 0 frames) with all local changes removed, while
   live H264 decode pulls hundreds of frames minutes apart — synthetic path or driver state, not the
   stream pipeline. `src/stream/mf.rs` (~1.9k
   lines, 16 hot-path env-var reads) is dead weight in the release binary and should move behind a
   cargo feature before any further work. MF's *video processor* (2.27 ms 1080p→720p rescale) is
   proven and separately useful.
3. **Re-measure everything (§5).** All decode timings predate the depacketizer fix, and the
   black-preview incident was resolved by a reboot rather than a code change.
4. **Phone-side stream instability (P1).** Occasional >800 ms gaps and encoder SPS changes between
   sessions; timing-sensitive tests flake because of it (`test_codec_switch_lag` decoded 10/20).
   Note the client's own >800 ms stall watchdog now fires on these and forces a reconnect — worth
   confirming that is the intended behaviour and not a reconnect storm.
5. **Mystery Wi-Fi peer (P2).** Something on `192.168.29.x` sent `resolution 1920x1080` twice and
   fights test settings. **Identify and kill it before trusting any further automated run** — it
   may already have invalidated some of §5.
6. **Command coalescing (P2).** `pending_command` is a single slot: dragging the zoom or exposure
   slider emits ~10 writes and drops all but the last, and those two are also written to local
   state optimistically so the UI can show a value the phone never applied.
7. **Per-frame allocations (P2).** `rgba_buf.clone()` (~1.4 MB/frame) to hand a preview frame to
   the channel, four LUT `Vec`s rebuilt per frame, and a full `SharedAppState` clone every UI
   frame (~20+ allocations at 33 Hz just to render a snapshot).
8. **`pending_command` send/ack (P2).** Client→phone commands are fire-and-forget over a
   hand-rolled WebSocket client that never verifies `Sec-WebSocket-Accept`, uses a fixed mask key,
   does not validate `payload_len`, and silently discards fragmented messages — and the settings
   JSON exceeds the 125-byte single-frame limit, so this path is likely dropping data.
9. **Blocking calls on hot paths (P2).** `run_adb_forward()` runs on the UI thread (visible
   window freeze); `VideoStreamServer.stop()` blocks the main thread up to 2 s in `onCleared()`;
   `state.lock().unwrap()` on the UI thread means one poisoned mutex panics all three threads.
10. **Unauthenticated control plane (P2).** Every endpoint is open with CORS `anyHost()`; no
    `setAuth` on the RTSP server. Fine for a trusted LAN, not for anything shared.
11. **Release signing (P2).** Release builds are signed with the **debug** key
    (`app/build.gradle.kts`). Commented in place; must be replaced before publishing.
12. **Numbers are debug-build (P2).** Unoptimised + debuginfo, preview conversion included.

## 4. Battery notes

- Biggest drains in order: display → camera sensor/ISP + HW encoder (esp. 4K30) → Wi-Fi radio at
  high bitrate.
- Shipped mitigations: idle screen-dim (display is #1), 15 fps mode, 720p default.
- Guidance: 720p15 = lowest-power usable; 1080p30 = sweet spot; 4K for short sessions (thermal
  throttle risk). ADB-forwarded USB traffic already avoids Wi-Fi for the desktop link.
- Screen-off handling is now a foreground service with clean-stop + instant resume (see §2);
  true screen-off *streaming* was proven impossible with display-bound GL (surface death + CamX
  wedge past in-app recovery) and is not pursued. Display still dims to 0.02 after 15 s idle.
- True drain measurement still not run (USB trickle charge masks it; kernel `current_now` reads
  nonsense scale, `charge_counter` frozen at 7000000, capacity 100→99 over ~25 min of 1440p).
  Re-run the drain window starting below ~80% if numbers are wanted.

**Caveat worth deciding before release:** the black-preview incident was cured by rebooting the
phone. A wedged `cameraserver` after long uptime is a real recurring condition, and the shipped
`AvailabilityCallback` watchdog only covers the case where the framework closes *our* camera. A
wedged HAL serving *other* apps is out of scope for the app. Users hitting this will look at it as
an app bug.

## 5. Measured numbers (live over ADB, measured 2026-09-30 — **pre-depacketizer fix**)

Software decode (openh264, debug build: decode + downscale to 1280x720 NV12 + preview RGBA per
frame, 10 s runs):

| feed | requested | actual source | frames | fps | avg ms | warm ms | p95 ms |
|---|---|---|---|---|---|---|---|
| rear-1080p | 1920x1080 | 1920x1080 | 128 | 12.8 | 64.6 | 65.5 | 111 |
| rear-1440p | 2560x1440 | 2560x1440 | 138 | 13.8 | 71.0 | 71.7 | 101 |
| rear-4k | 3840x2160 | 3840x2160 | 95 | 9.4 | 103.2 | 105.3 | 154 |
| front-1080p | 1920x1080 | 1920x1080 | 130 | 12.9 | 72.0 | 72.7 | 95 |
| front-1440p | 2560x1440 | 2560x1440 | 118 | 11.7 | 84.0 | 85.2 | 118 |
| front-4k | 3840x2160 | 3840x2160 | 90 | 8.9 | 110.7 | 112.5 | 157 |

Notes: 1440p/4K work on both cameras (no fallback). Cost scales sublinearly (4K ≈ 1.6× 1080p) since
output is fixed 720p. `fps` here is decode throughput under debug, not stream rate.

Wire frame rate (zero-decode probe, no backpressure), rear camera: **30 fps sustained at 720p
(28.2 avg incl. startup), 1080p (29.7), and 4K (29.6)**, IDR every ~2 s. The 30 fps pipeline works;
end-to-end rate today is decode-bound on desktop-debug.

MF video processor: **1920x1080 NV12 → 1280x720 NV12 at 2.27 ms/frame (1.98 GB/s)** — HW rescale
offload path is proven and available.

## 5b. Measured numbers (2026-10-01/02, post-fix, release + debug)

- Release headless, USB 1080p30: **410 vcam frames @ 30 fps in 15 s**, clean exit.
- Release headless, USB 720p30 (warm): 112 frames / 4 s (~28 fps). 4K: 51 / 4 s (~13 fps,
  desktop-decode ceiling — wire carries 30, see §5 wire probe note).
- Transport-switch gate `test_transport_switch` (debug, USB→WiFi→USB): phases 100/220/207
  preview frames, cutover gaps **0 ms / 0 ms**, keyframe locks 2.0 s / 1.0 s, vcam unrebuilt.
- Phone serves two parallel RTSP clients cleanly (racer + serving session overlap).

## 6. Useful commands

```bash
adb forward tcp:8080 tcp:8080 && adb forward tcp:8554 tcp:8554   # re-add after reboot
adb shell su -c 'kill $(pidof cameraserver)'                    # no-reboot camera reset
curl "http://127.0.0.1:8080/control?camera=back&resolution_str=1280x720&video_codec=h264&fps=15"
# Wi-Fi equivalents use http://192.168.29.140:8080 (phone DHCP — re-check with `adb shell ip route`)

cargo test                       # unit tests only, no phone required
cargo test --test test_mf_roundtrip                                # known-failing, see §3.2
cargo test -- --ignored --nocapture                                # ALL live tests - needs phone

# Per-test, phone must stay foreground:
cargo test --test test_fps_probe -- --ignored --nocapture --test-threads=1  # wire fps + HEVC check
cargo test --test test_mf_matrix -- --ignored --nocapture                   # SW perf matrix (~3 min)
cargo test --test test_mf_pipe -- --nocapture                              # MF processor checks
cargo test --test test_live_wifi -- --ignored --nocapture                  # Wi-Fi oracle (phone on 192.168.29.140)
cargo test --test test_transport_switch -- --ignored --nocapture           # USB→WiFi→USB race gate (both transports live, phone foreground)
MF_ENABLE_HW=1 cargo test --test test_mf_debug -- --ignored --nocapture      # MF experiment (silent)

# Headless desktop client (no GUI window):
.\target\release\awc-gui.exe --headless --phone-ip 192.168.29.140   # Wi-Fi, unplugged
.\target\release\awc-gui.exe --headless                             # USB (127.0.0.1 via forwards)
```

The stream worker now logs a `health:` line whenever depacketizer or decoder damage changes. That
line is the fastest way to tell an RTP problem from a decoder problem.