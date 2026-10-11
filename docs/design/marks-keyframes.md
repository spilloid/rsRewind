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
| `Shift+←` `Shift+→`, `PageUp` `PageDown` | previous / next keyframe |
| `[` `]` | previous / next mark |
| `m` | mark the moment on stage (no label; rename later) |
| `Shift+M` | open the panel to mark it with a label |
| `g` | open the panel at "Go to" |
| `Home` / `End` | oldest / latest moment |
| `Esc` | close the panel (or the image viewer) |

Shortcuts are read only from key presses no widget captured, so typing in the search box or a label
field never triggers them.

## 1. Marks: user-made guide marks

**A mark is a moment in time with an optional short label** (`id`, `at` in Unix ms, `label` up to 120
characters, `created_at`, `updated_at`). It is not tied to a source or a picture: the room is one
timeline, and jumping to a mark puts the camera at `at`, where the stage shows whatever is on screen
there, in any lane the filter admits.

**Writes go through the CLI, like the tray.** The UI may not link `rsrewind-storage`
(`rsrewind-query/tests/ui_boundary.rs`), so it runs `rsrewind mark add --at-ms <ms> --label=<text>`,
`mark rename <id> --label=<text>` and `mark remove <id>` as child processes (no shell; the label is one
argv element in `--label=` form so a label starting with `-` is never read as a flag), then re-reads
the list. Reads go through `rsrewind-query` (`History::marks`), read-only.

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
| `resume` | the first observation after a stretch of at least 60 s with nothing recorded (`QueryDb::gaps`: recorder off or died, paused, idle/locked); also the first moment of history | 3 |
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
windows; stepping is still monotonic because each answer is strictly past `from`.

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
  their card.

## Tests

- storage: v1 → v2 migration with backup; reopen idempotent; newer refused; mark add/rename/remove;
  label validation; fence refusal; forget and age retention delete marks, size retention keeps them.
- query: marks read on v2, empty on a v1 store (which still opens); keyframe derivation (pure) and over
  a fixture store (resume after a gap, per-monitor app switch, thinning, stepping both ways).
- UI: navigation model (`nav`: glide/cue choice, easing, mark/keyframe neighbours, jump-time parsing,
  shortcut mapping).
- CLI: `mark` round trip through the binary; the command is classified for agents.
- Boundary tests unchanged and green: the UI still reaches only core and query.
