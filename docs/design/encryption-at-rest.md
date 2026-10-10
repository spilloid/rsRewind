# Encryption at rest, without costing the user anything

Status: **exploration**, written 2026-10-10 against v0.0.5. Nothing here is built. Decision record:
`corporate-strategy/board/proposals/rsrewind-encryption-at-rest.md`.

Today anyone who can read the data folder can read the whole history: screenshots are plain WebP files,
and recognized text, window titles and app names sit in a plain SQLite database (`PRIVACY.md`,
"no encryption at rest"; issue #12 §2b). The maintainer's ask: SQLCipher for the database, authenticated
encryption for screenshots, OS-protected keys, and a recovery path that does not depend on one computer —
**and if usability degrades at all, it does not ship.** That last sentence is the design constraint, not a
preference, so it comes first.

## 1. The usability contract (ship gate)

Encryption ships only if every line below holds, measured on the maintainer's Plasma laptop and on kubert,
plaintext build vs encrypted build, same history:

| Promise | Budget | How it is measured |
|---|---|---|
| No new prompt, password or dialog, ever, in normal use (install, login, start, search, browse, tray, forget) | zero | scripted first run + login on both platforms |
| First run unchanged: the front door's two buttons are still the whole setup | zero extra steps | same |
| Search | p95 within 5 % of plaintext | `rsrewind search` over a fixed query set, 20 runs |
| Window open to first thumbnails | within 5 % | timed launch on the same history |
| Recorder CPU while the screen changes | within 5 % (absolute: < 1 percentage point) | `status` counters + `ps` over 10 min |
| Recorder start | within 250 ms of plaintext | timed `rsrewind start` |
| Disk use | within 2 % | data folder size after the same session |
| Forget, uninstall, moving to a new computer | no harder than today; ideally easier (see §6) | written walkthrough reviewed against `docs/reviews/2026-10-09-astra-usability.md` |
| Failure to reach the OS key store | recording keeps working (see §4.3), never a lock-out | fault injection |

If a budget is missed the feature stays behind a default-off setting until it is met. "Opt-in encryption" is
the fallback, not the plan.

## 2. What the threat model buys (and does not)

Protects against: a stolen or lost laptop/disk; another account on the same machine; backups, sync tools or
cloud folders that pick up the data folder; a copied data folder; forensic recovery of deleted pages, WAL
frames and old migration backups (they are ciphertext too).

Does **not** protect against: malware running as the user while they are logged in (it can ask the OS key
store exactly as rsRewind does), or someone using the unlocked session. Saying so plainly in `PRIVACY.md` is
part of the work.

## 3. Design

### 3.1 One data key, wrapped twice

- A random 256-bit **data key** per data folder, generated at first start.
- Stored only **wrapped**: (a) by the OS key store (§4), for daily use with no prompt; (b) by a **recovery
  key** (§5), so the history can be opened on another computer.
- Sub-keys are derived with HKDF-SHA256 from the data key with fixed labels: `db` (SQLCipher raw key),
  `media` (file encryption), later `segment` (export, with probe enrolment, V3). One key to protect, separate
  keys per use.

### 3.2 Database: SQLCipher

- `rusqlite` feature `bundled-sqlcipher-vendored-openssl` (SQLCipher 4, AES-256-CBC + HMAC-SHA512 per page).
  Every connection, including the read-only query connection, opens with `PRAGMA key = "x'<64 hex>'"`: a raw
  key, so no PBKDF2 on open (that is what keeps start-up and the window fast).
- FTS5, WAL, `secure_delete`, `fts5 secure-delete` and backups-before-migrate all keep working; the backups are
  encrypted copies.
- **Alternative to evaluate before choosing:** SQLite3 Multiple Ciphers (ChaCha20-Poly1305 or SQLCipher-compatible
  mode, no OpenSSL). Decide by build complexity on Windows + Linux CI and by the benchmark, not preference.
- Cost to the build: OpenSSL is compiled from source (needs Perl, present on CI runners and kubert; not Python).

### 3.3 Screenshots: one sealed file per picture

- XChaCha20-Poly1305 (RustCrypto `chacha20poly1305`, pure Rust, independently audited) or AES-256-GCM via
  `aws-lc-rs`/`ring`. Recommended: **XChaCha20-Poly1305**: random 192-bit nonces are safe without a counter,
  it is fast without AES hardware, and it adds no C.
- File: `magic "RSRE" | version | nonce | ciphertext+tag`, extension `.webp.rse`. Associated data = the
  file's relative media path, so a file swapped into another path fails to open instead of showing the wrong
  moment.
- Cost: a frame is ~100-300 KB; sealing/opening is microseconds against milliseconds of WebP work.
- Readers (`MediaReader`, OCR worker, segment export) open-and-verify in memory; nothing decrypted is written
  to disk.

### 3.4 What stays outside

`config.toml` (settings, privacy rules: no screen content) and logs (already content-free by rule) stay plain,
so `doctor` and the tray work even when the key store is unreachable. Segments leaving the machine stay as today
until probe enrolment (V3) gives them recipient keys; flagged, not hidden.

## 4. OS-protected keys, no prompts

| Platform | Wrap the data key with | Prompt? |
|---|---|---|
| Windows | DPAPI, current user (`CryptProtectData`); optionally TPM-backed via CNG later | none: unlocked by Windows sign-in |
| Linux (Plasma, GNOME) | Secret Service over the session bus (KWallet / GNOME Keyring; `zbus` is already a dependency) | none when the wallet is unlocked at login by PAM (default on Plasma 6 and GNOME) |
| Linux, no Secret Service or a locked wallet | see §4.3 | never a lock-out |
| macOS (later) | Keychain | none |

### 4.3 When the key store is unavailable

The one place this could hurt UX. Rule: **recording never stops because of encryption.**
- At first start with no reachable key store, generate the data key and keep it in `<data>/keys/local.key`
  (user-only permissions): protection equal to today's, `doctor` and the tray say "history is not protected by
  your login yet", and the key moves into the key store silently the first time one is reachable.
- A key store that is reachable but refuses (locked wallet that would prompt): do not prompt from a background
  recorder; keep recording into a short **sealed spool** (new frames encrypted to a per-session key that is
  itself wrapped with a public key whose private half is wrapped by the data key) and fold it in once unlocked.
  This keeps capture prompt-free; it is the most complex part and could be cut to "local.key fallback" first.

## 5. Recovery that does not depend on one computer

- A **recovery key**: 128 random bits shown as 8 groups of 4 characters (Crockford base32), like a BitLocker
  key. It wraps the data key (Argon2id is unnecessary for full-entropy keys; HKDF + XChaCha20-Poly1305 seal),
  and the wrapped copy lives in the data folder, so the folder plus the recovery key opens anywhere.
- **No UX cost at setup:** it is never demanded. The tray menu and the window show a quiet "Save your recovery
  key" item until it has been viewed once (and after that, "Show recovery key", behind the OS's own
  re-authentication where one exists: Windows Hello / polkit).
- Moving computers: copy the folder, open the window, it asks for the recovery key once (the only place a key
  is ever typed), re-wraps for the new machine's key store, done. Today that move needs file copying and an
  environment variable, so this is no worse and gains a guided step.
- Lose both the machine and the recovery key = history is gone. That is the point; it is said in the one
  sentence shown with the key.

## 6. Where encryption makes UX *better*

- **Erase everything becomes instant and complete**: destroying the data key (and its wrapped copies) makes the
  whole history, WAL, migration backups and stray files unreadable at once (crypto-shredding). Today "forget"
  has to hunt backups and cannot reach copies; with a key, an "Erase all history" button is honest.
- **Uninstall can offer "keep" or "erase" truthfully**, without walking the folder.
- **Sync tools and backups become safe by default**, which removes a caution the docs currently have to give.

## 7. Migration of existing history

Background and resumable, never blocking the window: `sqlcipher_export` into a new encrypted database, swap on
the next recorder start (backup-before-migrate rules apply), then encrypt media files oldest-first at
below-normal priority, each file replaced atomically; readers accept both forms until the folder is fully
converted. Schema/migration work is Opus tier under STD-001; the import path and key handling get an
independent review before release.

## 8. Stages

| # | Deliverable | Ship gate |
|---|---|---|
| E0 | Benchmark harness for the §1 budgets on plaintext (the baseline) | numbers recorded |
| E1 | Data key + OS key stores (DPAPI, Secret Service, local fallback) + recovery key, no consumers yet | no prompts in scripted runs |
| E2 | Media sealing (new files), readers accept both | §1 budgets |
| E3 | SQLCipher (new folders) | §1 budgets |
| E4 | Background migration of existing folders | resumable, crash-tested |
| E5 | "Erase all history", uninstall choice, PRIVACY.md update, independent review | review findings closed |

Open questions for the maintainer: SQLCipher vs SQLite3 Multiple Ciphers (decide on E0/E3 numbers);
whether the sealed spool (§4.3) is worth its complexity or "local.key until unlocked" is enough.
