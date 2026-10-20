;;; test-plugin.scm -- the plugin loads, its menu and commands exist, engine lookup,
;;; live permission defaults, replay directory preference, and shell command shape.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(mls-test-main
 (lambda ()
   (lazy-plugin-force)
   (mls-check "plugin is listed" (and (member 'mls (plugin-list)) #t))
   (mls-check "replay command defined" (procedure? mls-formalize-replay))
   (mls-check "live command defined" (procedure? mls-formalize-live))
   (mls-check "status command defined" (procedure? mls-show-status))
   (mls-check "MLS menu defined" (procedure? mls-menu))
   (let ((menu (object->string (mls-menu))))
     (mls-check "menu leads with one Start/Stop command" (procedure? mls-toggle))
     (mls-check-equal "start label before a session" "Start MLS" (mls-ui-start-label))
     (mls-check "menu shows the panel toggle" (mls-test-contains? menu "Show MLS panel"))
     (mls-check "menu offers settings and diagnostics"
                (mls-test-contains? menu "Settings and diagnostics"))
     (mls-check "menu offers the live and replay modes"
                (and (mls-test-contains? menu "Live model calls")
                     (mls-test-contains? menu "Replay recordings")))
     (mls-check "one-shot runs and the live permission sit under Advanced"
                (and (mls-test-contains? menu "Advanced")
                     (mls-test-contains? menu "Allow live model calls")
                     (mls-test-contains? menu "Formalize once from recordings")))
     (mls-check "the old ambiguous commands are gone"
                (and (not (mls-test-contains? menu "Open live status panel"))
                     (not (mls-test-contains? menu "Start live session")))))
   (mls-check-equal "engine comes from MLS_BIN" (mls-test-env "MLS_BIN") (mls-engine-path))
   (setenv "MLS_BIN" "")
   (mls-check-equal "engine falls back to mls on PATH" "mls" (mls-engine-path))
   (setenv "MLS_BIN" (mls-test-env "MLS_TEST_DIR"))
   (mls-check "live calls are not allowed by default" (not (mls-live-allowed?)))
   (mls-toggle-live)
   (mls-check "toggle allows live calls" (mls-live-allowed?))
   (mls-check "the permission is remembered as a preference"
              (equal? (get-preference "mls-live-consent") "on"))
   (mls-toggle-live)
   (mls-check "second toggle revokes live calls" (not (mls-live-allowed?)))
   (mls-check "live mode is the default" (mls-mode-live?))
   (mls-set-replay-dir! "/recordings/a b")
   (mls-check-equal "replay directory preference" "/recordings/a b" (mls-replay-dir))
   (mls-check "live shell command sets MLS_LIVE for the child only"
              (mls-test-contains? (mls-shell-command (list "review-run") #t) "MLS_LIVE=1 "))
   (mls-check "replay shell command unsets MLS_LIVE"
              (mls-test-contains? (mls-shell-command (list "review-run") #f) "unset MLS_LIVE; "))
   (mls-check "shell command quotes arguments"
              (mls-test-contains? (mls-shell-command (list "review-run" "a b") #f)
                                  "'a b'"))))
