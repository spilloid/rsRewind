# rsRewind design brief

Operator-supplied design brief, recorded verbatim on 2026-09-30. This is the foundation for logo
exploration, the brand board, wireframes, the WinUI visual system, motion, archive/library
concepts, marketing visuals and iconography. Decisions taken against it are recorded in
`docs/design/decisions.md`.

North star: **Nostalgia at the edges, clarity at the center.**

---

rsRewind is a local-first personal activity recorder and searchable work-history system. It captures screen state, application/window context, OCR text, and eventually richer contextual data so the user can search and revisit what happened on their Windows desktop.

The design goal is not generic productivity software.

The product should feel memorable, warm, polished, slightly playful, and distinctly tied to the concept of **rewinding through recorded personal history**.

The intended visual metaphor draws from:

- DVRs
- TiVo
- VHS / cassette-era media
- old-school rewind/playback controls
- recording libraries
- tapes on shelves
- archived sessions
- transport controls
- media timelines

However, this must remain a serious, usable Windows application.

The design principle is:

**Nostalgia at the edges, clarity at the center.**

The nostalgic/media metaphor should primarily shape branding, archive/library browsing, motion, timeline interaction, empty states, and product personality.

Core working surfaces such as search, privacy controls, settings, diagnostics, storage configuration, and query results should remain highly legible and practical.

Do not create a novelty VHS simulator.

Do not use heavy fake skeuomorphism.

Do not sacrifice usability for nostalgia.

## Brand relationship

rsRewind is a product from **Spillers Technology**.

The broader Spillers Technology design language currently includes:

- satin-black backgrounds
- warm ivory text
- gold as a studio-level accent
- subtle violet, sky, mint, and rose supporting accents
- restrained gradients
- soft radial glows
- subtle grain
- expressive serif typography for marketing/display work
- clean modern sans-serif typography for body/UI
- a crafted, workshop-like tone
- operator-built software rather than generic SaaS

The broader studio should feel like: **a technical workshop building careful, practical tools**

rsRewind should feel like: **a distinctive instrument built in that workshop**

Do not simply clone another Spillers Technology product. rsRewind should inherit the family resemblance while having its own recognizable identity.

## Product personality

Keywords: rewind, recall, recorded, archived, tactile, warm, playful, trustworthy, local, inspectable, personal, operator-focused, media-like, archival, responsive, calm, polished.

Avoid: cyberpunk, hacker aesthetic, neon overload, corporate blue SaaS, sterile dashboard UI, retro parody, full skeuomorphism, fake CRT effects, VHS static everywhere, scanline overlays, fake woodgrain, fake plastic VCR panels, novelty nostalgia.

## Product name

Human-facing: **rsRewind**. Machine-facing: `rsrewind`. Primary binary: `rsrewind.exe`.

Possible product tagline: **Rewind anything you did.**

Supporting product language: **Local. Searchable. Yours.**

More descriptive technical line: **A local-first, searchable record of what happened on your Windows desktop.**

## Core metaphor

The core metaphor is: **personal DVR / personal recorded archive**

The user's historical activity should conceptually feel like a media library containing recordings of past sessions.

Possible concepts:

- sessions as recordings
- days as archive collections
- moments as cues
- visual states as frames
- search results as indexed moments
- timeline navigation as scrubbing
- jumping to a result as cueing to a moment
- pause/resume as actual recording controls
- export as archival packaging

The user should feel: **"This is my personal archive. I can go back and inspect exactly what happened."**

## Strongest UX principle

Search and timeline are not separate products. Search should be a navigation mechanism into recorded history.

Example interaction:

1. User searches for `Konica printer`.
2. Search results show historical matches with timestamp, application, window title, OCR snippet, optional thumbnail.
3. User selects a result.
4. The application jumps directly to that moment on the timeline.
5. The full visual context becomes visible.

The product revolves around time. Time should therefore be visually central rather than hidden inside a secondary "History" page.

## Primary application structure

The main UI should behave like a serious Windows viewer/browser. A preferred conceptual layout:

- top: rsRewind identity, prominent search field, recording status
- left: date/history navigation, session navigation, application filters, archive/library access
- center: selected captured visual, search results, OCR/context
- bottom: persistent timeline / activity filament

```text
┌───────────────────────────────────────────────────────────────┐
│ rsRewind      [ Search anything you remember... ]      ● REC │
├───────────────┬───────────────────────────────────────────────┤
│               │                                               │
│ Today         │               CAPTURE                         │
│ Yesterday     │                                               │
│ This Week     │       [ selected screenshot ]                 │
│               │                                               │
│ Apps          │  3:42:17 PM                                   │
│ Teams         │  PowerShell                                   │
│ Edge          │  Administrator: Windows PowerShell            │
│ Terminal      │                                               │
│               │  OCR / context                                │
│               │                                               │
├───────────────┴───────────────────────────────────────────────┤
│ 9 AM      10       11       12       1       2      ●3:42    │
│ ──●────●──────────●●─────────●────────●●●───────●──────────  │
└───────────────────────────────────────────────────────────────┘
```

This is conceptual, not mandatory pixel-for-pixel.

## Two major UI modes

The product should balance personality and utility by separating two visual modes.

### Archive / Library

This is where the retro-media personality can be strongest. Use session cards, day groups, recording collections, tape-label-inspired visual elements, media-shelf organization, larger previews, playful transitions, archive-style metadata.

A session may visually resemble a simplified labeled recording. Example fields: date, start/end time, duration, top applications, thumbnail strip, activity density, session title if one exists.

Do not literally render photorealistic cassette tapes. Think **media spine / archival card / labeled recording** rather than **3D VHS cassette simulator**.

### Recall / Viewer

This is the main productivity surface. It should prioritize screenshot visibility, fast scrubbing, timestamp clarity, application/window metadata, OCR, search, filters, timeline movement. This mode should remain clean and restrained.

## Timeline

The timeline is one of the central brand and UX components. Introduce a recurring visual motif called the **timeline filament**:

```text
────●──────●──●────────────●──────
```

It represents recorded states, activity density, bookmarks, search matches, transitions, current playback position.

The timeline filament may appear in the main application timeline, branding graphics, website illustrations, social cards, splash/loading states, session cards, empty states. This motif should become strongly associated with rsRewind.

The active timeline position may have a subtle glow. Avoid excessive glow or gaming-style effects.

## Motion language

Motion should reinforce the metaphor of moving through time. General behavior: subtle, short, responsive, directional.

When scrubbing backward in time: previous visual states may enter from the left; current content may subtly shift right; movement should communicate backward temporal direction. When scrubbing forward: inverse motion.

Target transition length: approximately 100–180 ms for routine navigation.

When jumping from search: use a cleaner crossfade or short cue animation rather than pretending the user scrubbed through every intermediate moment.

When loading a historical session: a subtle "loaded recording" animation may be used (session card slides into viewer, timeline playhead snaps into position, recording label settles into place).

Do not use long theatrical transitions.

## Nostalgia levels

The design should primarily operate at levels 1 and 2.

- **Level 1 — Core metaphor (required):** rewind language, record status, timeline, media-history vocabulary, recording/session concepts, archive/library, simple transport metaphors.
- **Level 2 — Product delight (recommended):** shelf-style archive browsing, session cards resembling labeled recordings, filmstrip-like thumbnail preview, playback-head style timeline indicator, charming empty states, loading/cueing transitions, subtle media-library personality.
- **Level 3 — Novelty nostalgia (avoid or use extremely sparingly):** CRT effects, VHS snow, fake scanlines, realistic cassette renders, chrome knobs, fake wood paneling, large hardware-style buttons, constant analog distortion.

## Shape language

Prefer: rounded rectangles, medium-radius panels, pill-shaped metadata labels, thin slot-like separators, small circular status indicators, recording LEDs, index-tab motifs, tape-label-inspired cards, subtle concentric reel/ring geometry, segmented indicators, timeline ticks.

Avoid: sharp sci-fi geometry, excessively bubbly mobile UI, huge card grids, dashboard-card overload.

## Logo direction

The primary logo should NOT be a literal double-triangle rewind symbol (`⏪`). It is too generic.

- **Direction 1 — Rewind R (preferred).** A custom `R` mark where the letter incorporates counter-clockwise motion, rewind movement, a continuous timeline stroke, possibly an arrow integrated into the bowl or leg. At small sizes it should clearly read as an `R`; at larger sizes the rewind detail should become apparent. Must work at 16x16 tray icon, 32x32 titlebar/application icon, 48x48 / 64x64, 256x256 installer/application art, GitHub avatar, marketing assets. Do not include `rs` in the icon itself; the `rs` Rust association belongs in the wordmark.
- **Direction 2 — Timeline aperture.** A simplified circular or rounded-square mark that combines time, frames, playback head, rewind direction, archive. Possibly use negative space to imply an `R`.
- **Direction 3 — Abstract reel.** Paired circles or a flowing path to reference tape reels. Must remain highly abstract. Avoid resembling podcast apps, audio players, camera apps. This is the most risky concept.

Supporting motifs (not necessarily part of the main logo): two subtle reel circles, playback head, indexed frame marks, tape spine, archive label, cue marker, filmstrip preview, recording LED, timeline tick pattern.

## Color system

The broader studio uses warm gold. rsRewind should have its own product accent. Preferred direction: **soft periwinkle / video-tech blue**.

```text
Canvas          #09090B
Surface         #121218
Surface Raised  #191920

Text            #F8F3EB
Text Secondary  #C8C1B7
Text Tertiary   #9C968E

Rewind           #8EA6FF
Rewind Bright    #BDC9FF
Rewind Deep      #697FDB

Studio Gold      #E8BD72
Studio Gold Hi   #FFE2A9

Success / Ready  #74D4B5
Record / Danger  #EF98B1
```

Interpretation: periwinkle = time, playback, recall; rose/red = actively recording / destructive actions; mint = healthy/indexed/complete; gold = Spillers Technology identity and publisher relationship. Do not use gold as the primary application interaction color. rsRewind should remain distinguishable from the parent studio.

## Light mode

Support native system light/dark themes. Do not design only for dark mode. Light mode should remain warm rather than pure white.

```text
Warm Canvas     #F0EBE3
Warm Surface    #FBF7F0
Primary Ink     #171419
Muted Ink       #645F63
```

The goal is **warm archival paper + modern Windows** rather than bright corporate white.

## Typography

- Marketing / branding: Display **Fraunces** (fallback Iowan Old Style / Georgia); Body **Manrope** (fallback Segoe UI). Serif for expressive headlines and brand moments only.
- Native application UI: **Segoe UI Variable**. Technical details (diagnostics, IDs, paths, timestamps, SQL): **Cascadia Mono**. Fraunces only for rare brand moments (About, splash, marketing), if at all.

## Windows-native visual direction

The application uses WinUI 3, Windows App SDK, windows-reactor. Embrace native Windows interaction: Mica, Acrylic where justified, Fluent context menus, Fluent icons, native dialogs, Windows focus behavior, correct DPI behavior, native keyboard navigation, system theme integration. Custom branding sits on top of Windows rather than replacing it. Do not recreate every control from scratch.

## Recording status

Always easy to discover. States: Recording, Paused, Error. A small red/rose light: `● Recording`; paused: `○ Paused`. No aggressive flashing. No stealth behavior.

## Search results

Should feel like **indexed moments from the archive**: timestamp, application, window title, OCR/context snippet, optional thumbnail, match emphasis. The selected result jumps directly to the corresponding timeline point. Avoid treating search like a generic document-search page.

## Session cards

Borrow from DVR recording cards, cassette labels, tape spines, library index cards.

```text
SEP 30
Morning Work Session

10:02 AM – 1:14 PM
3h 12m

Teams · Edge · PowerShell

[thumbnail strip]

42 indexed moments
```

Optional: top/bottom label strip, archive ID, subtle tape-spine edge, activity density stripe, small rewind icon. Keep them flat and contemporary.

## Thumbnail filmstrip

Strongly aligned with the product: hover over timeline, session preview, search result context, recent recordings, fast scrub preview. Keep filmstrip cues subtle; no literal sprocket holes unless used very sparingly.

## Empty states

A good place for personality.

- No recordings: **Nothing on tape yet.** rsRewind will start building your local history once recording begins.
- No search results: **No moment found.** Try another phrase, application, or time range.
- Paused: **Recording paused.** Your timeline will stay quiet until you resume.

Use retro-media language gently. Do not make every message into a joke.

## Tone of voice

Concise, warm, clever, technical, confident. Media language only when natural.

Good terminology: Rewind, Recording, Session, Archive, Library, Cue, Timeline, Jump to moment, Review, Pause recording, Resume recording, Recently recorded, Open archive.

Avoid overly cute copy: "Pop in a tape!", "Radical!", "Be kind, rewind!", "Insert cassette!". The interface should not become parody.

## Tray icon

Readable at tiny sizes. Preferred: simplified rewind `R`, extremely clean monochrome treatment where required. No gradients at 16px. Tooltips: `rsRewind — Recording`, `rsRewind — Paused`.

## Splash / launch screen

Possibly one of the strongest brand moments: black/satin background, rewind R mark, subtle timeline filament, one slowly moving cue light, rsRewind, "Rewind anything you did." Keep launch fast; do not block launch for animation.

## Marketing visuals

May lean more heavily into the media metaphor than the app: abstract media archive, rows of labeled recordings, glowing time filament, archive shelf, rewind motion, blurred screenshot strips, soft reel geometry, periwinkle/gold interplay. Use Spillers Technology DNA: satin black, subtle grain, restrained glow, gold publisher accent, elegant typography.

## Product relationship lockup

`rsRewind / by Spillers Technology` or `rsRewind / A Spillers Technology project`. Do not over-emphasize the parent brand inside the main application; the app identity stays rsRewind-first.

## Design directions to explore

- **Concept A — Modern Rewind:** most native WinUI, minimal nostalgia, rewind R logo, periwinkle accent, strong bottom timeline, subtle playback motion, very clean viewer.
- **Concept B — Archive Shelf:** strongest library/media metaphor, labeled session cards, tape-spine-like archive organization, playful transitions, warm/archival details; the viewer remains clean once a recording is opened.
- **Concept C — Operator DVR:** denser, workstation-oriented, strong recording light, compact metadata, timeline feels like professional media transport, slightly less cute, strongest "tool" identity.

Do not force the project to choose immediately; use these explorations to determine the correct nostalgia intensity.

## Accessibility

Strong text contrast, Windows scaling support, keyboard navigation, focus indicators, screen-reader semantics, color never the sole status signal, respect reduced-motion preference; animations optional/reduced when requested by Windows settings.

## Design restraint rules

Do: use nostalgia through metaphor; use motion to explain temporal navigation; make the archive fun to browse; make the viewer extremely usable; allow Windows-native UI to remain visible; keep the main application calm.

Do not: skin every native WinUI control; turn the application into a fake VCR; use oversized decorative graphics in working views; add constant animations; overload panels with glow; build generic SaaS dashboard cards; put gradients on everything; create a new custom component where a native one works well.

## Summary

**A beautifully crafted modern Windows application that feels like a personal DVR for your digital workday.** Practical enough to use for hours, charming enough to remember, nostalgic without being kitschy, native without being generic, playful without being childish, visually related to Spillers Technology without looking like a recolored clone.
