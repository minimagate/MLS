# M3 TeXmacs plugin investigation

Investigated 2026-10-20 against TeXmacs 2.1.5 (`(version)` reports Guile 1.8.8). Every executed check ran headless (`-H`) with a scratch `TEXMACS_HOME_PATH`, a stub `mls` script, and no repository edits. Items marked **GUI-only** were not executed. Scratch scripts and the harness live under the session scratchpad (`m3-research/`), outside the repository.

Source paths are relative to `progs/` in the TeXmacs share tree. Line numbers refer to the bundle at `/Applications/TeXmacs.app/Contents/Resources/share/TeXmacs/`.

## 0. Findings that change the plan

1. **The running TeXmacs does not load the bundle's Scheme.** `TEXMACS_PATH` resolves to `~/.my-texmacs/TeXmacs` (`.my-texmacs-built-for` says 2.1.5). That tree is a patched copy of the bundle: `progs/init-texmacs.scm` appends loads of `my-texmacs-keys.scm`, `my-texmacs-page.scm` and `my-texmacs-symbol-search.scm` (lines 553-565); `texmacs/menus/main-menu.scm` adds an icon bar (276-281); `texmacs/texmacs/tm-view.scm:61` changes a default. The user's existing shortcuts are `M-backspace`, `A-backspace`, `M-delete`, `A-delete`, `M-1` and `M-k` (`my-texmacs-keys.scm:46-65`, `my-texmacs-symbol-search.scm:121`), where `M-` is Command and `A-` is Option. Check any plugin shortcut against this list.
2. **Silent data loss from non-canonical layout (blocker for save-after-load).** TeXmacs 2.1.5 needs blank lines in two places:
   - No blank line between `<style|generic>` and `<\body>`: the body imports empty, and a save writes only `<TeXmacs|2.1.5>`.
   - No blank line after `<TeXmacs|2.1.4>`: the `<style|…>` declaration is dropped on save.
   - A file with blank lines after both lines loads, saves and exports correctly.
   `tests/fixtures/tm/basic.tm` has the failing layout and loads as an empty document. Real TeXmacs files have the blank lines, so this only affects hand-made or M1-written fixtures. The plugin should refuse to save or reload when the imported body is empty but the file is not.
3. **Disclosure.** One early probe, `TeXmacs --help`, ran before `TEXMACS_HOME_PATH` was set and created `~/.TeXmacs/system/boot_lock` (0 bytes) and `~/.TeXmacs/system/tmp/90920/`, both born 2026-10-20 00:59:01. Nothing else under `~/.TeXmacs` or `~/.my-texmacs` changed. These two items were left in place; they should be removed by the user if no TeXmacs instance is starting.
4. **Headless quirks that shape the test harness.** An uncaught error under `-x` leaves the process waiting in the debugger, so every test must wrap its body in `catch` and run under an external timeout. Scheduled callbacks (`delayed`) never fired under `-x`. Plugins initialise lazily through `delayed (:idle 1000)` (`progs/init-texmacs.scm:519`, `kernel/texmacs/tm-plugins.scm:640-647`), so tests call `(lazy-plugin-force)` first. A stale `system/cache/plugin_cache.scm` hid a newly added plugin until it was deleted (`tm-plugins.scm:423-426` tracks only `$PATH` directories).

## 1. Plugin layout and loading

- Location: `$TEXMACS_HOME_PATH/plugins/<name>/progs/init-<name>.scm`. `TEXMACS_HOME_PATH` defaults to `$HOME/.TeXmacs` (verified with `HOME` overridden). `plugin-initialize` searches `$TEXMACS_HOME_PATH:$TEXMACS_PATH` (`tm-plugins.scm:621-638`).
- Discovery: `plugin-list` is a C++ primitive (`doc/devel/scheme/api/glue-auto-doc.en.tm:342`). Verified: `mls` appeared in it.
- `plugin-configure` (`tm-plugins.scm:595`, dispatch in 509-593). Used options: `:require expr`, `:launch "cmd"`, `:session "Name"`, `:scripts "Name"`, `:serializer fn`, `:winpath`/`:macpath`, `:initialize`. The `:initialize` body did not run in one stale-cache run and ran after a cache reset, so keep side effects at top level of the init file.
- Menus: `(menu-bind name items…)` expands to `(tm-define (name) (menu-dynamic …))` (`kernel/gui/menu-define.scm:515`). Plugins attach to the main menu through `plugin-menu`, which is linked at `texmacs/menus/main-menu.scm:59`: `(tm-menu (plugin-menu) (:require #t) (=> "MLS" (link mls-menu)))`. Other plugins use the same pattern, e.g. `plugins/python/progs/python-menus.scm`.
- Shortcuts: `(kbd-map (:mode in-text?) ("A-C-m" (mls-batch)))` (`kernel/gui/kbd-define.scm:244`). A non-string action is wrapped as a lambda (`kbd-map-one`, 224-231). **GUI-only:** conflict and display checks.
- Testing from a repository directory: `ln -s <repo>/plugins/mls $TEXMACS_HOME_PATH/plugins/mls`. Verified: `(member 'mls (plugin-list))` is true, `(lazy-plugin-force)` loads the init file, and a plugin function is callable.

## 2. Running external commands

Observed results of the Scheme primitives (scratch experiments e1-e3, e2):

| Call | Observed |
|---|---|
| `(system "cmd")` (`glue-symbols.scm`; Guile builtin) | Returns the exit status (`exit 3` gives 3). Child stdout goes to the TeXmacs process stdout, not a Scheme value. Blocks until exit. |
| `(eval-system "cmd")` (`glue` line 73) | Not reliable. `echo a` returned `"a\n"`. With exit 3 it returned `""` and the stdout leaked to the process. `echo e; echo f 1>&2` returned `"f\n"`, i.e. stderr. Do not use. |
| `(system-1 cmd url)` | Arity 2 exactly; not useful here. |
| `open-input-pipe` / `read-line` / `close-pipe` (`ice-9/popen.scm:205`, `:184`) | Requires `(use-modules (ice-9 popen))`. Read works. `close-pipe` returns the raw wait status (512 for exit 2). Blocking. |
| `string-quote` (glue 402) | Double quotes; does not protect `$` (verified). Use the sh-quote helper: `'` + text with `'` replaced by `'\''` + `'`. Verified with a path containing spaces and quotes. |
| `url-exists?`, `string-load`, `string-save` | Work with `$TEXMACS_HOME_PATH` URLs. |

Recommended pattern: `system` with redirected files, for example `mls batch … --output out.json > out.log 2> out.err`, then check the status and read `out.json` with `string-load`. Engine output never goes through eval (see §9).

## 3. Current buffer: path, save, reload, unsaved state

- `(current-buffer)` returns a URL; `(url->system (current-buffer))` gives the absolute path. `url-scratch?` is used for untitled buffers (`texmacs/texmacs/tm-files.scm`). Not separately tested.
- `(buffer-save url)` (glue 733) returns `#f` on success. It re-serialises the whole buffer in 2.1.5 layout (header becomes `2.1.5`, indentation changes). All four `mls-id` carriers survived. Saving under a different URL writes an empty document, so only save the buffer under its own URL.
- `(buffer-load url)` returns `#f` on success. It replaces the in-memory tree from disk silently and discards unsaved in-memory edits (verified). It also updates `buffer-last-save`, so a later user save does not prompt "changed on disk" (verified). The interactive `revert-buffer` (`tm-files.scm:581`) asks for confirmation when the buffer is modified; `revert-buffer-revert` (568) does not.
- Unsaved state: `buffer-modified?` stayed `#f` after `tree-set!` and after the `insert` command, although both edits were present in the tree. `buffer-pretend-modified` sets it. Headless, the flag is therefore not a reliable signal. Compare `(object->string (tree->stree (buffer-tree)))` with the snapshot taken at the last load or save instead.
- `url-last-modified` (glue 579) has one-second resolution. Two writes in the same second are indistinguishable (observed).

## 4. Tree access

- `(buffer-tree)` returns the body `(document …)`. Walk with `tree-compound?`, `tree-children`, `tree-label`, `tree-arity`, `tree-ref`, `tree->stree`. Every `mls-id` carrier is `(with "mls-id" UUID (document …))`; the walker found 4 of 4 in the fixture.
- Position: `tree-ip` (glue 206) gives the path, for example `(4 0 1)` for the theorem. `(tree-go-to node :start)` moves the cursor; `cursor-path` confirmed it.
- Mutation: `(tree-set! node child-index new-tree)` works, for example `(tree-set! thm 2 (tm->tree '(document …)))`. Edits are real: they appear in the saved file.

## 5. Visual markers without changing the saved document

No overlay or decoration primitive exists in 2.1.5. The glue list has only table decorations and balloons (`glue-symbols.scm`, grep for decor/highlight/overlay/mark).

| Approach | Survives `buffer-save` | LaTeX export (`-H -C in.tm out.tex`) | Visible in GUI | Verdict |
|---|---|---|---|---|
| Unused variable in carrier: `<with\|mls-id\|U\|mls-status\|error\|…>` | Yes (4 of 4 after save) | Byte-identical to baseline | No, by construction | Usable for stored state, not for display |
| `<with\|color\|red\|…>` in carrier | Yes (edits persist) | Changes: adds `xcolor` and a `\tmcolor` macro | Yes | Modifies the document; reject |
| `<hover-balloon\|…>` inside document body | Yes | Adds `\hoverballoon` calls | Hover tooltip (**GUI-only**) | Modifies the document; reject |
| Auxiliary buffer `tmfs://aux/mls-status` built with `buffer-set-body`, containing colored text and `hover-balloon` rows | Not part of the document | Export of the aux buffer shows the balloon text | Hover (**GUI-only**) | Recommended |
| `show-balloon` / `widget-balloon` (glue 698, 666) | n/a | n/a | Transient popup at a point (**GUI-only**) | Only as a supplement |

A temporary wrapper cannot be used, because in-memory edits persist on save. None of the tested mechanisms adds a per-block visual mark without changing the document or its export; a style macro was not tested and would change the exported LaTeX preamble.

Conclusion: there is no GUI-visible, export-neutral marker inside the document. Keep the document carrying only `mls-id`, and place markers in the auxiliary panel.

## 6. Status badge

- `(set-message left right)` (glue 847; used in `tm-files.scm:168`) is callable headless. The footer display is **GUI-only**.
- A menu title that shows counts depends on `menu-dynamic` re-evaluation when the menu opens. **GUI-only.**
- Recommended: `set-message` after each run, plus a counts header in the aux panel.

## 7. Review cards

- Widgets: `(tm-widget ((name args…) cmd) body)` with `form`, `form-input`, `form-enum`, `form-values`, `form-fields`, `bottom-buttons`, `aligned`/`item`. Examples: `doc/devel/scheme/gui/scheme-gui-forms.en.tm` and `scheme-gui-dialogs.en.tm`.
- Windows: `(dialogue-window menu-promise cmd "Title")` (`kernel/gui/menu-widget.scm:1137`); `top-window` (1125). A widget constructed through `(name args…)` returns a promise that `dialogue-window` accepts. Passing a plain lambda as the promise errors ("menu has bad format"). Headless, `dialogue-window` returned immediately.
- Callbacks: each button calls `(cmd value)`. The block UUID is captured in the closure, for example `(bottom-buttons ("Confirm" (cmd (list "confirm" uuid (form-values)))))`. Button firing and form rendering are **GUI-only**. Keep the logic in a plain procedure, `mls-review-action uuid action fields`, which was executed headless against the stub.
- An auxiliary "MLS review" document with `action` links was not tested.

## 8. Headless testing harness (executed)

Working command, used for all tests below:

```sh
perl -e 'alarm shift; exec @ARGV' 60 env TEXMACS_HOME_PATH="$RUN/home" \
  /Applications/TeXmacs.app/Contents/MacOS/TeXmacs -H -s \
  -x "(load \"$T/m3-test.scm\")" -q < /dev/null > out.txt 2> err.txt
```

- `-H` headless, `-s` suppresses messages, `-x` evaluates then exits with `-q`. Filter `approximating font` noise from stdout.
- The test file calls `(lazy-plugin-force)`, runs assertions through a `check` helper that prints `PASS`/`FAIL`, and wraps everything in `catch`.
- Stub engine: a POSIX shell script that mimics `mls batch … --output F` and `mls review ID --action A --output F`. It writes canned JSON, logs argv to `MLS_STUB_LOG`, and exits with `MLS_STUB_EXIT`.
- Each run uses a fresh `TEXMACS_HOME_PATH` with the plugin symlinked in, and a copy of the fixture.
- Result: 14 of 14 checks passed. They cover plugin declaration, carrier count, JSON parse with nested SIR and arrays, badge text, go-to of a known and an unknown block, the review call reaching the stub, a non-zero engine exit raising an error, `buffer-save` keeping 4 identities, and an external rewrite reloaded with `buffer-load`.

## 9. JSON and parsing of engine output

- TeXmacs Scheme has no JSON reader. `string->json` is unbound, and "json" appears only as a language name in `text-drd.scm` and `tm-modes.scm`.
- `string->object` / `read` (Guile 1.8.8): `#.` read-eval is disabled and raises an error. Trailing data is silently ignored (`"1 2"` reads as `1`). Strings are bytes, not characters (`"é∀"` has length 5).
- A pure-Scheme JSON reader of about 95 lines (objects to alists, arrays to vectors, `\uXXXX` to UTF-8 bytes, errors on trailing data) parsed the stub output correctly, including nested SIRs. It executes no engine text.
- Recommendation: the engine emits JSON; the plugin parses it with a pure reader and never evaluates it.

## 10. Persistent pipe session, for M4

- API: `connection-start`, `connection-status`, `connection-eval`, `connection-write-string`, `connection-write`, `connection-stop` (glue 645-652; docs `glue-auto-doc.en.tm:4942-4990`). Plugins declare `(:launch "cmd")` and `(:session "Name")`.
- Framing (`doc/devel/interface/interface-pipes.en.tm`): output blocks start with byte 2 (`DATA_BEGIN`) and end with byte 5 (`DATA_END`), with a `verbatim:` prefix for plain text.
- Executed with a stub daemon: `connection-start` returned `ok`. `connection-eval` returned the next DATA frame synchronously, but the first call returned the daemon's startup banner, so the frame stream is offset by one. The daemon must send a banner, or the plugin must drain one frame.
- Asynchronous output goes to `connection-notify` (`utils/plugins/plugin-eval.scm:191`). It needs the event loop, which is **GUI-only**.
- `plugin-async` does not exist in 2.1.5 (no definition in `progs/` or `plugins/`).
- Blocking: `connection-eval` waits for `DATA_END`, so a slow engine freezes the UI.

## Recommended M3 design

- **Engine invocation (batch, no daemon):** `mls batch FILE --output OUT.json` with `--replay DIR` or `--live`, the M2 CLI. Add `mls review FILE --block UUID --action confirm|reject|edit --fields FIELDS.json --output OUT.json`. The plugin runs commands through `system` with sh-quoted arguments and redirects stdout and stderr to files; it checks the status and shows failures in the footer.
- **Save and reload protocol:** (1) always `buffer-save` before an engine command, then read the file back with `tree-import` and require its body to equal the buffer with mls-id carriers stripped; on a mismatch put the previous file text back and stop before the engine runs; (2) run the engine; (3) if the status is 0 and the file changed, `buffer-load`; (4) guard against empty imports (§0.2) before saving or reloading.
- **Data format:** JSON. Parse with the pure reader in §9. Never `read` or evaluate engine output.
- **Document:** only `<with|mls-id|UUID|…>` carriers. Statuses and SIRs stay in the engine (spec §3.2). Verified that the carrier survives save and export.
- **Markers and status panel:** an auxiliary buffer `tmfs://aux/mls-status`, created with `buffer-set-body`. Each block is a row with a colored dot, a hover-balloon error message, and a row action that calls `tree-go-to` on the carrier.
- **Badge:** `set-message` in the footer after each run, plus a counts header in the panel.
- **Review cards:** `dialogue-window` with a `form` per block. Confirm, Reject and Edit call `mls-review-action` with the block UUID captured in the closure. Then reload from the engine output.
- **Menu and shortcut:** `(tm-menu (plugin-menu) …)` and a `kbd-map` entry. Pick a chord not in the user's list in §0.1, and check conflicts in the GUI.
- **Async:** M3 may block for replay runs. For live runs, use `system "… &"` with polling, which needs a GUI test because scheduled callbacks did not fire headless.
- **Tests:** the harness in §8 is reusable; it should move under `tests/` only when M3 is implemented.

## Unverified (GUI-only)

Hover-balloon rendering, dialogue rendering and button callbacks, the footer message, menu label refresh, shortcut conflicts, `show-balloon` markers, `tree-bounding-rectangle`, async polling through `delayed`, and whether `set-message` repaints before a blocking `system` call.

## Blockers and risks

- No GUI-visible, export-neutral marker inside the document. The panel design avoids the problem but changes the spec's "marker in the block" wording.
- Non-canonical `.tm` layout silently drops the body or the style on save (§0.2). Guard before save and reload, and keep the M1 fixtures canonical.
- A blocking `system` call freezes the UI for the length of a live run.
- Shortcut and dialogue behaviour are unverified until someone runs the GUI build.
