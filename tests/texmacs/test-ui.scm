;;; test-ui.scm -- the Start command and its setup checks, the panel's problem card,
;;; error banner and diagnostics, engine stderr diagnosis, the focus toolbar target, and
;;; the live/replay launcher. Starting a real session is covered by live-integration.scm.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(define (ui-panel) (object->string (mls-panel-tree (mls-panel-active-view))))

(define (ui-fresh name)
  (let ((path (mls-test-doc name)))
    (string-save (mls-test-file-text (mls-test-fixture "badges.tm")) path)
    (load-buffer (string->url path))
    (switch-to-buffer (string->url path))
    (string->url path)))

(define (ui-problem-title)
  (and mls-ui-problem (car mls-ui-problem)))

(mls-test-main
 (lambda ()
   (define work (mls-test-env "MLS_TEST_WORK"))
   (lazy-plugin-force)

   ;; Every click shows a result: the panel says what is happening before any work.
   (mls-check-equal "the phase starts idle" 'idle mls-ui-phase)
   (mls-check "an idle panel invites a start"
              (mls-test-contains? (ui-panel) "Not running"))

   ;; Setup checks, in order, each with a recovery action.
   (new-buffer)
   (mls-check "start on an unsaved document is refused" (not (mls-start)))
   (mls-check-equal "the problem says to save" "Save this document first" (ui-problem-title))
   (mls-check "the panel shows the problem and a Save action"
              (and (mls-test-contains? (ui-panel) "Save this document first")
                   (mls-test-contains? (ui-panel) "(save-buffer)")))
   (mls-check-equal "the phase is error" 'error mls-ui-phase)

   (let ((url (ui-fresh "ui.tm")))
     (setenv "MLS_BIN" "/nonexistent/mls-engine")
     (mls-check "start without an engine is refused" (not (mls-start)))
     (mls-check-equal "the problem names the engine" "MLS engine not found" (ui-problem-title))
     (mls-check "the panel offers to set the engine path"
                (mls-test-contains? (ui-panel) "(mls-ui-choose-engine)"))
     (setenv "MLS_BIN" (string-append (mls-test-env "MLS_TEST_DIR") "/stub/mls"))

     (mls-check "start in live mode without permission is refused" (not (mls-start)))
     (mls-check-equal "the problem asks for live permission" "Allow live model calls?"
                      (ui-problem-title))
     (mls-check "the permission card explains cost and offers both choices"
                (let ((panel (ui-panel)))
                  (and (mls-test-contains? panel "may cost money")
                       (mls-test-contains? panel "(mls-ui-allow-live)")
                       (mls-test-contains? panel "(mls-ui-choose-replay)"))))
     (mls-check "no engine process was asked to start" (not mls-live-connected?))

     (set-preference "mls-mode" "replay")
     (mls-set-replay-dir! "relative/dir")
     (mls-check "replay mode with a bad folder is refused" (not (mls-start)))
     (mls-check-equal "the problem asks for a folder" "Choose a recordings folder"
                      (ui-problem-title))
     (mls-set-replay-dir! work)
     (mls-check "replay mode with a folder passes the setup checks" (not (mls-ui-preflight)))
     (mls-check "the replay launch environment carries the folder"
                (begin (mls-live-set-launch-env!)
                       (equal? (getenv "MLS_SERVE_REPLAY") work)))
     (set-preference "mls-mode" "live")
     (mls-live-set-launch-env!)
     (mls-check-equal "the live launch environment has no folder" "" (getenv "MLS_SERVE_REPLAY"))

     ;; The launcher runs the engine in the chosen mode, MLS_LIVE only for live.
     (let ((probe (string-append work "/serve-probe"))
           (out (string-append work "/serve-probe.out")))
       (string-save "#!/bin/sh\necho \"LIVE=${MLS_LIVE:-unset} $*\"\n" probe)
       (system (string-append "chmod +x " (mls-sh-quote probe)))
       (setenv "MLS_SERVE_BIN" probe)
       (setenv "MLS_SERVE_REPLAY" "")
       (system (string-append (mls-live-launcher) " > " (mls-sh-quote out)))
       (mls-check-equal "live launch" "LIVE=1 serve --live\n" (string-load out))
       (setenv "MLS_LIVE" "1")
       (setenv "MLS_SERVE_REPLAY" "/rec/a b")
       (system (string-append (mls-live-launcher) " > " (mls-sh-quote out)))
       (mls-check-equal "replay launch keeps the folder whole and drops MLS_LIVE"
                        "LIVE=unset serve --replay /rec/a b\n" (string-load out))
       (unsetenv "MLS_LIVE"))

     ;; Errors stay in the panel, not only in the footer.
     (set! mls-ui-phase 'starting)
     (mls-fail "the engine exited with status 2")
     (mls-check-equal "a failure while starting sets the error phase" 'error mls-ui-phase)
     (mls-check "the error banner shows the message and a dismiss action"
                (let ((panel (ui-panel)))
                  (and (mls-test-contains? panel "the engine exited with status 2")
                       (mls-test-contains? panel "(mls-ui-dismiss-error)"))))
     (mls-ui-dismiss-error)
     (mls-check "dismissing removes the banner"
                (not (mls-test-contains? (ui-panel) "the engine exited with status 2")))

     ;; Engine stderr is turned into an explanation.
     (mls-check-equal "a missing API key is recognised" "Model API key missing"
                      (car (mls-ui-stderr-problem "Error: OPENROUTER_API_KEY is not set")))
     (mls-check-equal "a Lean failure is recognised" "Lean toolchain problem"
                      (car (mls-ui-stderr-problem "lake: command not found")))
     (mls-check "unknown stderr is not guessed at" (not (mls-ui-stderr-problem "segfault")))
     (set! mls-ui-stderr "")
     (mls-ui-append-stderr! "line one\nError: OPENROUTER_API_KEY is not set\n")
     (mls-ui-engine-failed! "The MLS engine stopped")
     (mls-check-equal "a dead engine is explained from its stderr" "Model API key missing"
                      (ui-problem-title))
     (mls-check "the engine's own words are shown"
                (mls-test-contains? (ui-panel) "OPENROUTER_API_KEY is not set"))

     ;; Diagnostics.
     (set! mls-ui-diagnostics? #f)
     (mls-ui-diagnostics)
     (mls-check "settings and diagnostics list the checks"
                (let ((panel (ui-panel)))
                  (and (mls-test-contains? panel "Settings and diagnostics")
                       (mls-test-contains? panel "Engine")
                       (mls-test-contains? panel "live calls not allowed yet"))))
     (mls-ui-diagnostics)

     ;; Snapshot versus live is always stated.
     (set! mls-current-view (mls-json-parse (mls-test-file-text (mls-test-views "badges.json"))))
     (set! mls-current-document url)
     (mls-render-view mls-current-view)
     (mls-check "a one-shot view is labelled as a snapshot that is not updating"
                (and (mls-test-contains? (ui-panel) "Snapshot")
                     (mls-test-contains? (ui-panel) "not updating")))
     (mls-ui-set-phase! 'live "Watching your edits")
     (mls-check "a live session is labelled live"
                (and (mls-test-contains? (ui-panel) "\"Live\"")
                     (mls-test-contains? (ui-panel) "Stop")))
     (mls-check-equal "the menu offers Stop while running" "Stop MLS" (mls-ui-start-label))
     (set! mls-ui-phase 'stopped)

     ;; The focus toolbar targets the MLS block under the cursor.
     (let* ((carriers (mls-carriers))
            (verified (assoc "e688a3ab-4e5d-4b7e-9c57-c857a7e07741" carriers))
            (section (assoc "9f0c1a2e-3b4d-4c5e-8f60-718293a4b5c6" carriers)))
       (mls-check-equal "a carrier is an MLS focus target"
                        "e688a3ab-4e5d-4b7e-9c57-c857a7e07741"
                        (mls-ui-focus-id (cdr verified)))
       (let ((theorem (tree-ref (tree-ref (cdr verified) 2) 0)))
         (mls-check-equal "a theorem inside a carrier is an MLS focus target"
                          "e688a3ab-4e5d-4b7e-9c57-c857a7e07741"
                          (mls-ui-focus-id theorem)))
       (mls-check "a section's content is not a focus target"
                  (not (mls-ui-focus-id (tree-ref (tree-ref (cdr section) 2) 0))))
       (mls-check-equal "the focus summary names both statuses"
                        "MLS: statement, proof verified"
                        (mls-ui-focus-summary "e688a3ab-4e5d-4b7e-9c57-c857a7e07741"))
       (mls-check "the focus toolbar offers Review and the panel"
                  (let ((icons (object->string (focus-extra-icons (cdr verified)))))
                    (and (mls-test-contains? icons "Review")
                         (mls-test-contains? icons "MLS panel"))))
       (mls-check "an error block offers Retry in the focus toolbar"
                  (mls-test-contains?
                   (object->string (focus-extra-icons
                                    (cdr (assoc "20126ddc-bfe5-4ff3-a604-a8cf7a75f411" carriers))))
                   "Retry"))))))
