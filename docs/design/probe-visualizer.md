# Probe and visualizer: the plan to a full-featured split

Status: **plan**, written 2026-10-08 against `b7c4298` (v0.0.3). Builds on `distributed.md` (the
replication design, stage 1-2 built) and `ARCHITECTURE.md` (process model, UI boundary). Each
milestone below updates its own row when it lands.

## What "separation" means here

Two roles, one product:

- **Probe**: a machine whose screen is recorded. It records, seals settled history into segments on a
  schedule and drops them where a file mover picks them up. It needs no window and no one watching it.
- **Visualizer**: the machine you look from. It takes in segments from every probe automatically, keeps
  one replica store per probe, and shows all of them in the rewind window. It may also record itself.

The split is by **role and process**, not by binary. The hard rules keep one executable
(`CLAUDE.md`, "Single exe") and the UI is already a separate process whose dependency graph cannot reach
capture, storage, OCR or the daemon (`rsrewind-query/tests/ui_boundary.rs`). A probe is
`rsrewind daemon` + `rsrewind export --watch`; a visualizer is `rsrewind import --watch` + `rsrewind ui`.
Splitting into separate binaries or installers would need that rule amended; nothing below needs it,
and it stays reversible if packaging ever argues for it.

Where the visualizer runs matters most in practice: the maintainer's own desk is Linux. The UI crate
(Iced + wgpu) and everything it reads through are already portable; only the CLI refuses to start it
off Windows. So the first milestone is a Linux visualizer of Windows probes, which needs no Linux
recorder at all.

## Milestones

| # | Milestone | Done when | Size | State |
|---|---|---|---|---|
| **V1** | **Visualizer anywhere** | `rsrewind ui` runs on Linux; `search`/`recent` read every source through the `History` facade (fixes the "no database" known issue on a central-only folder); a real Windows probe's segments, imported on Linux, browse and search correctly | 1 session | planned |
| **V1.5** | **Gaps are first-class** | `History::gaps(range, filter)` explains every stretch with no observation from the markers the recorder already writes: recorder off (clean stop), recorder died (start with no stop), paused, idle/locked, privacy skip (needs `privacy_skip` events, Unit B), unknown. `recent` prints gap lines; the window shows gaps in the room and the strip with reason and duration, and stepping across one says what was skipped. A static screen (one long observation) is never a gap | 1-2 sessions | built 2026-10-08: `History::gaps` (heartbeat tells running from died), `rsrewind gaps`, gap lines in `recent`, strip bands and notes in the window. Open: privacy skips as events (to split "nothing stored"), gaps older than 30 days in the window |
| **V2** | **Hands-off pipeline** | `rsrewind export --watch` on a probe (interval, settle, quiet on no-op); `rsrewind import --watch <inbox>` on the visualizer (moves imported files to `done/`, refused ones to `rejected/` with a reason file, never deletes); per-source freshness (last segment, lag) in `sources`, `doctor` and the window; Windows scheduled task and systemd `--user` unit documented and tested | 1-2 sessions | planned |
| **V3** | **Trust what arrives** | Independent adversarial review of the import path and the facade (owed since stage 1); probe enrolment: per-probe secret, MAC over each manifest, visualizer allow-list, unsigned segments refused once a source is enrolled (motion `rsrewind-network-transport` option A; no network code) | 2 sessions | planned |
| **V4** | **Close the recorder privacy debt** | Unit B in `remediation-status.md`: F2 frame provenance, F3 pause acknowledgement, F4 on the Windows backends, F5 persist handling, F9, F21, plus real `privacy_skip` events. Live-verified on kubert, not just diff-read | 2-3 sessions (Opus tier) | planned |
| **V5** | **Linux probe** | First slice built 2026-10-08 (KWin ScreenShot2 + script window list, ext-idle-notify, lock = idle, no OCR yet: states stay `pending`). Then: Plasma Wayland recorder per `rsrewind-linux-wayland-port` motion (portal + PipeWire, KWin window list, idle-notify, OCR engine chosen by benchmark); a week of the maintainer's own desktop recorded | 3-5 sessions | planned |
| **T** | **Tray** | `rsrewind tray`: its own process with a notification-area icon on Windows, a StatusNotifierItem on Linux (Plasma, most others), a menu-bar item on macOS. Shows recording / paused / idle / not running / privacy-not-enforced; menu: pause (15 min, 1 h, until resumed), resume, forget the last 10 min / 1 h (confirmed), open the window, start/stop the recorder. It changes state only the way the CLI does (the SQLite control row, `rsrewind forget` as a child process), so a crashed tray cannot stop or corrupt recording; started at login next to the recorder | 1-2 sessions | Linux built 2026-10-08 (ksni StatusNotifierItem; verified live over dbusmenu: pause, resume, forget arms without deleting); Windows and macOS next |
| **V6** | **Visualizer, full featured** | Sources pane (label, hide, last seen, privacy-enforcement badge per source); per-source lane filter; day and session navigation (the brief's Archive mode); what was on every screen at a moment (multi-monitor); a scripted screenshot check of the window (STD-008 candidate for a wgpu surface) | 2-3 sessions | planned |

**Release gate.** V1-V4 plus the 8-hour acceptance run make 0.1.0. A probe on anyone else's machine
waits for V3 and V4: an unauthenticated import path and open recorder privacy findings are acceptable
on one person's own machines, not on someone else's.

## Order and why

1. **V1 first** because it is small, it is what the maintainer will look at every day, and it puts real
   cross-machine data in front of every later milestone.
2. **V2 before V3** because enrolment is easier to design once the inbox flow exists and its failure
   cases (rejected files, stale sources) have names.
3. **V3 and V4 before V5** so the Linux recorder inherits closed findings instead of copying open ones
   (the Linux port motion names this opportunity cost).
4. **V1.5 and T early**: gaps decide whether the history can be trusted at a glance, and the tray is
   the everyday surface (the operator asked for both, 2026-10-08). The operator's own machine records
   from 2026-10-08 on the V5 slice, which moves its first slice ahead of V3/V4 for that one machine only.
5. **V6 last**, built on data from real probes rather than synthetic history.

This follows the provisional rule recorded in the Linux port motion: build the smallest working
vertical slice, show it, keep each decision reversible.

## Decisions taken provisionally (reversible)

- **Roles, not binaries** (above).
- **The visualizer imports by watching a folder, not by the UI.** The UI may not depend on storage, so
  it never imports itself. A future "import now" button may start `rsrewind import` as a child process
  and read the result; it never links the writer.
- **Watch loops poll; no file-system notification dependency.** A segment arriving a minute late is
  fine, and polling behaves the same on SMB shares, sync folders and every OS.
- **Inbox files are moved, never deleted, by import.** Disposal is the operator's decision, like every
  other deletion in this product.

## Open questions for the maintainer

1. Probes on machines other than your own: when? (Sets whether V3/V4 block V5 or only a public claim.)
2. Should a visualizer that also records show its own machine as one lane among the others (today) or
   apart from them?
