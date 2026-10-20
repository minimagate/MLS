# M4 TeXmacs live mode: research (PARTIAL)

Status: **partial checkpoint**, written 2026-10-20 when the session was paused. Questions 1, 2 and 5 are not yet executed, and the Q4 scenario script needs a fix before the remaining framing cases can run. Resume from "Remaining work".

Target: TeXmacs 2.1.5 bundle at `/Applications/TeXmacs.app/Contents/Resources/share/TeXmacs/` (paths below are relative to `progs/` unless stated). The bundle's `progs` tree was read, not the user's patched `~/.my-texmacs` copy (not read, per the constraint on that directory). `TEXMACS_PATH` in the user's shell points at `~/.my-texmacs/TeXmacs`, so the findings below apply to the stock 2.1.5 bundle and must be re-checked against the patched tree before M4 code relies on them.

Harness (all executed, scratch only under `scratchpad/m4-research-tm/`):
- Every run used fresh `HOME` and `TEXMACS_HOME_PATH` directories, `TEXMACS_PATH` set to the bundle, and `perl -e 'alarm 120; exec @ARGV'`.
- `-H` (headless) mode: the process exits as soon as `-x` returns, so no timers or event loop run. This explains the M3 finding that `delayed` never fired.
- Non-headless with `QT_QPA_PLATFORM=offscreen` (Qt offscreen platform plugin shipped in `Contents/Plugins/platforms/libqoffscreen.dylib`): the real Qt event loop runs without a window. This is the mode used for all timer and async results below. It is an approximation of the GUI and is labelled as such.
- Stub engine: `scratchpad/m4-research-tm/daemon/probe_daemon.py` (line-delimited JSON in, DATA frames out).

Legend: **EXECUTED (headless -H)**, **EXECUTED (offscreen Qt loop)**, **NOT EXECUTED**, **GUI-ONLY** (cannot be checked in either mode).

## Q3. Timers and idle (EXECUTED, offscreen Qt loop)

- `delayed` is a macro (`kernel/texmacs/tm-dialogue.scm:113`) that builds a closure and passes it to `exec-delayed-pause`. The closure returns `#t` when done, or a number of ms to wait before the next call.
- A plain body is wrapped as `(lambda () body... #t)` (`delayed-sub`, tm-dialogue.scm:40-44), so `(delayed (:every N) body)` runs **once**, not repeatedly. Repetition needs a chain: the body schedules the next `delayed` and returns `#t`.
- Observed: `exec-delayed`, `(delayed (:pause N))`, and a chained 100 ms tick all fired. The chained tick ran 12 times at about 100 ms intervals (log in `runs/e2e.*`). `(delayed (:idle 300))` fired after about 300 ms without input.
- `(idle-time)` returns ms since the last user input and grows while idle. `(change-time)` is a timestamp that stayed constant while nothing changed; I did not yet measure how it moves on an edit.
- A raw `exec-delayed` thunk that returns a number did **not** reschedule in my test. Use the chain pattern instead, and do not rely on the numeric return.
- `-H` exits after `-x`, so headless tests that need timers must run without `-H`.

## Q4. Persistent engine connection (partly EXECUTED, offscreen Qt loop)

Setup: `plugin-configure` with `(:launch "env PROBE_LOG=... python3 probe_daemon.py [banner]")`. The plugin's init file is copied into a fresh `TEXMACS_HOME_PATH/plugins/`. `(lazy-plugin-force)` starts it. Plugin registration works (`plugin-list` contains the probe).

| Case | Result | Status |
|---|---|---|
| `connection-start` | returns `"ok"` | EXECUTED (offscreen) |
| `connection-eval` with a string request | returns the next frame as `(document "...")`; payload is text after `verbatim:` | EXECUTED (offscreen) |
| Pushes sent by the daemon while idle | arrive in `connection-notify` with `ch="output"`, one per frame, about every 300 ms while the UI is idle | EXECUTED (offscreen) |
| Banner, eval right after start (no event-loop pump) | the first `connection-eval` returns the banner; every later reply is off by one | EXECUTED (offscreen). Reproduces the M3 offset. |
| Banner, after the event loop has run for about 0.5 s | the banner arrives through `connection-notify`; evals are not offset | EXECUTED (offscreen). The banner race is timing dependent. |
| Unsolicited frame sent before the reply to an eval | the eval returns **both** frames concatenated: `(document (concat "push..." "reply..."))` | EXECUTED (offscreen). Pushes that land during an eval corrupt the reply. |
| Unicode and `<>&"` in JSON | not run (scenario script bug, see below) | NOT EXECUTED |
| `command:` and `scheme:` frames | not run | NOT EXECUTED |
| `connection-write-string` (fire and forget) | not run | NOT EXECUTED |
| Blocking: heartbeat timer while a 2 s `slow` eval runs | not run | NOT EXECUTED |
| `connection-notify` override | `(tm-define (connection-notify lan ses ch t) (former lan ses ch t) ...)` chains with `utils/plugins/plugin-eval.scm:191` and was observed working | EXECUTED (offscreen) |
| Channel names | `"output"` observed; `"prompt"` and status channels from source (plugin-eval.scm:106-111, 200) | source only |
| Clean shutdown | `connection-stop` and quit; daemon exit on stdin close not checked | NOT EXECUTED |

Notes:
- Framing per `doc/devel/interface/interface-pipes.en.tm` and `progs/utils/plugins/plugin-cmd.scm:39`: DATA_BEGIN (0x02) + `verbatim:` + text + DATA_END (0x05). `DATA_ESCAPE` is 0x1b. The daemon writes no newline after DATA_END.
- The docs say that output must be one outer block per reply, and that a later block may be read before earlier output is consumed. The merge seen above is consistent with this.
- Blocking: `connection-eval` blocks the UI until a frame arrives. Not yet measured.

Engineering consequence so far: do not use `connection-eval` for replies that might race with pushes. Route every frame through `connection-notify` and match replies by request id. The daemon should send no banner, and the plugin should not depend on eval order. Re-check after the remaining cases run.

## Q1. Observing edits (NOT EXECUTED; source only)

- `buffer-notify` (`part/part-shared.scm:211`) is a `tm-define` called by the C++ notifier with `event` `'announce` and a modification `mod`. The C++ `buffer-attach-notifier` glue exists (`prog/glue-symbols.scm:726`, docs `glue-auto-doc.en.tm:5590`). It is lazy-loaded through `init-texmacs.scm:427`, and it is designed for shared parts, so using it means overriding a shared-part hook. The string `buffer-notify` appears in the binary; whether the C++ calls it for every edit is **NOT EXECUTED**.
- `notify-cursor-moved` (`utils/library/cursor.scm:141`) is a no-op hook. It is documented as called after any cursor movement, including the mouse. **NOT EXECUTED.**
- `keyboard-press` (`utils/misc/gui-utils.scm:357`, plus several others) fires per key. An override can delegate explicitly through `former` (`kernel/texmacs/tm-define.scm:230-250`), preserving the previous implementation. Compatibility with the user's uninspected patched tree remains unverified. Keystrokes do not cover menu commands, paste or undo.
- Plain `tm-define` redefinition replaces the master routine. Conditional definitions, including `:require` and `:mode`, may chain. The plugin should delegate through `former` rather than rely on condition ordering.
- Cost per keystroke: **NOT MEASURED.**

## Q2. Cursor tracking (NOT EXECUTED; source only)

- `(cursor-path)` is a glue primitive. `tree-innermost` (`utils/library/tree.scm:231`) walks up from the cursor and accepts a symbol, a list of symbols, or a predicate. A predicate on the mls carrier (`with` whose first child is `"mls-id"`) fits. `tree-go-to` and `tree->path` are at tree.scm:284-304.
- Detecting that the cursor left a block: compare the carrier found by `tree-innermost` with the previous one, on each idle tick. Polling is cheaper to get right than relying on `notify-cursor-moved`. **NOT EXECUTED.**

## Q5. Serialising the buffer in memory (NOT EXECUTED)

- `serialize-texmacs` (glue, `prog/glue-symbols.scm:470`, C++ `tree_to_texmacs`) is the candidate for producing canonical `.tm` text without saving. Not run yet. It must be checked against the M3 layout rule (blank lines after `<TeXmacs|…>` and before `<\body>`), and the result must be parsed with `mls FILE` (`crates/mls-cli/src/main.rs` `inspect`, no model calls, reads one `.tm` path, `--write-ids` only writes).
- Until it is run, the design assumes the plugin sends a path to a saved file, as M3 does.

## Q6. Re-rendering (NOT EXECUTED)

- The status panel is built by `mls-render-view` (`progs/mls-panel.scm`), which rebuilds the whole aux buffer. A per-block update would need `buffer-set-body` on the panel or a patch. No timing measured.

## Recommended direction (provisional)

1. Connection: start the daemon through `plugin-configure :launch`, with no banner. Each request carries an id. Replies and pushes arrive only via `connection-notify`, and the plugin matches them by id. Keep `connection-eval` out of the live path.
2. Edits: observe through a chained `keyboard-press` plus idle polling of a cheap fingerprint of the buffer. Use `buffer-notify` only after it is tested.
3. Timers: chained `delayed (:pause …)` with `:idle` for debounce. Do not use `:every` with a plain body.
4. Serialisation: use `serialize-texmacs` only if the round trip with `mls FILE` passes. Otherwise keep save-then-read (M3 protocol).
5. Panel: rebuild on each view, as M3 does, until measured.

## Remaining work (in order)

1. Fix the Q4 scenario script. The stage-2 `cond` in `scratchpad/m4-research-tm/scm/e4-scen.scm` lost a parenthesis when the `write` clause was patched, so the `slow`, `command_frame`, `scheme_frame`, `unicode` and `write` runs logged nothing. Rebuild it and rerun those cases.
2. Q1: attach a notifier to a loaded `.tm` copy, make `(insert ...)` edits, and count `buffer-notify` calls and `notify-cursor-moved` calls. Measure `change-time` after an edit.
3. Q2: `tree-innermost` with a carrier predicate on a fixture copy.
4. Q5: `serialize-texmacs` on the fixture, write it to a scratch `.tm` with the canonical layout, run `mls FILE` on it.
5. Q6: time one panel rebuild.
6. Final doc: at most 200 lines, a "Recommended M4 client design" section, and the GUI-only list (below).

## GUI-only, not verified

- Whether the real GUI (not Qt offscreen) delivers `connection-notify` pushes at the same cadence.
- Keyboard and menu hooks in the real GUI, shortcut conflicts, hover balloons, footer messages, and menu label refresh.
- Behaviour of the user's patched `~/.my-texmacs` tree (its `keyboard-press`, `notify-*` and `init-texmacs.scm` changes).
- Whether `idle-time` resets on mouse movement in the real GUI.
