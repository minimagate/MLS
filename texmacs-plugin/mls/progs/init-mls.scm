;;; init-mls.scm -- TeXmacs plugin entry point for MLS (thin client).
;;;
;;; Loads the modules in plugins/mls/progs, then declares the plugin, the MLS menu
;;; and the keyboard shortcut. The engine owns all state; the plugin only runs
;;; `mls review-*` commands and renders their JSON.
;;;
;;; Preferences: mls-engine (engine path, empty for MLS_BIN or PATH) and
;;; mls-replay-dir (last recordings directory). The live toggle is session-only.
;;;
;;; Shortcut: Option+Control+M (A-C-m) runs "Formalize document (replay...)".
;;; Chosen to avoid the chords already in use: M-backspace, A-backspace,
;;; M-delete, A-delete, M-1 and M-k (see docs/research/m3-texmacs.md, section 0, item 1).

(define-preferences
  ("mls-engine" "" noop)
  ("mls-replay-dir" "" noop))

(define (mls-load-module name)
  (let ((u (url-unix "$TEXMACS_HOME_PATH:$TEXMACS_PATH"
                     (string-append "plugins/mls/progs/" name ".scm"))))
    (if (url-exists? u)
        (load (url-materialize u "r"))
        (error "mls: missing module" name))))

(for-each mls-load-module
          '("mls-text" "mls-json" "mls-engine" "mls-document"
            "mls-panel" "mls-review" "mls-badges" "mls-formalize" "mls-live"))

(menu-bind mls-menu
  ("Formalize document (replay...)" (mls-formalize-replay))
  ("Formalize document (live)" (mls-formalize-live))
  ("Start live session" (mls-live-start))
  ("Stop live session" (mls-live-stop))
  ("Open live status panel" (mls-live-show-panel))
  ("Show status" (mls-show-status))
  ---
  ((check "Allow live model calls" "v" (mls-live-allowed?)) (mls-toggle-live)))

(tm-menu (plugin-menu) (:require #t)
  (=> "MLS" (link mls-menu)))

(kbd-map
  ("A-C-m" (mls-formalize-replay)))
