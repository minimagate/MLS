;;; test-review.scm -- confirm, reject and edit against the stub engine, SIR edit
;;; validation and the round trip through the SIR JSON file, and the card procedures.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(mls-test-main
 (lambda ()
   (define work (mls-test-env "MLS_TEST_WORK"))
   (define url (string->url (mls-test-doc "canonical.tm")))
   (define drafted "a98f524a-4ad6-4b0d-92d2-15b37d08f555")
   (define theorem "e32a226a-c78b-4321-9eba-e14b4c734ed4")
   (define bad-fields
     (list (cons "variables" "no colon here") (cons "hypotheses" "")
           (cons "conclusion" "x is a widget") (cons "assumptions" "")))
   (define good-fields
     (list (cons "variables" "x : set\ny : real")
           (cons "hypotheses" "x is nonempty\n\n  y >= 0 ")
           (cons "conclusion" "x is a widget")
           (cons "assumptions" "widget is a set\nx is finite")))
   (lazy-plugin-force)
   (setenv "MLS_STUB_FAIL" "")
   (setenv "MLS_STUB_SEQ" "")
   (setenv "MLS_STUB_VIEW" (mls-test-views "replay.json"))
   (load-buffer url)
   (mls-mark-synced! url)
   (mls-test-clear-log)
   (mls-formalize 'replay work)
   (mls-check "a view is loaded for review" (mls-view? mls-current-view))

   ;; confirm and reject: the stub returns the confirmed view for confirm.
   (mls-test-clear-log)
   (setenv "MLS_STUB_VIEW" (mls-test-views "confirmed.json"))
   (mls-check "confirm succeeds" (mls-review-action drafted "confirm" '()))
   (mls-check "confirm calls review-action with block and action"
              (mls-test-contains? (mls-test-log)
                                  (string-append "<review-action> <" (mls-test-doc "canonical.tm")
                                                 "> <--block> <" drafted "> <--action> <confirm>")))
   (mls-check-equal "confirm re-renders the counts" 1 (mls-count mls-current-view "statement_confirmed"))
   (setenv "MLS_STUB_VIEW" (mls-test-views "replay.json"))
   (mls-check "reject succeeds" (mls-review-action drafted "reject" '()))
   (mls-check "reject passes --action reject"
              (mls-test-contains? (mls-test-log) "<--action> <reject>"))

   ;; edit: a complete SIR file goes to the engine and parses back.
   (mls-test-clear-log)
   (mls-check "edit succeeds" (mls-review-action drafted "edit" good-fields))
   (mls-check "edit passes --sir" (mls-test-contains? (mls-test-log) "<--sir>"))
   (let* ((sir-path (string-append (mls-test-env "MLS_STUB_LOG") ".sir"))
          (sir (mls-json-parse (mls-test-file-text sir-path))))
     (mls-check "edited SIR is valid JSON with an object root" (pair? sir))
     (mls-check-equal "edited variables" 2 (vector-length (mls-json-get sir "variables")))
     (mls-check-equal "first variable name" "x"
                      (mls-json-get (vector-ref (mls-json-get sir "variables") 0) "name"))
     (mls-check-equal "second variable type" "real"
                      (mls-json-get (vector-ref (mls-json-get sir "variables") 1) "type"))
     (mls-check-equal "hypotheses are trimmed and blank lines dropped"
                      (vector "x is nonempty" "y >= 0")
                      (mls-json-get sir "hypotheses"))
     (mls-check-equal "conclusion" "x is a widget" (mls-json-get sir "conclusion"))
     (mls-check-equal "assumptions_made" (vector "widget is a set" "x is finite")
                      (mls-json-get sir "assumptions_made"))
     (mls-check-equal "kind is kept from the block" "definition" (mls-json-get sir "kind"))
     (mls-check-equal "label is kept from the block" "def:widget" (mls-json-get sir "label"))
     (mls-check-equal "uses is kept from the block" (vector "def:set") (mls-json-get sir "uses")))

   ;; invalid edits never reach the engine.
   (mls-test-clear-log)
   (mls-check "bad variable line is refused" (not (mls-review-action drafted "edit" bad-fields)))
   (mls-check "bad variable line is reported"
              (mls-test-contains? (or mls-last-error "") "name : type"))
   (mls-check "empty conclusion is refused"
              (not (mls-review-action drafted "edit"
                                      (list (cons "variables" "x : set") (cons "conclusion" "  ")))))
   (mls-check "refused edits call no engine" (not (mls-test-contains? (mls-test-log) "review-action")))
   (mls-check "block without SIR cannot be edited"
              (not (mls-review-action theorem "edit" good-fields)))
   (mls-check "unknown action is refused" (not (mls-review-action drafted "approve" '())))

   ;; round trip: the edit form text reproduces the stored SIR.
   (let* ((block (mls-view-block drafted))
          (sir (mls-json-get block "sir")))
     (mls-check-equal "variables text round-trips"
                      (mls-json-get sir "variables")
                      (mls-parse-variables (mls-variables-text sir))))

   ;; cards: read-only text, the dispatch entry points, and missing-document handling.
   (let ((card (mls-card-document (mls-view-block drafted))))
     (mls-check "card shows the conclusion"
                (mls-test-contains? (object->string card) "Conclusion: x is a widget"))
     (mls-check "card shows the Lean statement"
                (mls-test-contains? (object->string card) "def Widget")))
   (mls-check "review card opens" (mls-review-open drafted))
   (mls-check "edit card opens" (mls-review-open-edit drafted))
   (mls-check "edit card refuses a block without SIR" (not (mls-review-open-edit theorem)))
   (mls-check "close message does nothing" (mls-review-dispatch (list "close")))
   (setenv "MLS_STUB_VIEW" (mls-test-views "confirmed.json"))
   (mls-test-clear-log)
   (mls-check "confirm from the card dispatches" (mls-review-dispatch (list "confirm" drafted)))
   (mls-check "card confirm reaches the engine"
              (mls-test-contains? (mls-test-log) "<--action> <confirm>"))
   (mls-check "unknown block cannot open a card" (not (mls-review-open "00000000-0000-4000-8000-00000000ffff")))

   (let ((saved mls-current-document))
     (set! mls-current-document #f)
     (mls-check "review without a document is refused" (not (mls-review-action drafted "confirm" '())))
     (set! mls-current-document saved))))
