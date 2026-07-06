# PaperCast — plan to completion (v2, 2026-07-05)

## Context

PaperCast mirrors a Linux desktop (COSMIC/Wayland) to an Android e-ink tablet over
USB (`adb reverse`), with an e-ink-tuned pipeline, runtime display modes, a VNC
path, and a custom pull-based transport + native Android receiver. **All software
buildable without hardware is done, reviewed, and emulator-verified.** This
document is the plan for everything that remains, through v1.0 and beyond: the
hardware test protocol (what to measure, how, and what each measurement decides),
the vendor refresh backend, color support for Kaleido-class panels, the reach
milestones, and the release.

Workflow unchanged: a cheaper implementing LLM codes milestone-by-milestone; a
stronger reviewer model verifies each batch against this file and records verdicts
here. The original reviewer (Fable) hands off after this revision — see "Review
loop & hand-off" at the end.

**Definition of done (v1.0):** PaperCast daily-drives the chosen tablet — modes
switch live, latency and ghosting are measured and tuned, the winning transport is
documented as the default path, a vendor refresh backend exists (or is documented
as unnecessary for the device), the color decision is made and implemented-or-
closed, the vendored VNC patches are submitted upstream, and a tagged GitHub
release ships source + a prebuilt APK with a README that matches reality.
M13–M15 are **not** required for 1.0; they are demand-driven post-1.0 work.

## Current state

- Repo: `github.com:chase-lambert/papercast` (MIT). HEAD `cbde7cd`; 5 commits
  ahead of origin at last check — push them.
- Workspace: `papercast-core` (pipeline), `papercast-capture` (test pattern +
  Wayland ext-image-copy-capture), `papercast-proto` (wire format v1, NDK-safe),
  `papercast-recv-core` (receiver core, `--features android` → JNI cdylib),
  `papercast` (binary: CLI, config, ctl socket, VNC + papercast transports),
  `vendor/rustvncserver` (3 local patches, see its VENDORED.md), `android/`
  (Kotlin shell: RecvCore/FrameRenderer/MainActivity/RefreshBackend).
- Milestone history lives in git (Phase 0 `621b262` … M11b.1 `78f5d78`, backlog
  fixes `4d82be6`/`0163ac2`, docs consolidation `5faa253`/`cbde7cd`). Review
  verdicts 1–21 are resolved; their full text is in this file's git history
  (`~/.claude/plans/`, and the pre-v2 version of this file). Only still-open
  findings are carried in the ledger below.
- Docs: `README.md` is the single public doc (keep its Roadmap current;
  STATUS.md/ROADMAP.md/android/README.md are deleted — don't recreate them).
  Working state + review record live here.
- Verdict 21 (2026-07-05, final Fable review): backlog fixes `4d82be6` (test-
  pattern clamp; regression sweep pins the geometry incl. the `max(8)` floor) and
  `0163ac2` (pipeline reprocesses cached raw frame on settings change; biased
  select is the right priority, `borrow_and_update` consumes the flag, both arms
  cancel-safe, fps-only no-op is test-pinned) — **both approved from full diffs**.
  Docs consolidation approved. Push everything.

## Ground rules for the implementing agent (unchanged, restated)

- Small commits, one milestone or sub-step each. Gate before every commit:
  `cargo test --workspace` green; `cargo clippy -p papercast-core -p
  papercast-capture -p papercast -p papercast-proto -p papercast-recv-core`
  clean (vendored warnings deferred to M16).
- **Never add Co-Authored-By or attribution trailers to commits.**
- Network listeners bind `127.0.0.1` by default, always. Device specifics stay in
  config files (TOMLs) and the Kotlin `RefreshBackend` layer — never in
  `papercast-proto` or `papercast-recv-core`.
- Invariants: Bayer dithering anchored to absolute output-framebuffer coordinates
  (never tile-local); Wayland proxies stay on their dedicated thread, no `await`
  there (sync `watch::borrow()` is fine); import
  `rustvncserver::server::ServerEvent`; vendored changes documented in VENDORED.md.
- Design forks and M14-class decisions: **stop and wait for review before
  implementing.** Record the question here.
- On-device results get recorded in the "M9 results" appendix of this file
  (raw data) and distilled into README + the device TOML (public truth).

## Open findings ledger (everything carried forward; cite the number when closing)

- **L1 (verdict 20 → M12):** `FrameRenderer`'s cache-redraw path replays
  `lastHint`. With a real waveform backend, a full-screen redraw with a stored
  Fast/A2 hint will ghost. When any vendor backend lands, the redraw path must
  force Quality.
- **L2 (verdict 19 → M9.6):** the Kotlin Gray8→ARGB loop is per-pixel (~7.7 M
  px/frame at 3200×2400). Measure on device; if convert+draw > ~10 ms/frame,
  do M11c (expand to ARGB in the Rust core; the direct-ByteBuffer design already
  anticipates this).
- **L3 (verdict 19, note only):** `stop()` holds the RecvCore monitor through the
  native join; a hanging connect could block the UI thread ~3 s. Impossible on
  loopback (refusals are instant). Revisit only if ANRs appear on device.
- **L4 (verdict 17, note only):** `on_disconnect` fires on clean stop as well as
  lost link. Document/split only if the callback ever drives visible UI.
- **L5 (Phase 1 backlog, still open):** live output resize (needs rustvncserver
  framebuffer-resize support — RESEARCH, likely never worth it in-tree);
  rotated/transformed outputs; damage-narrowed pipeline work (`Rect::padded`).
  None block 1.0.

---

## Phase H — Hardware validation (M9). Runs the week the tablet arrives.

The point of M9 is not "does it work" — it's to produce **numbers and verdicts
that drive five specific decisions**: daily transport (D1), mode/refresh tuning
(D2), reading-mode dither (D3), receiver performance work (D4), and the color
gate (D5). Every sub-step below names the decision its data feeds. Record all
raw results in the appendix at the bottom of this file.

### M9.0 — Device intake (30 minutes)

- Enable USB debugging; `adb devices` must show `device`.
- Record in the appendix: `adb shell getprop ro.product.manufacturer` and
  `ro.product.model` (drives `RefreshBackend.byManufacturer()`), panel size,
  native resolution (mono AND color effective resolution if Kaleido), Android
  version, panel refresh modes the vendor UI exposes.
- Create/adjust the device TOML from the *actual* panel: `target-size` = native
  mono resolution, `levels` = panel gray levels, starting `fps` = 15.
  (`boox-tab-x-c.toml` exists; a different device gets its own file — measured
  values, not spec-sheet guesses.)

### M9.1 — Smoke, VNC path (day 0)

`papercast run --config <device>.toml` + `adb reverse tcp:5900 tcp:5900` + AVNC →
`127.0.0.1:5900`. Pass: renders, tracks live changes, survives 10 minutes and a
cable replug (re-run `adb reverse`). This is the fallback that must always work;
if it doesn't, stop and fix before anything else.

### M9.2 — Smoke, native receiver (day 0)

`scripts/build-recv-core.sh` (arm64), `./gradlew assembleDebug`, install,
`adb reverse tcp:5920 tcp:5920`, host `--transport papercast`. Pass: renders;
survives host restart (auto-reconnect), app background/foreground (cached frame
redraws — M11b.1), and replug.

### M9.3 — Latency measurement (feeds D1)

Method: `papercast run --latency-test ...` stamps a ms counter on every frame.
Film the desktop monitor and the tablet **in the same phone slow-mo shot**
(240 fps ≈ 4 ms resolution). Freeze frames, read both counters, subtract; take
≥10 samples per cell, record median and worst.

Matrix (each cell = one short filmed session):

| Transport | Mode | Panel refresh setting |
|---|---|---|
| VNC+AVNC / papercast+native | writing / browsing / reading | fastest (A2/X-class) and quality (Regal/GC-class) |

Also do the subjective test that matters most: type continuously in a terminal
and an editor in writing mode and judge whether the lag is workable.

**D1 decision rule:** the native transport must beat VNC by ≥30 ms median in
writing mode *or* show visibly less ghosting/artifacting to become the documented
daily path. If it doesn't, **VNC+AVNC is the daily driver**, the native app is
kept as the vehicle for the refresh backend (M12) and re-judged after M12 — and
if it still doesn't win after M12, say so honestly in the README and deprioritize
the app. Known physics for sanity-checking the numbers: pipeline ~23 ms, USB
single-digit ms, panel ~100–150 ms (A2) to several hundred (GC16); the VNC path
carries an additional ~30 ms pacing ceiling (~31 fps).

### M9.4 — Ghosting & per-mode refresh tuning (feeds D2)

For each mode, scroll a text page repeatedly and count updates until ghosting is
objectionable → that count (with margin) becomes the mode's
`full-refresh-updates`; judge whether the periodic full-refresh flash is more or
less annoying than the ghosting it clears and tune `full-refresh-secs`
accordingly. On Boox: pair each PaperCast mode with the best per-app refresh
setting (writing↔A2/X, reading↔Regal/GC — verify, don't assume) and record the
pairing table in the README's Boox tips and the device TOML comments. On a
60 Hz-class panel (Daylight): most of this collapses — verify and record that
`full-refresh-*` can be near-off.

### M9.5 — Reading-mode dither A/B (feeds D3)

Uncomment the pre-staged `[modes.reading] dither = "atkinson"` block in the
device TOML. Same PDF page and terminal screenful under Bayer then Atkinson
(config hot-reloads — flip while watching). Judge on-panel: (a) text crispness
(macro photo of the same glyphs), (b) partial-update stability — wiggle the
cursor; Atkinson is frame-unstable, so small damage may ripple the dither pattern
into large visible churn (this is exactly what an LCD couldn't show; verdict 12).
**D3 rule:** Atkinson wins only if crisper *and* not visibly unstable on partial
updates. If it wins, flip the builtin `reading` overlay in `mode.rs` and document
the tile-diff interaction in README; otherwise delete the staged block and record
the verdict here.

### M9.6 — Receiver performance & soak (feeds D4)

- Add a *temporary* `System.nanoTime()` log around convert+draw in
  `FrameRenderer.onFrame`, watch `adb logcat`. **D4 rule (L2):** if convert+draw
  > ~10 ms/frame at native resolution, do **M11c**: move Gray8→ARGB expansion
  into `papercast-recv-core` (deliver ARGB in the ByteBuffer; Kotlin's loop
  becomes a straight `IntBuffer` copy). One commit, host tests for the expansion.
  Remove the timing log either way.
- Soak: 2 h mirroring in browsing mode. Watch `adb shell dumpsys meminfo
  com.papercast` at 0/1/2 h (flat = no leak; the JNI local-ref class of bug shows
  up here) and note battery drain %/h on both ends.
- Pipeline CPU at device resolution: the host logs avg ms/frame every 300 frames.
  At 3200×2400 expect ~2× the 23 ms/2560×1440 baseline. **If avg ms/frame > the
  writing-mode frame budget (33 ms), that is the entry criterion for M15 (wgpu)
  — record the number and move on; don't optimize speculatively.**

### M9.7 — Color evaluation (feeds D5; only if the panel is color)

Kaleido 3 ground truth: mono is native-resolution (~300 ppi on the Tab X C);
color is quarter-resolution (~150 ppi), heavily desaturated, 4096 colors. The
question is whether that color is worth the sharpness loss *per content type*.

Method — the color A/B exists today with zero new code: `papercast run --raw`
serves unprocessed **color** over VNC; a normal run serves the processed mono
mirror. View the same content through both on the actual panel:

1. syntax-highlighted code in your editor,
2. a photo-heavy web page,
3. a PDF with color figures/charts,
4. plain prose (control — mono should win outright).

Photograph each pair, and rate each content type: does color add real
comprehension/pleasantness value that outweighs the resolution loss?

**D5 rule:** color wins for ≥2 of the first three content types → **build M12.5**,
with `browsing` (and `video` if used) defaulting to color and `reading`/`writing`
staying mono. Color marginal everywhere → close M12.5 as "evaluated, mono wins;
raw VNC remains the color escape hatch" and record why. Mono-only panel
(Daylight) → close M12.5 as N/A for this device but keep the design (below) for
the repo's future.

### M9.8 — Record and act

Fill the appendix table; distill into README (a short "Measured on <device>"
subsection under the tablet section + updated Boox tips) and the device TOML.
Then execute the D1–D5 outcomes as individual milestones/commits. **Pause for
review after M9.8** — a fresh reviewer should sanity-check the decisions against
the data before M12/M12.5 code starts.

---

## M12 — Vendor refresh backend (after M9; the payoff milestone)

One more `RefreshBackend` implementation; core/proto untouched, `generic` remains
the fallback and the emulator build must keep working.

- **Onyx (Boox):** RESEARCH the current SDK artifact first (Onyx maven,
  `http://repo.boox.com/repository/maven-public/` — artifact names/versions move).
  Map hints via `EpdController`: Fast → A2/DU partial, Quality → full GC16,
  Auto → app default. Confine the dependency to the `onyx` backend (runtime
  reflection or a Gradle flavor) so `generic` builds never need the Boox repo.
  **Close L1 in the same commit:** cache-redraw (`setSurface` path) forces
  Quality instead of replaying `lastHint`.
- **Daylight (DC-1):** RESEARCH whether SolOS exposes any refresh/display API; if
  not, `generic` *is* the integration and M12 reduces to TOML tuning — record
  that outcome explicitly.
- **Any other device:** same pattern — RESEARCH the vendor SDK, implement behind
  the seam, wire `byManufacturer()`.
- Then re-run M9.3/M9.4 for the native transport and update the D1 verdict —
  waveform control is the native path's main advantage; this is where it either
  proves out or doesn't.

## M11c (conditional, from D4) — ARGB expansion in the receiver core

Only if M9.6 measured the Kotlin loop slow. `papercast-recv-core` converts
Gray8→ARGB8888 into a second reusable buffer; the JNI ByteBuffer carries
4 bytes/px; `FrameRenderer` copies via `asIntBuffer()`. Keep Gray8 delivery
behind the existing API for host tests. Note: if M12.5 happens too, do M11c
*first* or fold them — both touch the same ByteBuffer contract; don't churn it
twice.

## M12.5 — Color support (only if D5 said build it)

Everything today is grayscale end-to-end **by design**. This milestone adds color
as a first-class, per-mode option without breaking the mono path. Sub-steps, each
its own commit, host-testable before any device time:

1. **Protocol v2** (`papercast-proto`): bump `PROTO_VERSION` to 2; add a
   pixel-format byte to `ServerHello` (0 = Gray8, 1 = RGB888; per-connection,
   not per-rect). Start honoring the client's hello version: a v1 client gets
   Gray8 regardless of mode (the negotiation hook already exists — the sender
   currently ignores the client's version; v2 is where checking starts).
   Round-trip + mixed-version tests.
2. **Pipeline color path** (`papercast-core`): add `color: bool` and
   `saturation: f32` (default `1.0`) to the eink config. When color: keep RGB
   through tone/scale, sharpen on **luma only** (no channel fringing), apply the
   saturation multiplier (Kaleido desaturates hard — expect to ship ~1.3–1.6;
   tune on device), then quantize per channel at `levels = 16`/channel with the
   coordinate-anchored Bayer. **RESEARCH at implementation: decorrelate the
   channels** — using the identical threshold matrix per channel correlates the
   error into luminance blotches; a per-channel matrix offset/rotation is the
   standard fix. Same absolute-coordinate anchoring invariant, per channel,
   test-pinned like the mono version. Expect ~3× pipeline CPU; measure against
   the M9.6 number before optimizing.
3. **Mode integration** (`papercast`): color defaults per D5 (typically
   `browsing`/`video` color, `reading`/`writing` mono); mode switching flips
   pixel format mid-connection → the sender re-hellos or (simpler, decide at
   review) keeps format per-connection and forces a reconnect on a color↔mono
   mode switch. **This is a design fork — pause for review with a proposal.**
4. **Receiver** (`papercast-recv-core` + Kotlin): `Framebuffer` grows
   bytes-per-pixel; JNI ByteBuffer carries 3 (or 4, post-M11c) bytes/px; the
   bitmap is already ARGB_8888 so Kotlin changes are minimal.
5. **On-device color validation:** repeat the M9.7 content set through the real
   color pipeline (not `--raw`) — check per-channel dither stability on partial
   updates (the M9.5 wiggle test, now in color), tune `saturation`, confirm the
   CPU number, and re-judge the per-mode defaults. Record in the appendix.

The VNC path picks up color automatically wherever the pipeline emits RGB (it
already ships RGBA; today it's gray-expanded).

---

## Phase 3 — reach (post-1.0 unless demand appears earlier)

- **M13 — Portal/PipeWire capture** (GNOME/KDE/Fedora-proofing). Entry: a real
  user on a non-ext-image-copy-capture compositor. `ashpd` ScreenCast (persist
  the restore token) + `pipewire` crate behind a `portal` cargo feature; no
  damage info → existing TileDiff covers it; `papercast probe` reports and
  auto-selects ext-image-copy-capture → portal → error.
- **M14 — true extended display. ADR before ANY code (hard review gate).**
  Evaluate on then-current COSMIC: virtual/headless outputs (cosmic-randr,
  zcosmic protocols; `zcosmic_workspace_image_capture_source_manager_v1` was in
  probe output as a possible shortcut), nested headless compositor, wlroots
  approaches. Prototype only the ADR's recommendation.
- **M15 — wgpu pipeline.** Entry criterion: the M9.6/M12.5 CPU measurement
  exceeds the frame budget on real workloads. Port gray→LUT→unsharp→dither to
  compute passes; keep the CPU path for tests and as fallback.
- **M16 — upstream the vendored patches** (part of v1.0, see Phase R): PR all
  three rustvncserver patches (bind address; SetEncodings/ClientCutText parser
  fix; pacing constants — pitch the *event-driven send* as the real upstream fix
  and the constants as the minimal version). If merged and released, drop
  `vendor/` for the crates.io dep; if upstream is unresponsive after ~a month,
  document the vendoring as permanent in VENDORED.md and close.

## Phase R — Release v1.0

Cut only after M9 decisions are executed and M12 (and M12.5 if gated in) landed.

1. **Docs truth pass:** README claims match measurements (replace "untested
   pending hardware" phrasing with the measured table); device TOML(s) final.
2. **M16 submitted** (merged not required — submitted + VENDORED.md updated).
3. **Release hygiene:** `cargo test --workspace` + clippy gate green; crate
   versions to 1.0.0; a short CHANGELOG.md generated from the milestone history
   (this is the one new doc that's allowed).
4. **Tag + GitHub release:** `v1.0.0`; attach a release-built APK. RESEARCH at
   release time: a real signing keystore (even self-signed release keys beat
   debug keys for install-over-update); document the signing setup in the
   release notes, keystore never committed.
5. Post-1.0: issues drive M13/M14/M15; keep this file as the working plan.

---

## Review loop & hand-off

1. Implementing agent works milestone-by-milestone, small commits, updates the
   README Roadmap and this file's appendix as results land.
2. **Pause for review:** after M9.8 (decisions vs data), after M12, after each
   M12.5 sub-step 1–4 (step 3 is a design fork — review the proposal *before*
   code), before any M14 code (ADR), and before the v1.0 tag.
3. Any strong reviewer model can pick this up cold: this file + `git log -p`
   since the last verdict is the full state. Verify independently (run the test
   gate, read diffs — don't trust status reports; the record shows agent reports
   are occasionally off by a commit). Append verdicts here, numbered from 22.
4. Standing red lines for reviewers: dither coordinate-anchoring changes,
   any listener beyond loopback, protocol changes without a version bump, and
   attribution trailers in commits.

## Appendix — M9 results (fill in on hardware; template)

```
Device: __________  Manufacturer string: __________  Android: ____
Panel: ____ ppi mono / ____ ppi color / refresh modes: __________

Latency (median ms / worst ms, ≥10 samples):
  transport   mode      panel-setting   median   worst   notes
  vnc         writing   fast            ____     ____
  vnc         writing   quality         ____     ____
  papercast   writing   fast            ____     ____
  papercast   writing   quality         ____     ____
  (repeat for browsing, reading)

Ghosting: updates-until-objectionable per mode: reading __ browsing __ writing __
Chosen full-refresh-updates / -secs per mode: __________
Boox per-app refresh pairing: __________

Dither A/B (D3): crispness winner ____  stability winner ____  verdict ____
Receiver convert+draw ms/frame (D4): ____   soak meminfo 0/1/2h: ____/____/____
Pipeline avg ms/frame @ native res: ____   battery %/h: host ____ tablet ____
Color eval (D5): code ____ web ____ pdf ____ prose ____  → verdict ____

Decisions: D1 ____  D2 ____  D3 ____  D4 ____  D5 ____
```
