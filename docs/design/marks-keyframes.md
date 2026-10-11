# Marks, keyframes and navigation in the rewind window

Status: **built 2026-10-11** (v0.1.0 push), on branch `claude/nifty-einstein-dd0bhh`. Not yet reviewed
(STD-001 adversarial review pending) and not yet run visually on real recorded history: see the dated
notes in `CLAUDE.md`.

## What the user gets

The top-right pill of the window (it used to be a dead label, "N sources · M moments") is the
**navigator**: a mark count that opens the marks panel, previous/next mark, previous/next keyframe, and
"+ Mark". The panel lists every mark (click to jump, rename, delete with a confirmation), takes a label
for a new mark, and has a "Go to" field that understands `14:30`, `yesterday 09:00`, `2026-10-08 14:00`,
`15m` (ago), `now`.

| Key | Does |
|---|---|
| `←` `→` (`↓` `↑`) | previous / next moment (existing) |
| `,` `.` or `PageUp` `PageDown` | previous / next keyframe |
| `[` `]` | previous / next mark |
| `Shift` + any of the above (`<` `>` `{` `}`) | go there **and extend the selection** to it |
| click / `Shift`-click / `Ctrl`(`Cmd`)-click a card | select one / the range from the anchor / toggle one |
| `Ctrl+A` | select every moment loaded around the cursor |
| `Ctrl+C` | copy the picked picture (or the one on stage) |
| `m` | mark the moment on stage (no label; rename later) |
| `Shift+M` | open the panel to mark it with a label |
| `g` | open the panel at "Go to" |
| `Home` / `End` | oldest / latest moment |
| `Esc` | close the panel, else clear the selection (the image viewer keeps its own Esc) |

Shortcuts are read only from key presses no widget captured, so typing in the search box or a label
field never triggers them.

## 1. Marks: user-made guide marks

**A mark is a moment in time with an optional short label** (`id`, `at` in Unix ms, `label` up to 120
characters, `created_at`, `updated_at`). It is not tied to a source or a picture: the room is one
timeline, and jumping to a mark puts the camera at `at`, where the stage shows whatever is on screen
there, in any lane the filter admits.

**Writes go through the CLI, like the tray.** The UI may not link `rsrewind-storage`
(`rsrewind-query/tests/ui_boundary.rs`), so it runs `rsrewind mark add --at-ms <ms> --label-stdin`,
`mark rename <id> --label-stdin` and `mark remove <id>` as child processes (no shell). The label goes
on the child's standard input, not in its arguments, so it is never shown in the process list and can
never be read as a flag. Reads go through `rsrewind-query` (`History::marks`), read-only. The window
re-reads marks with every recorder-status poll (5 s), with every window of moments it loads, after
every mark command, and when a rename or delete says "there is no mark" (changed elsewhere).

**Migration by keypress.** `rsrewind mark add` opens the store for writing, so the first mark after
an upgrade migrates a v1 database (with its backup), possibly while an older recorder still runs.
That recorder keeps working (v2 only adds a table) but an older binary started later refuses the
store. Migration semantics are unchanged; the window only says "Updating the history format…" when a
mark command has run for over a second while the store it opened was v1 (else "Working…"). The
reader checks for the `marks` table on every read, so the new mark shows without reopening.

**Storage: schema v2** (`0002_marks.sql`, `SCHEMA_VERSION` 1 → 2):

```
marks(id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL,
      label TEXT NOT NULL DEFAULT '' CHECK (length(label) <= 120),
      created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)
CREATE INDEX marks_at ON marks (at)
```

The existing migration machinery applies unchanged: a database with data is backed up to
`backups/recall-v1-<utc>.db` before the migration runs, a failed backup refuses the migration, and a
v1 binary refuses the v2 database (`SchemaTooNew`). Downgrading means restoring that backup.

**Reading older stores.** `rsrewind-query` used to require the exact schema version. It now reads any
version from 1 to `SCHEMA_VERSION` (`MIN_READ_SCHEMA = 1`): v2 only adds a table, every v1 read is
unchanged, and a read-only reader cannot migrate. Without this, every imported replica would be skipped
by the window after an upgrade until the next `rsrewind import` migrated it. A store without the
`marks` table simply has no marks. Newer-than-known is still refused.

**Validation.** A label is trimmed, may not contain control characters, and is at most 120 characters
(enforced in `Store` and by the table's `CHECK`). Reading, labels are cleaned again (control characters
dropped): the database is treated as user-editable input, as for source labels. A mark cannot be added
inside a deletion fence (a forgotten interval): like every other writer, it would put something back
into history the user erased.

**Lifecycle.**

- `forget` / `delete_range(since, until)` deletes marks with `since <= at < until`. A label is the
  user's own words about that time, and "forget" means the time is gone.
- Age retention (`retention_days`) deletes marks older than the horizon, with the history.
- Size retention (`max_bytes`) does **not** delete marks: it trims the oldest pictures to save bytes,
  marks cost a few bytes each, and a mark over trimmed history still says where something was.
  A mark with nothing recorded around it jumps to the gap there, which the window explains.
- Uninstall never touches them (they live in `recall.db` with the rest of history).

**Replication: local only, deliberately.** Marks are not written into segments and `rsrewind import`
never brings any. They are an annotation made by whoever looks at the window, about whatever lanes it
shows, so they belong to the data folder of that window, not to any probe's history. Changing the
segment format would also widen the import path, which still has no independent security review. If
marks should ever travel, that is a new segment section with its own review.

Marks live in the data folder's own `recall.db`. On a visualizer-only folder (only `sources/<id>/`
replicas, never recorded) the first mark creates that database; the window then shows an empty
"This machine" lane next to the probes (the same thing `rsrewind pause` already does there).

**Not for agents.** `rsrewind mark` is classified "not for agents" and the MCP server does not expose
marks. Read-only marks over MCP would be small, but labels are free text the user wrote to themselves,
the MCP disclosure text (`rsrewind mcp --enable`) would have to name them, and the ceiling has no
notion of a source-less row. Left for a deliberate decision.

## 2. Keyframes: automatic stops, derived at read time

No recorder change, no capture-path change, no schema change. `rsrewind-query` derives keyframes from
what the recorder already stores (`rsrewind_query::keyframes`, pure and unit-tested, fed by
`QueryDb::keyframes`):

| Kind | When | Priority |
|---|---|---|
| `resume` | the first observation after a stretch of at least 60 s with nothing recorded (`QueryDb::gaps`: recorder off or died, paused, idle/locked). The first moment of history counts only when the range looked at starts at least 60 s before it and the recorder was not running then; otherwise it is not a stop | 3 |
| `app_switch` | an observation whose application differs from the previous observation **on the same monitor** | 2 |
| `window_switch` | same application, different window | 1 |
| `visual_change` | the stored dHash (`visual_states.fingerprint`) differs from the previous state on that monitor by at least 24 of 64 bits: a different screen, not an edited one | 1 (scored by bits) |

Per-monitor comparison matters: with two screens, consecutive observations alternate monitors, so
comparing in time order alone would call every observation a switch. The previous observation of each
monitor before the range is looked up (bounded: the 64 observations started just before it).

**Thinning.** Candidates from every admitted lane are merged by time and thinned: within 15 s of a kept
stop, only the higher priority (then higher score, then earlier) survives. Without this, alt-tabbing
produces a stop per second. Ranges are capped at 20 000 observations per lane.

**Stepping.** `History::step_keyframe(filter, from, forward)` looks in widening windows (30 min,
doubling, bounded by the first and last observation) and returns the nearest stop strictly after (or
before) `from`. Thinning depends on the window, so a stop near a window's edge can differ between two
windows; stepping is still monotonic because each answer is strictly past `from`. A window denser
than 20 000 observations per source is read from the end the step needs: the newest rows for a
backward step, the oldest for a forward one, with each monitor's previous state taken from just
before the first row read.

Heuristics, honestly: the thresholds (60 s, 24 bits, 15 s) were chosen on synthetic history and need
tuning on real use. A stored fingerprint is `NULL` in history built by tests and old imports; those
states never yield `visual_change`.

## 3. Movement

- **Glide** (`ui::nav`): when the target is within the room's depth, the camera eases from where it is
  now (including mid-animation, so held arrow keys keep flowing) to the target in log-time, so the last
  part of the approach is slow and readable. 140-240 ms by distance, ease-out cubic.
- **Cue** (unchanged idea, brief §"jumping from search"): when the target is beyond the room's depth,
  the camera is placed just after it and settles back onto it in 180 ms, instead of flying through
  hours of history.
- Marks are drawn on the strip (gold flags, over all history) and in the room (a floor line at their
  depth, with the label); keyframes in the loaded window are ticks on the strip and a small tab on
  their card; picked cards get a mint border.

## 4. Selection and exports (added to the same branch, 2026-10-11)

**Selection** (`ui::pick`, pure): the moment on stage is still "what the detail pane shows"; the
*picks* are what exports act on. A plain click picks one moment and makes it the anchor; Shift-click
or a Shift+navigation key picks every moment from the anchor (or, with none, the moment on stage) to
the target, in timeline order across the lanes the filter admits, filled from history
(`History::later` paging, at most 2 000), not just the loaded window; Ctrl/Cmd-click toggles one.
The navigator shows "N picked · duration" and a clear button; the strip shows the span.

**Exports** (`ui::export`) run on a thread of the window's process with progress and cancel; the
recorder is another process and is never involved. Pictures come from `MediaReader` (no database).

| Export | Acts on | How |
|---|---|---|
| Copy image (`Ctrl+C`) | the one picked moment, else the one on stage | full size, `arboard` (X11 / Wayland data-control / Win32) |
| Save PNG | same | full size, `png` crate |
| GIF | 2-300 picked moments | `gif` crate, NeuQuant palette per frame, letterboxed to the first frame's size fitted to 480/720/960/1280 |
| WebM | same, only if `ffmpeg` is on PATH | PNG frames in a scratch folder + an `ffconcat` list with real durations, `ffmpeg -an -c:v libvpx-vp9`, scratch folder always removed |

- **Timing.** Each frame shows for the real time to the next moment, clamped to 0.25-1.5 s, so a
  ten-minute idle stretch is 1.5 s, not ten minutes; a clip longer than 60 s is sped up evenly
  (no frame under 60 ms). The last frame shows 1.5 s.
- **Guards.** At least 2 and at most 300 frames; at most 300 x 1280 x 800 pixels in total.
- **Where.** New files in `<data folder>/exports/` (no file dialog: it would need another native
  dependency; the path is shown after every export). Never overwrites: a name that exists gets a
  `-2` suffix, and a `.part` file (made with `create_new`) is renamed into place only when complete.
- **Crash-safe leftovers.** An export holds `.<name>.lock` with an exclusive lock for its whole run;
  its `.part` file and WebM's `.frames/` scratch folder are removed on success, failure, cancel or
  panic. A crash leaves them, so when the window opens and before every export,
  `export::sweep` removes `.rsrewind-*.part` / `.frames` / `.lock` whose lock is missing or held by
  nobody (older than 60 s), and nothing else. A stale scratch folder never blocks the next export.
- **Progress** redraws on a ~10 Hz tick, not every frame.
- **Leaving the store.** Every file export first asks, saying the file is a copy outside
  rsRewind's history that `forget` and retention will not delete (`PRIVACY.md`). Copy says so after.
- **Forgotten pictures.** A moment picked before a `forget` (or retention) has no file any more: the
  export stops with "the picture from ... is gone", deletes its partial file, and never writes a
  blank frame.
- **Silent.** rsRewind records no audio; `-an` keeps WebM silent by construction.
- **ffmpeg is an optional, run-time-detected exception, not a dependency.** Nothing links libvpx or
  ships FFmpeg; `ARCHITECTURE.md`'s "no FFmpeg until benchmarks justify it" is about storage and
  still holds. Without an `ffmpeg` on PATH the WebM button is disabled with a tooltip saying why.
  It is looked up once when the window opens.
- **Boundary.** New UI dependencies are third-party and local-only (`png`, `gif`, `arboard`, which
  brings `image`, `x11rb`, `wl-clipboard-rs`); no first-party crate was added, so
  `ui_boundary.rs` is unchanged and green. No CLI `clip` command was needed: the UI may spawn
  `ffmpeg` itself, as it spawns `rsrewind`.

## Tests

- storage: v1 → v2 migration with backup; reopen idempotent; newer refused; mark add/rename/remove;
  label validation; fence refusal; forget and age retention delete marks, size retention keeps them.
- query: marks read on v2, empty on a v1 store (which still opens); keyframe derivation (pure) and over
  a fixture store (resume after a gap, per-monitor app switch, thinning, stepping both ways).
- UI: navigation model (`nav`: glide/cue choice, easing, mark/keyframe neighbours, jump-time parsing,
  shortcut mapping); selection (`pick`); exports (`export`: GIF timing and guards, PNG round trip,
  a forgotten picture, cancel, no ffmpeg, and a real WebM encode when ffmpeg is installed).
- CLI: `mark` round trip through the binary; the command is classified for agents.
- Boundary tests unchanged and green: the UI still reaches only core and query.
