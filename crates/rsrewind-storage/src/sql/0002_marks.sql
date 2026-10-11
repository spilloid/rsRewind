-- Schema v2: marks, the user's own guide marks on the timeline (docs/design/marks-keyframes.md).
-- A mark is a moment in time with an optional short label; it belongs to no source and no picture.
-- Written only through `Store` (`rsrewind mark ...`); `forget` and age retention delete the marks in
-- their range, size retention keeps them. Never exported in segments: marks are local to this data
-- folder. This file is embedded in the binary and must never change once released.

CREATE TABLE marks (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    at         INTEGER NOT NULL,
    label      TEXT NOT NULL DEFAULT '' CHECK (length(label) <= 120),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX marks_at ON marks (at);
