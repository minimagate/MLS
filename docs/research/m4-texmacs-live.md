# M4 TeXmacs live mode: completed stock-bundle investigation

Scope: GNU TeXmacs **2.1.5**, 2026-10-20, stock application bundle at
`/Applications/TeXmacs.app/Contents/Resources/share/TeXmacs/`.
All runtime findings below use its Qt **offscreen** event loop, except the explicitly
identified earlier headless result. They establish Scheme/protocol behavior, not real GUI UX.
The owner's patched `~/.my-texmacs`, `~/.TeXmacs`, and visible GUI were not accessed or changed.

## Harness and reproducibility

New scratch harness and retained evidence: `/private/tmp/mls-tm-live-j0qe53am/` (`LAB` below).
Each `run.py` invocation creates a fresh `runs/<case>-*/userhome`, `texhome`, and document copy;
sets `HOME`, `TEXMACS_HOME_PATH`, and stock `TEXMACS_PATH`; and unsets live-model credentials.
Every TeXmacs launch is bounded by `perl -e 'alarm 45; exec @ARGV'`, with
`QT_QPA_PLATFORM=offscreen`, `-s -x '(load "…/scm/<probe>.scm")'`, and stdin closed.
The controlled Python daemon also has its own 25-second alarm. No existing process was stopped.
Successful runs exited 0; an initial sandbox launch aborted during initialization, and one
scratch Q1 syntax error expired under its alarm before the corrected probes succeeded.
Offscreen execution required sandbox escalation on this host.

Commands actually used, with every invocation creating another isolated profile:

```sh
LAB=/private/tmp/mls-tm-live-j0qe53am
python3 "$LAB/run.py" slow q4.scm
python3 "$LAB/run.py" command_frame q4.scm
python3 "$LAB/run.py" scheme_frame q4.scm
python3 "$LAB/run.py" unicode q4.scm
python3 "$LAB/run.py" unicode_ascii q4.scm
python3 "$LAB/run.py" write q4.scm
python3 "$LAB/run.py" q1-buffer q1.scm
python3 "$LAB/run.py" q2-inclusive q2.scm
python3 "$LAB/run.py" q5 q5.scm
python3 "$LAB/run.py" q6 q6.scm
target/debug/mls "$LAB/runs/q5-b7kyu895/serialized.tm"
```

The final command is read-only CLI inspection, with no models or `--write-ids`.
Each run retains `command.json`, `probe.log`, `stdout.log`, and `stderr.log`;
connection runs also retain `daemon.log`. Q5 retains `serialized.tm` and `inspect.json`.
Earlier checkpoint logs were also checked: `c1.zyEMJu/e3.log`, `s-bnow.Dc6Nbb/e4.log`,
and `s-ptr.eD5TYc/e4.log` under the previous scratch harness's `runs/` directory:
`/private/tmp/claude-501/-Users-michelangelogubinelli-Github-MLS/0ae94e67-b04f-4014-afda-3be0edc1be8e/scratchpad/m4-research-tm/`.

## Q4. Persistent connection and framing — executed first

The broken nested scenario `cond` was replaced with a small dispatch function.
A temporary `mlsprobe` plugin registers `:launch`; `connection-start` returns `"ok"`.
Frames are DATA_BEGIN (0x02), `verbatim:`, payload, DATA_END (0x05), without a trailing newline.
The daemon accepts newline-delimited JSON requests. No startup banner is used in the new probes.

| Probe | Observed result |
|---|---|
| `slow-atz653to` | A 2-second `connection-eval` took **2005 ms**; the 50-ms heartbeat made **0** ticks during it. |
| `write-2yy2oe5x` | `connection-write-string` returned unspecified; its reply arrived asynchronously through `connection-notify`, channel `"output"`. |
| `command_frame-7sr1w56j` | A controlled `command:` frame **executed** the registered Scheme callback. Eval returned empty; the following JSON reply arrived through notify. |
| `scheme_frame-_8hhbzaz` | `scheme:` produced `(document "scheme-hello")`, rather than JSON text. |
| `unicode-dwbntl8y` | Literal UTF-8 became TeXmacs internal text: `é` became byte 0xe9, `∀` became `<forall>`, and angle brackets became `<less>`/`<gtr>`. It was not a UTF-8 JSON byte round trip. |
| `unicode_ascii-rfx2oxdj` | ASCII JSON containing `\u00e9\u2200`, `<tag>`, `&`, and escaped quotes survived intact. |

The raw-Unicode TeXmacs run exited 0; the first Python log printer failed to decode its
Cork bytes as UTF-8. Reading the retained bytes with escaped invalid sequences confirmed the conversion.
The existing MLS JSON parser already decodes `\u` escapes, including surrogate pairs.

Earlier executed observations were confirmed in the retained checkpoint logs:
- An immediate eval consumed a startup banner, leaving replies one request behind.
- A push arriving during eval merged with its reply into one `(document (concat …))`.
- Idle pushes reached `connection-notify` about every 300 ms.

Therefore eval is unsuitable for the live path: it blocks timers and has no reliable
request/reply separation when unsolicited output exists. Notify trees also need structural
text extraction, not Scheme evaluation or blind conversion with `object->string`.
`command:` handling occurs before the JSON notify handler: a notify-only parser is **not**
a sandbox against executable frame types. The trusted daemon/transport must emit only `verbatim:` JSON.
`connection-stop` was called before quitting; a read-only check of all six probe daemon
PIDs showed none remained. A graceful stdin-EOF handshake was not established by these runs.

## Q1. Observing edits — executed

Evidence: `q1-buffer-xnlgt5os/probe.log`.
After loading `(part part-shared)`, overriding `buffer-notify` with a call to `former`,
and attaching `buffer-attach-notifier` to the fixture URL:
- Text insertion emitted six `announce`/`done` modification pairs.
- Two root `tree-insert` operations emitted one pair each; a later text insertion emitted six pairs.
- Total: **14 announce + 14 done** callbacks. Reattaching did not duplicate the observed events.
- The chained `notify-cursor-moved` hook ran **46** times across startup, editing,
  normalization, and cursor moves: neither hook is a one-callback-per-keystroke interface.

A startup-created temporary buffer could become active between delayed callbacks.
Selecting the fixture explicitly before an edit resolved an initially misleading absence of notifications.
Production callbacks must retain/check the document URL instead of assuming the active buffer is unchanged.
`change-time` did not change synchronously with the insert, then advanced on later event-loop turns;
use it as a coarse UI hint, not a per-block revision. Similarly, Q5 saw `buffer-modified?` still
false immediately after an in-memory insert. Maintain a client dirty/revision counter instead.
The probe logs every event for evidence; production hooks should only mark dirty and return.
Keyboard/menu/paste/undo coverage and real keystroke overhead remain GUI checks.

## Q2. Cursor tracking — executed, including a boundary correction

Evidence: `q2-inclusive-tj4pceyf/probe.log`; source: `utils/library/tree.scm:231`.
Use a predicate matching `(with "mls-id" UUID BODY)` with **`(tree-innermost carrier? #t)`**.
The optional argument includes the cursor's enclosing node: the default drops an extra path component
and missed the carrier at its wrapper start/end in this probe.
The inclusive form found carrier A at both boundaries and inside its text, found carrier B
inside nested math, and returned `#f` in the intervening/outside paragraphs.
Comparing the previous UUID with the next result detected both A→outside and B→outside.
Poll this ownership on a short idle tick; cursor notifications can merely request an earlier poll.

## Q3. Timers and idle — earlier execution retained

Source: `kernel/texmacs/tm-dialogue.scm:40–113`.
`delayed` wraps a plain body with a final `#t`; a plain `(:every N)` body consequently ran once.
A body that explicitly schedules the next `delayed (:pause N)` reliably repeated
(earlier evidence: 12 ticks at approximately 100 ms). `(:idle 300)` also fired while quiet.
`idle-time` grew during quiet periods; scripted edits/moves reset it in the new probes.
Headless `-H` exited when `-x` returned and did not service these timers; use the offscreen Qt loop
for timer/connection integration tests. Physical mouse behavior remains unverified.

## Q5. In-memory serialization — executed and parsed by MLS

Evidence: `q5-b7kyu895/{probe.log,serialized.tm,inspect.json}`.
`(serialize-texmacs (buffer-get URL))` serialized the **whole buffer**, including its header/style,
in **1 ms** for this fixture (617 characters). It already supplied blank lines after `<TeXmacs|…>`
and before `<\body>`; no manual separator repair was necessary.
An `UNSAVED-PROBE` insertion appeared in the serialized snapshot and not in the disk copy;
byte comparison confirmed that the original fixture copy stayed unchanged.
`target/debug/mls serialized.tm` returned **5 blocks, 0 diagnostics**: all four existing UUIDs
survived, plus the new uncarried commentary. This establishes a path to unsaved-buffer snapshots;
new blocks still need stable carrier assignment before relying on their identities across revisions.

## Q6. Panel rebuilding — executed, synchronous cost only

Evidence: `q6-t7big36h/probe.log`, using the existing `mls-render-view` implementation.
One sample each: **4 rows: 24 ms; 100 rows: 27 ms; 1000 rows: 80 ms**.
The resulting panel bodies had 6, 102, and 1002 children respectively (two heading/badge rows).
These timings include Scheme tree construction, buffer replacement, and switching; they exclude
subsequent Qt painting and are not a benchmark distribution. Coalesce view updates; do not rebuild
on every edit or cursor notification. Whole-panel replacement remains a reasonable initial implementation.

## Recommended M4 client design

1. Register a persistent plugin connection with no banner. Use `connection-write-string` for
   newline-delimited, versioned requests; attach document UUID, client revision, and request ID.
2. Consume replies/pushes exclusively through a chained `connection-notify` handler. Accept only
   the expected document/concat/text structure, recover ASCII JSON, and validate its schema/identity.
   Keep raw UTF-8, `command:`, and `scheme:` out of daemon output; never evaluate JSON-derived text.
3. Chain `buffer-notify`, `keyboard-press`, and `notify-cursor-moved` with `former`; attach a notifier
   per document. Mark dirty and increment a client revision cheaply, then debounce/coalesce on a chained timer. Suppress feedback
   from the client's own status-buffer writes and verify the target buffer on every delayed callback.
4. Derive active block with inclusive `tree-innermost`; cursor departure plus idle debounce gates work.
   Serialize the whole unsaved buffer when needed, preserve/assign carriers, and reject stale replies.
5. Rebuild the panel only when a new accepted view arrives. Keep the current editing buffer/cursor
   owned by the user; M3's `mls-render-view` switches buffers, so live rendering must preserve focus.
6. Define explicit shutdown/cancellation/reconnect and bounded queues. Do not infer graceful daemon
   cleanup from `connection-stop`; add a tested shutdown acknowledgement before relying on it.

## GUI-only checks, not verified

- Actual keyboard, paste, undo/redo, menu and mouse edit coverage; real keystroke latency and shortcut conflicts.
- Mouse effects on `idle-time`, focus changes, cursor movement and block departure while typing.
- Real-window push cadence, redraw/scroll latency, hover balloons, footer messages and menu-label refresh.
- Focus/cursor/selection preservation during panel updates and interactions with other installed plugins.
- Review/edit-dialog rendering, text entry, and actual confirm/reject/edit button callbacks.

Other limits: the owner's patched tree and hook compatibility remain intentionally uninspected.
Graceful EOF/shutdown, daemon-death recovery, and large-document soak/backpressure tests could
be performed offscreen but were not established by this investigation.
