;;; mls-ui.scm -- what the user sees and clicks: one Start/Stop command, the MLS side
;;; panel next to the document, the focus toolbar for the block under the cursor, setup
;;; checks with recovery actions, and the menu.
;;;
;;; The session phase is one of: idle (never started), starting, live, stopped,
;;; snapshot (a one-shot status, not updating) and error. Every command sets a phase
;;; before it does any work and refreshes the panel, so a click always shows a result.
;;; Errors are kept in the panel (mls-ui-error), not only in the footer, so they stay
;;; visible when the status bar is hidden.
;;;
;;; Panel and toolbar actions are fixed command strings; the only engine-derived
;;; values they carry are view indices or UUIDs checked by mls-badge-uuid?.

;; Load TeXmacs' own focus menus first, so the MLS overloads below are defined after
;; them and win for MLS blocks.
(use-modules (generic generic-menu) (text text-menu))

(define mls-ui-phase 'idle)
(define mls-ui-note "")
(define mls-ui-problem #f)        ; (title detail ((label . command) ...)) or #f
(define mls-ui-error #f)          ; (message . time) or #f
(define mls-ui-updated #f)        ; time of the last view
(define mls-ui-panel-open? #f)
(define mls-ui-diagnostics? #f)
(define mls-ui-stderr "")         ; tail of the live engine's stderr

;;; Preferences: mode (live or replay) and the remembered consent to live calls.

(define (mls-mode)
  (if (equal? (get-preference "mls-mode") "replay") 'replay 'live))

(define (mls-mode-live?) (eq? (mls-mode) 'live))
(define (mls-mode-replay?) (eq? (mls-mode) 'replay))

(define (mls-ui-time t)
  (strftime "%H:%M:%S" (localtime t)))

;;; Phase and errors

(define (mls-ui-set-phase! phase note)
  (set! mls-ui-phase phase)
  (set! mls-ui-note note)
  (when (memq phase '(starting live snapshot))
    (set! mls-ui-problem #f))
  (mls-ui-refresh))

(define (mls-ui-set-problem! problem)
  (set! mls-ui-problem problem)
  (set! mls-ui-phase 'error)
  (set! mls-ui-note (car problem))
  (set-message (string-append "MLS: " (car problem)) "MLS")
  (mls-ui-show-panel))

(define (mls-ui-error! message)
  "Called by mls-fail: keep the error in the panel."
  (set! mls-ui-error (cons message (current-time)))
  (when (eq? mls-ui-phase 'starting)
    (set! mls-ui-phase 'error)
    (set! mls-ui-note "MLS could not start"))
  (mls-ui-refresh))

(define (mls-ui-view-updated!)
  (set! mls-ui-updated (current-time)))

(tm-define (mls-ui-dismiss-error)
  (:secure #t)
  (set! mls-ui-error #f)
  (mls-ui-refresh))

(define (mls-ui-running?)
  (memq mls-ui-phase '(starting live)))

;;; Setup checks

(define (mls-ui-engine-found? path)
  (if (string-index path #\/)
      (and (file-exists? path) (access? path X_OK))
      (= 0 (system (string-append "command -v " (mls-sh-quote path) " >/dev/null 2>&1")))))

(define (mls-ui-engine-problem)
  (let ((path (mls-engine-path)))
    (and (not (mls-ui-engine-found? path))
         (list "MLS engine not found"
               (string-append "TeXmacs looked for the engine at " path
                              ". Build it with cargo build -p mls-cli, then give its path"
                              " (or a launcher script) here.")
               '(("Set engine path" . "(mls-ui-choose-engine)"))))))

(define (mls-ui-document-problem)
  (let ((url (mls-main-buffer)))
    (cond ((not url)
           (list "Open a document first"
                 "MLS checks the theorems of the document in this window."
                 '()))
          ((or (url-scratch? url) (not (url-exists? url)))
           (list "Save this document first"
                 "MLS works on a saved .tm file, so it can keep block identities with it."
                 '(("Save" . "(save-buffer)"))))
          (else #f))))

(define (mls-ui-mode-problem)
  (cond ((and (mls-mode-live?) (not (mls-live-allowed?)))
         (list "Allow live model calls?"
               (string-append "In live mode MLS sends the text of this document's theorems,"
                              " lemmas and definitions to the model provider configured for"
                              " the engine. That uses your API key and may cost money."
                              " Replay mode uses recorded answers instead and sends nothing.")
               '(("Allow and start" . "(mls-ui-allow-live)")
                 ("Use recordings" . "(mls-ui-choose-replay)"))))
        ((and (mls-mode-replay?)
              (let ((dir (mls-replay-dir)))
                (not (and (> (string-length dir) 0)
                          (char=? (string-ref dir 0) #\/)
                          (file-is-directory? dir)))))
         (list "Choose a recordings folder"
               "Replay mode answers from recorded model replies. Choose the folder that holds them."
               '(("Choose folder" . "(mls-ui-choose-replay)")
                 ("Use live calls" . "(mls-ui-use-live)"))))
        (else #f)))

(define (mls-ui-preflight)
  "The first setup problem that stops a start, or #f."
  (or (mls-ui-document-problem)
      (mls-ui-engine-problem)
      (mls-ui-mode-problem)))

(define (mls-ui-stderr-problem text)
  "Explain a failed engine start from its stderr, or #f when nothing is recognised."
  (let ((has? (lambda (s) (and (mls-search s text 0) #t))))
    (cond ((or (has? "OPENROUTER_API_KEY") (has? "API key") (has? "api key"))
           (list "Model API key missing"
                 (string-append "The engine needs OPENROUTER_API_KEY. Apps started from the"
                                " Dock do not read your shell profile: point the engine path"
                                " at a launcher script that exports the key.")
                 '(("Set engine path" . "(mls-ui-choose-engine)") ("Try again" . "(mls-start)"))))
          ((or (has? "lake") (has? "Lean") (has? "lean-toolchain"))
           (list "Lean toolchain problem"
                 "The engine could not start Lean. Run make setup-lean in the MLS repository."
                 '(("Try again" . "(mls-start)"))))
          ((has? "MLS_LIVE=1")
           (list "Live mode refused"
                 "The engine refused live calls. Allow live model calls and start again."
                 '(("Allow and start" . "(mls-ui-allow-live)"))))
          (else #f))))

(define (mls-ui-engine-failed! reason)
  "The live engine stopped or never answered. Show why, with the engine's own words."
  (set! mls-ui-problem
        (or (mls-ui-stderr-problem mls-ui-stderr)
            (list reason
                  "The engine's last messages are below. Check the engine path and settings."
                  '(("Try again" . "(mls-start)") ("Settings" . "(mls-ui-diagnostics)")))))
  (set! mls-ui-phase 'error)
  (set! mls-ui-note reason)
  (mls-ui-show-panel))

(define (mls-ui-append-stderr! text)
  (let ((all (string-append mls-ui-stderr text)))
    (set! mls-ui-stderr
          (if (> (string-length all) 2000)
              (substring all (- (string-length all) 2000) (string-length all))
              all))))

;;; Commands

(tm-define (mls-start)
  (:synopsis "Start MLS on this document")
  (:secure #t)
  (set! mls-ui-error #f)
  (set! mls-ui-stderr "")
  (mls-ui-set-phase! 'starting "Checking setup")
  (mls-ui-show-panel)
  (let ((problem (mls-ui-preflight)))
    (cond (problem (mls-ui-set-problem! problem) #f)
          ((mls-live-start)
           (mls-ui-set-phase! 'starting "Starting the engine")
           #t)
          (else
           (when (eq? mls-ui-phase 'starting)
             (mls-ui-set-phase! 'error "MLS could not start"))
           #f))))

(tm-define (mls-stop)
  (:synopsis "Stop MLS")
  (:secure #t)
  (mls-live-stop)
  (mls-ui-set-phase! 'stopped "Stopped. Pills show the last results.")
  #t)

(tm-define (mls-toggle)
  (:synopsis "Start or stop MLS")
  (if (mls-ui-running?) (mls-stop) (mls-start)))

(define (mls-ui-start-label)
  (if (mls-ui-running?) "Stop MLS" "Start MLS"))

(tm-define (mls-ui-allow-live)
  (:secure #t)
  (mls-set-live-allowed! #t)
  (set-preference "mls-mode" "live")
  (mls-start))

(tm-define (mls-ui-use-live)
  (:secure #t)
  (set-preference "mls-mode" "live")
  (mls-ui-after-settings))

(tm-define (mls-ui-choose-replay)
  (:secure #t)
  (:interactive #t)
  (interactive
   (lambda (dir)
     (when (mls-check-replay-dir dir)
       (mls-set-replay-dir! dir)
       (set-preference "mls-mode" "replay")
       (mls-ui-after-settings)))
   (list "Recordings folder" "string" (mls-replay-dir))))

(tm-define (mls-ui-choose-engine)
  (:secure #t)
  (:interactive #t)
  (interactive
   (lambda (path)
     (set-preference "mls-engine" (mls-trim path))
     (mls-ui-after-settings))
   (list "MLS engine path" "string" (get-preference "mls-engine"))))

(define (mls-ui-after-settings)
  "A setting changed: restart a running session, or retry a start that was blocked."
  (cond ((mls-ui-running?) (mls-live-stop) (delayed (:pause 300) (mls-start)))
        ((eq? mls-ui-phase 'error) (mls-start))
        (else (mls-ui-refresh))))

(tm-define (mls-ui-diagnostics)
  (:synopsis "Show MLS settings and diagnostics")
  (:secure #t)
  (set! mls-ui-diagnostics? (not mls-ui-diagnostics?))
  (mls-ui-show-panel))

(tm-define (mls-ui-refresh-snapshot)
  (:secure #t)
  (mls-show-status))

;;; The side panel

(define (mls-ui-panel-document)
  (if (buffer-exists? (string->url mls-panel-name))
      (buffer-get-body (string->url mls-panel-name))
      (tm->tree (mls-panel-tree (mls-panel-active-view)))))

(tm-tool* (mls-tool win)
  (:name "MLS")
  (:quit (set! mls-ui-panel-open? #f))
  (refreshable "mls-panel"
    (resize "360px" "640px"
      (texmacs-input (mls-ui-panel-document)
                     '(style (tuple "generic" "mls-badges" "mls-panel" "side-tools"))
                     (string->url mls-panel-name)))))

(define (mls-ui-refresh)
  "Rebuild the panel body from the active view and the session state."
  (catch #t
    (lambda ()
      (let ((build (lambda ()
                     (buffer-set-body mls-panel-name
                                      (tm->tree (mls-panel-tree (mls-panel-active-view)))))))
        (if (defined? 'mls-live-with-suppressed-notifications)
            (mls-live-with-suppressed-notifications build)
            (build)))
      (when mls-ui-panel-open? (refresh-now "mls-panel")))
    (lambda args #f)))

(tm-define (mls-ui-show-panel)
  (:synopsis "Show the MLS panel beside the document")
  (:secure #t)
  (mls-ui-refresh)
  (set! mls-ui-panel-open? #t)
  (catch #t
    (lambda () (tool-select :right 'mls-tool))
    (lambda args #f))
  #t)

(tm-define (mls-ui-hide-panel)
  (:secure #t)
  (set! mls-ui-panel-open? #f)
  (catch #t (lambda () (tool-close :any 'mls-tool noop)) (lambda args #f))
  #t)

(tm-define (mls-ui-toggle-panel)
  (:synopsis "Show or hide the MLS panel")
  (if mls-ui-panel-open? (mls-ui-hide-panel) (mls-ui-show-panel)))

;;; Panel header, setup card and diagnostics (trees used by mls-panel-tree)

(define (mls-ui-phase-pill)
  (let ((spec (case mls-ui-phase
                ((idle) '("neutral" "Not running"))
                ((starting) (list "blue" (string-append "Starting" mls-icon-dots)))
                ((live) '("green" "Live"))
                ((snapshot) '("neutral" "Snapshot"))
                ((stopped) '("neutral" "Stopped"))
                (else '("red" "Needs attention")))))
    `(mls-pill ,(car spec) "filled" ,(cadr spec))))

(define (mls-ui-document-name)
  (let ((url (if (mls-ui-running?) (mls-badges-document) (mls-main-buffer))))
    (if (and url (not (url-scratch? url))) (url->string (url-tail url)) "unsaved document")))

(define (mls-ui-mode-line)
  (let ((mode (if (mls-mode-live?)
                  "Live model calls"
                  (string-append "Replay from " (mls-replay-dir))))
        (stamp (cond ((and (eq? mls-ui-phase 'snapshot) mls-ui-updated)
                     (string-append "snapshot taken " (mls-ui-time mls-ui-updated)
                                    ", not updating"))
                    (mls-ui-updated (string-append "updated " (mls-ui-time mls-ui-updated)))
                    (else "no results yet"))))
    `(with "font-size" "0.84" ,(string-append mode " <#2014> " stamp))))

(define (mls-ui-actions pairs)
  "A row of action links from (label . command) pairs."
  (let loop ((xs pairs) (acc '()))
    (if (null? xs)
        (cons 'concat (reverse acc))
        (loop (cdr xs)
              (append (list `(action ,(caar xs) ,(cdar xs)))
                      (if (null? acc) acc (cons "   " acc)))))))

(define (mls-ui-header)
  (list
   `(concat ,(mls-ui-phase-pill) (hspace "0.6spc") (strong ,(mls-ui-document-name)))
   (mls-ui-mode-line)
   (mls-ui-actions
    (append (if (mls-ui-running?)
                '(("Stop" . "(mls-stop)"))
                '(("Start" . "(mls-start)")))
            (if (eq? mls-ui-phase 'snapshot)
                '(("Refresh snapshot" . "(mls-ui-refresh-snapshot)"))
                '())
            '(("Settings" . "(mls-ui-diagnostics)") ("Hide" . "(mls-ui-hide-panel)"))))))

(define (mls-ui-problem-card)
  (if (not mls-ui-problem)
      '()
      (append
       (list `(concat (mls-pill "amber" "filled" "!") (hspace "0.5spc")
                      (strong ,(car mls-ui-problem)))
             (cadr mls-ui-problem))
       (if (null? (caddr mls-ui-problem)) '() (list (mls-ui-actions (caddr mls-ui-problem))))
       (if (and (> (string-length mls-ui-stderr) 0) (eq? mls-ui-phase 'error))
           (list `(with "font-family" "tt" "font-size" "0.75" ,(mls-ui-stderr-tail)))
           '()))))

(define (mls-ui-stderr-tail)
  (let* ((lines (mls-non-blank-lines mls-ui-stderr))
         (n (length lines)))
    (mls-join (if (> n 6) (list-tail lines (- n 6)) lines) " / ")))

(define (mls-ui-error-banner)
  (if (not mls-ui-error)
      '()
      (list `(concat (mls-pill "red" "filled" ,(string-append mls-icon-cross " Error"))
                     (hspace "0.5spc") ,(car mls-ui-error))
            `(concat (with "font-size" "0.84"
                       ,(string-append "at " (mls-ui-time (cdr mls-ui-error)) "   "))
                     (action "Dismiss" "(mls-ui-dismiss-error)")))))

(define (mls-ui-check-line ok? label detail)
  `(concat (mls-pill ,(if ok? "green" "red") "filled"
                     ,(if ok? mls-icon-check mls-icon-cross))
           (hspace "0.5spc") (strong ,label) (hspace "0.5spc") ,detail))

(define (mls-ui-diagnostics-lines)
  (if (not mls-ui-diagnostics?)
      '()
      (let* ((engine (mls-engine-path))
             (doc (mls-ui-document-problem))
             (mode (mls-ui-mode-problem)))
        (list
         '(strong "Settings and diagnostics")
         (mls-ui-check-line (not doc) "Document"
                            (if doc (car doc) (mls-ui-document-name)))
         (mls-ui-check-line (mls-ui-engine-found? engine) "Engine" engine)
         (mls-ui-check-line (not mode) "Mode"
                            (if (mls-mode-live?)
                                (if (mls-live-allowed?) "live calls allowed" "live calls not allowed yet")
                                (string-append "replay from " (mls-replay-dir))))
         (mls-ui-actions '(("Use live calls" . "(mls-ui-use-live)")
                           ("Use recordings" . "(mls-ui-choose-replay)")
                           ("Set engine path" . "(mls-ui-choose-engine)")))
         (mls-ui-actions (if (mls-live-allowed?)
                             '(("Revoke live permission" . "(mls-ui-revoke-live)"))
                             '(("Allow live calls" . "(mls-ui-allow-live)"))))
         '(with "font-size" "0.84"
            "Live mode sends theorem text to the configured model provider and may cost money. Lean runs locally inside the engine.")))))

(tm-define (mls-ui-revoke-live)
  (:secure #t)
  (mls-set-live-allowed! #f)
  (mls-ui-refresh))

;;; Focus toolbar and context menu for the block under the cursor

(define (mls-ui-carrier-id t)
  (and (tree? t) (mls-carrier? t)
       (let ((id (tree->stree (tree-ref t 1))))
         (and (mls-badge-uuid? id) id))))

(define (mls-ui-focus-id t)
  "The block UUID when T is an MLS carrier or a formal block directly inside one."
  (catch #t
    (lambda ()
      (or (mls-ui-carrier-id t)
          (and (tree? t)
               (tree-in? t '(theorem lemma proposition corollary definition))
               (let* ((up (tree-up t))
                      (up2 (and up (tree-up up))))
                 (or (mls-ui-carrier-id up)
                     (and up (tree-is? up 'document) (mls-ui-carrier-id up2)))))))
    (lambda args #f)))

(define (mls-ui-focus-summary id)
  (let ((specs (mls-badge-block-specs id)))
    (if (null? specs)
        "MLS: not checked yet"
        (string-append "MLS: " (mls-join (map cadddr specs) ", ")))))

(define (mls-ui-retry? id)
  (let ((block (mls-badge-view-block id)))
    (and block
         (or (eq? (mls-json-get block "needs_rerun") #t)
             (equal? (mls-field block "status") "formalization_error")))))

(tm-menu (focus-extra-icons t)
  (:require (mls-ui-focus-id t))
  (let ((id (mls-ui-focus-id t)))
    //
    (if (and (not (mls-ui-running?)) (not (mls-badge-view-block id)))
        ("Start MLS" (mls-start)))
    (if (or (mls-ui-running?) (mls-badge-view-block id))
        (text (mls-ui-focus-summary id))
        //
        (if (mls-badge-view-block id)
            ("Review" (mls-badge-open id)))
        (if (mls-ui-retry? id)
            ("Retry" (mls-badge-retry id))))
    ("MLS panel" (mls-ui-show-panel))))

(tm-menu (focus-extra-menu t)
  (:require (mls-ui-focus-id t))
  (let ((id (mls-ui-focus-id t)))
    ---
    (if (mls-badge-view-block id)
        ("MLS: review this block" (mls-badge-open id)))
    (if (mls-ui-retry? id)
        ("MLS: formalize this block again" (mls-badge-retry id)))
    ((eval (mls-ui-start-label)) (mls-toggle))
    ("Show MLS panel" (mls-ui-show-panel))))
