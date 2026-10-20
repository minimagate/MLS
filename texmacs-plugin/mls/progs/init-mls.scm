;;; init-mls.scm -- TeXmacs plugin entry point for MLS (thin client).
;;;
;;; Loads the modules in plugins/mls/progs, then declares the plugin, the MLS menu
;;; and the keyboard shortcut. The engine owns all state; the plugin only runs
;;; `mls review-*` commands and renders their JSON.
;;;
;;; Preferences: mls-engine (engine path, empty for MLS_BIN or PATH), mls-replay-dir
;;; (recordings directory), mls-mode (live or replay) and mls-live-consent (whether the
;;; user allowed live model calls).
;;;
;;; The menu has one main command, Start/Stop MLS; everything else lives in the MLS
;;; panel (mls-ui.scm). One-shot runs stay under Advanced.
;;;
;;; Shortcut: Option+Control+M (A-C-m) starts or stops MLS. Chosen to avoid the chords
;;; already in use: M-backspace, A-backspace, M-delete, A-delete, M-1 and M-k (see
;;; docs/research/m3-texmacs.md, section 0, item 1).

(define-preferences
  ("mls-engine" "" noop)
  ("mls-replay-dir" "" noop)
  ("mls-mode" "live" noop)
  ("mls-live-consent" "off" noop))

(define (mls-load-module name)
  (let ((u (url-unix "$TEXMACS_HOME_PATH:$TEXMACS_PATH"
                     (string-append "plugins/mls/progs/" name ".scm"))))
    (if (url-exists? u)
        (load (url-materialize u "r"))
        (error "mls: missing module" name))))

(for-each mls-load-module
          '("mls-text" "mls-json" "mls-engine" "mls-document"
            "mls-panel" "mls-review" "mls-badges" "mls-formalize" "mls-live" "mls-ui"))

(menu-bind mls-menu
  ((eval (mls-ui-start-label)) (mls-toggle))
  ((check "Show MLS panel" "v" mls-ui-panel-open?) (mls-ui-toggle-panel))
  ---
  (-> "Mode"
      ((check "Live model calls" "*" (mls-mode-live?)) (mls-ui-use-live))
      ((check "Replay recordings..." "*" (mls-mode-replay?)) (mls-ui-choose-replay)))
  ("Settings and diagnostics" (mls-ui-diagnostics))
  ---
  (-> "Advanced"
      ((check "Allow live model calls" "v" (mls-live-allowed?)) (mls-toggle-live))
      ("Status snapshot" (mls-show-status))
      ("Formalize once from recordings..." (mls-formalize-replay))
      ("Formalize once with live calls" (mls-formalize-live))))

(tm-menu (plugin-menu) (:require #t)
  (=> "MLS" (link mls-menu)))

(kbd-map
  ("A-C-m" (mls-toggle)))
