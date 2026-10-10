# Astra (gpt-6-astra, high, read-only) usability roleplay of v0.0.5 @ 9f87d7a

Recorded verbatim (2026-10-09). The maintainer asked for an adversarial usability evaluation as a user who "just wants to remember shit easily". It is a walkthrough of the repository (docs, site, CLI help, UI and tray strings, installer), not a live GUI test; its scores and Sam's reactions are its own inferences. Spot-checked by the orchestrator before filing: the Start-menu shortcut runs `ui` only (`installer/rsrewind.wxs:105`), the empty window has no start action (`crates/rsrewind-ui/src/app.rs:810`), the MSI adds no PATH entry, and `Text not recognized yet.` is the Linux detail-pane wording (`app.rs:1259`). Absolute paths in links below are from the reviewer's checkout.

---

**1. Verdict in three sentences**

rsRewind offers Sam something compelling: a searchable memory of things seen across applications, with the surrounding screenshot preserved locally. Today, getting that memory requires terminal work, Linux cannot deliver text search over its own recordings, and unresolved privacy problems undermine Sam’s confidence in pausing and forgetting. The tray is a meaningful improvement, but v0.0.5 still asks Sam to administer the machinery before enjoying the magic.

*Method:* Repository-based walkthrough, not a live GUI test. I read the requested documents and relevant implementation, ran `target/release/rsrewind --help` and every listed subcommand’s `--help`, and changed no files. `help --help` rejects `--help`; plain `help` works. Scores and Sam’s reactions are inferred; later beats assume Sam gets help past earlier blockers.

**2. Why the concept is genuinely cool**

Sam remembers a fragment—“toner,” a price, somebody mentioning a deadline—without remembering which application contained it. rsRewind can connect recognized words to a timestamp, application, window and screenshot, then let Sam inspect adjacent moments. That preserves visual context which a bookmark or copied sentence would miss. ([README.md:37](../../README.md:37))

For Sam, the distinctive promise is *finding something without having decided to save it beforehand* (inferred). Search results actually cue the rewind room; this central design idea is implemented, not merely illustrated. Imported machines can share that view, and recording needs no account or cloud service. ([app.rs:395](../../crates/rsrewind-ui/src/app.rs:395), [site/probes.html:41](../../site/probes.html:41))

**3. The story walk**

**1 — Discovery**

**Expectation:** “Tell me what this remembers and how I get it back.”

**Reality:** “Rewind anything you did” and “Type a few words you remember” communicate the benefit quickly. Screenshots explicitly disclose made-up history. But Linux appears beside Windows near the install button; its lack of text recognition is explained much later. The README is more upfront, then quickly enters Rust/MSVC build instructions. ([site/index.html:25](../../site/index.html:25), index:94; README:57–94)

**Score: 2/5.**  
*Sam:* “Yes. That thing I keep wishing my computer could do. Wait—does the search part work at home?”

**2 — Install: work Windows, home Linux**

**Expectation:** Download, double-click, follow a few screens.

**Reality:** Windows offers a per-user MSI without administrator rights. Verification asks Sam to run `Get-FileHash`, `Get-Content` and `Get-AuthenticodeSignature`, then inspect the signer. First-run instructions immediately require `rsrewind doctor`. The MSI installs no PATH entry, so the documented bare command is not made available by the installer alone (inferred from its complete manifest). ([site/install.html:26](../../site/install.html:26), install:37–52; installer/rsrewind.wxs)

Linux requires `sha256sum --check --ignore-missing SHA256SUMS.txt`, `tar -xzf …`, and `install -m 755 … ~/.local/bin/rsrewind`. The instructions assume that destination exists and commands there are discoverable. Recording specifically requires Plasma **6 on Wayland**, not merely “KDE.” (install:58–82)

**Score: Windows 4/5; Linux 5/5.**  
*Sam:* “I wanted yesterday’s recipe. Why am I checking a certificate and installing something into a dot folder?”

**3 — First run and next login**

**Expectation:** Open rsRewind, agree to recording, see confirmation.

**Reality:** The Windows shortcut launches `ui`, not recording. The empty window says “Nothing on tape yet.” and “rsRewind will start building your local history once recording begins.” It offers no start button. Sam must discover `rsrewind start`, then `rsrewind status`; the tray also needs launching. ([installer/rsrewind.wxs:100](../../installer/rsrewind.wxs:100), app.rs:805–812)

Login startup **does exist**: `rsrewind tray --autostart on` registers both tray and recorder on Windows and Linux. It does not launch them immediately. The website describes only the icon and additionally teaches Linux users to author `~/.config/systemd/user/rsrewind.service`. The MSI’s separate startup feature is off by default and has no checkbox. ([main.rs:731](../../crates/rsrewind-cli/src/main.rs:731), main.rs:778–850; install:64–81; installer:78–85)

**Score: 5/5.**  
*Sam:* “‘Once recording begins.’ Lovely. Who begins it? Apparently me, using another spell.”

**4 — Living with it**

**Expectation:** Quiet background recording, obvious state, sensible privacy and storage controls.

**Reality:** The tray has recording/paused/stopped wording and badges, plus “Quit the tray icon (recording continues).” Five minutes without input stops new storage; settings require `config.toml`. Defaults advertise 90 days/50 GB. Reported Linux consumption is 15–30% of one CPU core while screens change. ([model.rs:68](../../crates/rsrewind-tray/src/model.rs:68), model:223–268; install:88–91; CHANGELOG:121)

The tray has no distinct idle state; CLI status labels a live, unpaused recorder “recording.” Leaving a matching excluded window visible skips the whole monitor by design. Bank pages are not a documented default exclusion category. (model:15–24; main.rs:452–456; PRIVACY:48–66)

**Score: 3/5.**  
*Sam:* “The little menu makes sense. But ‘Recording’ doesn’t explain why my afternoon is missing.”

**5 — The money moment: yesterday’s thing**

**Expectation:** Type a remembered word or choose Yesterday; get the picture within seconds.

**Reality:**

- **Search:** `rsrewind search toner` returns text, metadata and an `Image:` path—not an opened screenshot. Default limit: 20. ([render.rs:28](../../crates/rsrewind-cli/src/render.rs:28); executed search help)
- **Recent:** `rsrewind recent` returns 20 textual rows containing timestamps, applications, titles and `[id …, ocr …]`. Its help exposes no date filter. (render.rs:63–85; executed recent help)
- **Window:** “Search anything you remember…” leads to clickable results and screenshot detail; double-click enlarges the picture. But the sidebar contains sources, not Yesterday or application filters. Navigation offers “← Earlier,” “Later →,” “Latest,” and “depth …”. Search takes only text plus source selection, capped at 100. (app.rs:618–635, 842–940, 1114–1233)
- **Linux:** Local screenshots remain browsable but lack searchable recognized text. The detail pane says “Text not recognized yet.” That sounds like waiting will help (inferred). Imported Windows text remains searchable. (CHANGELOG:52–53; app.rs:1258; README:107)

First opening on substantial history can leave thumbnails blank for 20 seconds or more. (CHANGELOG:56–57)

**Score: Windows 4/5; Linux 5/5.**  
*Sam:* “I know it was yesterday. Why am I steering through screenshot space? And how long is ‘yet’?”

**6 — Bank site; forget the last ten minutes**

**Expectation:** Pause means safe now; forget gives clear confirmation.

**Reality:** “Pause until resumed” is readily available. “Forget the last 10 minutes…” changes to “Click again to delete the last 10 minutes for good”; confirmation expires after ten seconds. (model.rs:190–205)

But pause lacks recorder acknowledgement: “Paused” can precede completion of an in-flight tick. Forget acts on the selected data store, not every imported machine; backups and delivered copies can survive. The tray discards the command’s output and errors, hiding both deletion receipts and warnings. ([docs/remediation-status.md:15](../../docs/remediation-status.md:15), main.rs:674–706; [tray/lib.rs:68](../../crates/rsrewind-tray/src/lib.rs:68))

**Score: 5/5.**  
*Sam:* “‘For good’ had better mean something. Did it delete it, fail, or leave another copy somewhere?”

**7 — Both machines together**

**Expectation:** Add Work Laptop and Home Desktop once.

**Reality:** Run `rsrewind export`, locate `outbox`, move files, then `rsrewind import <folder>` on the viewing machine. Export excludes the newest unsettled history—five minutes by default. The website supplies `schtasks` and `robocopy`; automatic export/import watch modes remain planned and are absent from help. ([site/probes.html:39](../../site/probes.html:39), [probe-visualizer.md:34](../../docs/design/probe-visualizer.md:34))

**Score: 5/5.**  
*Sam:* “My memory app needs me to remember to transport its memory.”

**8 — Something is missing**

**Expectation:** Explain the missing time and offer a fix.

**Reality:** v0.0.5 genuinely adds gap bands, durations, stepping notices, and `rsrewind gaps --since 24h`. However, one explanation is “nothing stored (privacy rule or capture problem)”; these causes remain indistinguishable. Window gap coverage stops at 30 days. The tray’s error instruction is “Recorder error: see `rsrewind doctor`.” (CHANGELOG:32–41; [gap.rs:30](../../crates/rsrewind-core/src/gap.rs:30); model.rs:74)

Worse, documented writer overload can drop changed screens while extending the previous picture’s span. That can make missing content look like continuity. (CHANGELOG:60–61)

**Score: 4/5.**  
*Sam:* “‘Privacy rule or capture problem.’ So either you protected me or broke. Excellent diagnosis.”

**9 — Uninstall or move**

**Expectation:** Clear choices: keep history, erase history, or move everything.

**Reality:** Windows uninstall intentionally preserves `%LOCALAPPDATA%\rsRewind`; no `purge` command exists. Linux instructions create a binary, screenshot-permission entry, autostart entry and optionally a service, without a matching removal walkthrough. Migration is documented as copying the data folder and optionally setting `RSREWIND_DATA_DIR`. ([docs/installer.md:68](../../docs/installer.md:68); install:60–82; README:135–153)

**Score: 4/5.**  
*Sam:* “Keeping my memories is good. Making me locate all the embarrassing leftovers myself is less good.”

**4. Why it is ergonomically “like trying to hump a toaster” right now**

Ranked by how much they prevent Sam getting value:

1. **Installation assumes terminal literacy.** Verification, installation and first use are command sequences; Windows bare commands also lack installer PATH support. Evidence: install:37–65, installer manifest. **Hurts:** Sam on both machines.
2. **Opening the app does not get recording started.** “Nothing on tape yet.” has explanation but no action. Evidence: app.rs:805–812; shortcut `Arguments="ui"`. **Hurts:** every first-time user.
3. **Home search cannot fulfil the headline promise.** No Linux recognition, yet “Search anything you remember…” and “Text not recognized yet.” Evidence: CHANGELOG:52; app.rs:842,1259. **Hurts:** Linux Sam.
4. **Privacy controls overstate certainty.** Pause reports success before acknowledgement; exclusions require process names/title patterns in TOML; plaintext history remains readable to anyone with folder access. Evidence: remediation:15–16; PRIVACY:48–57,135–148. **Hurts:** bank/password-conscious Sam.
5. **The emergency delete button hides its receipt.** “For good” appears before execution; results, failures and surviving-copy warnings disappear into discarded output. Evidence: model:203; tray/lib:76–82; main:691–706. **Hurts:** embarrassed Sam most.
6. **Yesterday is a navigation exercise.** No date/app controls; 100-result ceiling; empty search advises “Try another phrase, application, or time range” without corresponding controls. Evidence: app:630–635,869–940,1156. **Hurts:** anyone remembering context instead of exact words.
7. **Two-machine use is a recurring courier job.** Export, move, import; no shipped watch mode. Evidence: probes:41–46; executed help. **Hurts:** work/home Sam.
8. **Missing memories can masquerade as an unchanged screen.** Dropped changes extend the old image’s span. Evidence: CHANGELOG:60–61. **Hurts:** anyone trusting chronology.
9. **Health information lives outside the main task.** Window shows “sources · moments,” not recording state; failures send Sam to `doctor`; privacy gaps lack specific causes. Evidence: app:849–861; model:74; gap:36. **Hurts:** users troubleshooting empty results.
10. **Leaving or moving requires filesystem housekeeping.** Preserved history, backups, permission/startup files and manual migration lack one guided surface. Evidence: installer docs:68–83; install:66–82; README:151. **Hurts:** users changing computers or withdrawing trust.

**5. Fixes ranked by impact ÷ effort**

*Relative effort estimates are inferred. All proposals preserve one executable, native UI, local storage, separate capture, and privacy checks before persistence.*

1. **Correct capability and startup wording — small; #1–3, #9.** Put “Linux: visual browsing only” beside download/search. Replace indefinite “yet” with the actual limitation. Document that tray autostart starts both processes.
2. **Show action results — small/medium; #5, #9.** Return structured deletion results to the tray: deleted count, failures, surviving backups and copies, explicit machine scope. Offer clickable diagnostics.
3. **Add Yesterday/Today/date and app filters — medium; #6.** Expose existing query fields; add result pagination and a simple chronological thumbnail list alongside the room.
4. **Make first launch actionable — medium; #1–2.** Native setup with “Start recording,” preview, exclusions review and “Start at login.” Invoke the same executable’s commands from the separate UI process.
5. **Show trustworthy state everywhere — medium; #9.** Recording/paused/idle/error, last saved moment, storage usage, recognition availability and loading progress in the window.
6. **Guide exclusions, cleanup and migration — medium; #4, #10.** Friendly application/window rule selection, folder picker, explicit keep/delete choices, and an inventory of remaining copies.
7. **Close pause/privacy and persistence defects — substantial; #4, #8.** Acknowledge pause only after capture reaches the barrier; complete frame provenance/fail-closed work; report dropped changes honestly. This is a release prerequisite despite lower effort efficiency.
8. **Implement Linux recognition — substantial; #3.** Local engine plus backlog processing and clear progress; benchmark against the existing CPU cost.
9. **Automate the file workflow — substantial; #7.** Implement planned watch modes, folder selection, freshness and rejection reporting. The no-built-in-transport rule itself costs convenience: Sam must still arrange file movement. Improve that boundary without disguising it as automatic sync.

**6. What NOT to change**

- Search selecting the actual historical moment and showing its surrounding context.
- Local operation without accounts, telemetry or cloud dependency.
- The native viewer’s full-picture zoom, adjacent-moment stepping and system theme.
- Tray language such as “Pause until resumed” and “Quit the tray icon (recording continues).”
- Separate UI/capture processes; quitting the viewer should remain harmless to recording.
- Explicit deletion confirmation and preservation of history during uninstall.
- Conservative whole-monitor exclusion intent, repeat-safe imports, and honest release warnings.

The brief’s “clarity at the center” remains the right standard: keep the personality, and give Sam a visible way to get yesterday back.
