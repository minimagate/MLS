;;; test-hostile.scm -- hostile strings never reach a shell or TeXmacs markup evaluation.
;;; Probes are shell and markup payloads that would create a sentinel file (PWN_*) with
;;; a relative `touch`. run.sh starts every test in its own sentinels directory, which is
;;; the working directory of this process, so a sentinel appearing there is an injection.
;;; Covered: document names, the replay directory, MLS_BIN, block ids, NUL bytes, engine
;;; text in the status panel, the review cards, the failure messages.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(define (sentinel? name) (file-exists? name))

(define (fold-left-like f acc xs)
  (if (null? xs) acc (fold-left-like f (f (car xs) acc) (cdr xs))))

(define (walk-find tree pred)
  "The sub-trees of TREE, depth first, for which PRED holds."
  (let loop ((t tree) (acc '()))
    (let ((acc2 (if (pred t) (cons t acc) acc)))
      (if (pair? t) (fold-left-like loop acc2 t) acc2))))

(define (has-action-with-payload? t)
  (and (pair? t) (eq? (car t) 'action) (pair? (cdr t))
       (mls-test-contains? (object->string t) "PWN_JSON")))

(define (fresh name)
  "The canonical fixture copied to the work file NAME, loaded and current."
  (let ((path (mls-test-doc name)))
    (string-save (mls-test-file-text (mls-test-fixture "canonical.tm")) path)
    (load-buffer (string->url path))
    (string->url path)))

(mls-test-main
 (lambda ()
   (define work (mls-test-env "MLS_TEST_WORK"))
   (define stub (mls-test-env "MLS_BIN"))
   (define evil-id "x'; touch PWN_ID1; echo '\"$(touch PWN_ID2)`touch PWN_ID3`\n")
   (lazy-plugin-force)
   (setenv "MLS_STUB_FAIL" "")
   (setenv "MLS_STUB_SEQ" "")
   (setenv "MLS_STUB_WRITE_IDS_FROM" "")
   (setenv "MLS_STUB_RUN_FROM" "")
   (setenv "MLS_STUB_VIEW" (mls-test-views "replay.json"))

   ;; control: an unquoted system call creates the sentinel, so the checks below mean something.
   (system "touch PWN_CONTROL")
   (mls-check "control: an unquoted system call creates a sentinel" (sentinel? "PWN_CONTROL"))

   ;; document names: argv arrives intact, no injected file.
   (for-each
    (lambda (name)
      (let ((path (string-append work "/" name)))
        (catch #t
          (lambda ()
            (string-save (mls-test-file-text (mls-test-fixture "canonical.tm")) path)
            (load-buffer (string->url path))
            (mls-test-clear-log)
            (mls-formalize 'replay work)
            (mls-check (string-append "document name arrives intact: " name)
                       (mls-test-contains? (mls-test-log) (string-append "<" path ">"))))
          (lambda args (mls-check (string-append "document name is usable: " name) #f)))))
    (list "a$(touch PWN_DOC1)b.tm" "a`touch PWN_DOC2`b.tm" "c'; touch PWN_DOC3; '.tm"
          "d\"; touch PWN_DOC4; \".tm" "g $HOME \\ x.tm" "h\n; touch PWN_DOC5.tm"))
   (mls-check "no injected file from document names"
              (not (or (sentinel? "PWN_DOC1") (sentinel? "PWN_DOC2") (sentinel? "PWN_DOC3")
                       (sentinel? "PWN_DOC4") (sentinel? "PWN_DOC5.tm"))))

   ;; replay directory with shell metacharacters.
   (fresh "hostile-doc.tm")
   (let ((rec (string-append work "/rec$(touch PWN_DIR1) `touch PWN_DIR2` 'q'")))
     (system (string-append "mkdir -p " (mls-sh-quote rec)))
     (mls-test-clear-log)
     (mls-check "hostile replay dir accepted" (mls-formalize 'replay rec))
     (mls-check "replay dir arrives intact"
                (mls-test-contains? (mls-test-log) (string-append "<--replay> <" rec ">")))
     (mls-check "no injected file from the replay dir"
                (not (or (sentinel? "PWN_DIR1") (sentinel? "PWN_DIR2")))))

   ;; MLS_BIN with shell metacharacters in its directory.
   (let ((bin (string-append work "/bin $(touch PWN_BIN) dir")))
     (system (string-append "mkdir -p " (mls-sh-quote bin) " && cp " (mls-sh-quote stub) " "
                            (mls-sh-quote (string-append bin "/mls"))))
     (setenv "MLS_BIN" (string-append bin "/mls"))
     (mls-test-clear-log)
     (mls-check "hostile MLS_BIN path runs the engine" (mls-formalize 'replay work))
     (mls-check "engine argv seen through the hostile MLS_BIN"
                (mls-test-contains? (mls-test-log) "CALL <review-run>"))
     (mls-check "no injected file from MLS_BIN" (not (sentinel? "PWN_BIN")))
     (setenv "MLS_BIN" stub))

   ;; block ids with quotes, newlines and command substitution, sent to review-action.
   (mls-test-clear-log)
   (mls-check "review confirm with a hostile id runs" (mls-review-dispatch (list "confirm" evil-id)))
   (mls-check "block id arrives as one argument"
              (mls-test-contains? (mls-test-log) (string-append "<--block> <" evil-id ">")))
   (mls-check "no injected file from the block id"
              (not (or (sentinel? "PWN_ID1") (sentinel? "PWN_ID2") (sentinel? "PWN_ID3"))))

   ;; NUL in an engine argument: the command is cut short, nothing runs.
   (catch #t
     (lambda ()
       (mls-with-workdir
        (lambda ()
          (mls-engine-call (list "review-status"
                                 (string-append "x" (string (integer->char 0)) "; touch PWN_NUL"))
                           #f #f))))
     (lambda args #f))
   (mls-check "no injected file from a NUL in an argument" (not (sentinel? "PWN_NUL")))

   ;; engine-controlled view: strings with markup and payloads stay inert in the panel.
   (setenv "MLS_STUB_VIEW" (mls-test-views "hostile.json"))
   (mls-check "hostile view is a protocol-1 view"
              (mls-view? (mls-json-parse (mls-test-file-text (mls-test-views "hostile.json")))))
   (mls-check "status with the hostile view renders" (mls-show-status))
   (let ((st (tree->stree (buffer-get-body (string->url mls-panel-name)))))
     (mls-check "no action node carries engine text in the panel"
                (null? (walk-find st has-action-with-payload?))))
   (let ((uuid "<action|x|(system \"touch PWN_JSON0\")>"))
     (mls-check "review card opens for the hostile block" (mls-review-open uuid))
     (mls-check "edit card opens for the hostile block" (mls-review-open-edit uuid))
     (mls-check "card document has no action node"
                (null? (walk-find (tree->stree (tm->tree (mls-card-document (mls-view-block uuid))))
                                  has-action-with-payload?)))
     (mls-test-clear-log)
     (mls-check "confirm from the hostile block runs" (mls-review-dispatch (list "confirm" uuid)))
     (mls-check "the hostile block id is sent as one argument"
                (mls-test-contains? (mls-test-log) (string-append "<--block> <" uuid ">"))))
   (for-each (lambda (n) (mls-check (string-append "no injected file " n) (not (sentinel? n))))
             (list "PWN_JSON0" "PWN_JSON1" "PWN_JSON2" "PWN_JSON3" "PWN_JSON4" "PWN_JSON5"
                   "PWN_JSON6" "PWN_JSON7" "PWN_JSON8" "PWN_JSON9" "PWN_JSON10"))

   ;; markup in plain strings is literal text, and failure messages are shown as text.
   (let ((t (tm->tree "<action|x|(system \"touch PWN_DIRECT\")>")))
     (mls-check "tm->tree keeps markup text literal" (not (and (pair? t) (eq? (car t) 'action)))))
   (mls-fail "<action|x|(system \"touch PWN_MSG1\")> $(touch PWN_MSG2) `touch PWN_MSG3`")
   (mls-check "failure text with markup and substitutions is shown as text"
              (mls-test-contains? (or mls-last-error "") "PWN_MSG1"))
   (mls-check "no injected file from an engine error message"
              (not (or (sentinel? "PWN_MSG1") (sentinel? "PWN_MSG2") (sentinel? "PWN_MSG3"))))
   (setenv "MLS_STUB_VIEW" (mls-test-views "replay.json"))))
