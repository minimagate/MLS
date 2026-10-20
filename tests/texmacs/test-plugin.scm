;;; test-plugin.scm -- the plugin loads, its menu and commands exist, engine lookup,
;;; live toggle defaults, replay directory preference, and shell command shape.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(mls-test-main
 (lambda ()
   (lazy-plugin-force)
   (mls-check "plugin is listed" (and (member 'mls (plugin-list)) #t))
   (mls-check "replay command defined" (procedure? mls-formalize-replay))
   (mls-check "live command defined" (procedure? mls-formalize-live))
   (mls-check "status command defined" (procedure? mls-show-status))
   (mls-check "MLS menu defined" (procedure? mls-menu))
   (mls-check "menu lists the live toggle"
              (mls-test-contains? (object->string (mls-menu)) "Allow live model calls"))
   (mls-check "menu lists the replay command"
              (mls-test-contains? (object->string (mls-menu)) "Formalize document (replay"))
   (mls-check-equal "engine comes from MLS_BIN" (mls-test-env "MLS_BIN") (mls-engine-path))
   (setenv "MLS_BIN" "")
   (mls-check-equal "engine falls back to mls on PATH" "mls" (mls-engine-path))
   (setenv "MLS_BIN" (mls-test-env "MLS_TEST_DIR"))
   (mls-check "live toggle is off by default" (not (mls-live-allowed?)))
   (mls-toggle-live)
   (mls-check "toggle turns live on" (mls-live-allowed?))
   (mls-toggle-live)
   (mls-check "second toggle turns live off" (not (mls-live-allowed?)))
   (mls-set-replay-dir! "/recordings/a b")
   (mls-check-equal "replay directory preference" "/recordings/a b" (mls-replay-dir))
   (mls-check "live shell command sets MLS_LIVE for the child only"
              (mls-test-contains? (mls-shell-command (list "review-run") #t) "MLS_LIVE=1 "))
   (mls-check "replay shell command unsets MLS_LIVE"
              (mls-test-contains? (mls-shell-command (list "review-run") #f) "unset MLS_LIVE; "))
   (mls-check "shell command quotes arguments"
              (mls-test-contains? (mls-shell-command (list "review-run" "a b") #f)
                                  "'a b'"))))
