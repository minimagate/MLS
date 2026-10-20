;;; test-panel.scm -- the status panel rows, the footer badge, Go to (cursor lands on the
;;; carrier), and the index-based panel actions.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(define (mls-path<? a b)
  "Lexicographic order on cursor paths."
  (cond ((null? a) (pair? b))
        ((null? b) #f)
        ((< (car a) (car b)) #t)
        ((> (car a) (car b)) #f)
        (else (mls-path<? (cdr a) (cdr b)))))

(mls-test-main
 (lambda ()
   (define url (string->url (mls-test-doc "canonical.tm")))
   (define drafted "a98f524a-4ad6-4b0d-92d2-15b37d08f555")
   (define theorem "e32a226a-c78b-4321-9eba-e14b4c734ed4")
   (define view #f)
   (define tree-text "")
   (define cursor-drafted #f)
   (define cursor-theorem #f)
   (define proof-view #f)
   (define proof-tree-text "")
   (define annotated-view #f)
   (define annotated-block #f)
   (define annotated-panel-text "")
   (define annotated-card-text "")
   (lazy-plugin-force)
   (set! view (mls-json-parse (mls-test-file-text (mls-test-views "replay.json"))))
   (set! tree-text (object->string (mls-panel-tree view)))
   (mls-check "blocks without decision annotations render without review highlights"
              (let ((plain-block (vector-ref (mls-json-get view "blocks") 0)))
                (and (null? (mls-decision-lines plain-block))
                     (string? (mls-balloon-text plain-block))
                     (pair? (mls-card-lines plain-block)))))

   (set! annotated-view
         (mls-alist-set
          view "blocks"
          (list->vector
           (map (lambda (block)
                  (if (equal? (mls-field block "block_id") drafted)
                      (mls-alist-set
                       (mls-alist-set
                        block "decision_fields"
                        (vector '( ("field" . "conclusion.quantifier")
                                   ("reason" . "disagrees")
                                   ("evidence" . null) )))
                       "decision_differs"
                       '(("value" . (("probability" . 0.97)))
                         ("basis" . "jev")))
                      block))
                (vector->list (mls-json-get view "blocks"))))))
   (set! annotated-block
         (vector-ref (mls-json-get annotated-view "blocks") 1))
   (set! annotated-panel-text (object->string (mls-panel-tree annotated-view)))
   (set! annotated-card-text (object->string (mls-card-lines annotated-block)))
   (mls-check "decision highlights appear in the panel tooltip and review card"
              (and (mls-test-contains? annotated-panel-text "conclusion.quantifier")
                   (mls-test-contains? annotated-panel-text "disagrees")
                   (mls-test-contains? annotated-panel-text "Warning: SIR may differ from source text")
                   (mls-test-contains? annotated-card-text "conclusion.quantifier")
                   (mls-test-contains? annotated-card-text "disagrees")
                   (mls-test-contains? annotated-card-text "Warning: SIR may differ from source text")))
   (mls-check "decision highlights leave status and confirmation unchanged"
              (and (equal? (mls-field annotated-block "status") "sir_drafted")
                   (equal? (mls-field annotated-block "review") "none")))

   ;; M5 ProofReport wire shape: proof is optional, with a tagged status and report fields.
   (set! proof-view
         (mls-alist-set view "blocks"
                        (list->vector
                         (let loop ((blocks (vector->list (mls-json-get view "blocks")))
                                    (statuses '("verified" "unproven" "refuted"))
                                    (acc '()))
                           (if (null? statuses) (append (reverse acc) (cdr blocks))
                               (let* ((block (car blocks))
                                      (outcome (car statuses))
                                      (proof `(("status" . ,(if (equal? outcome "refuted")
                                                                `(("status" . ,outcome)
                                                                  ("evidence" . "checked counterexample"))
                                                                `(("status" . ,outcome))))
                                               ("candidate" . null) ("attempts" . 1)
                                               ("diagnostics" . #()))))
                                 (loop (cdr blocks) (cdr statuses)
                                       (cons (mls-alist-set
                                              (mls-alist-set
                                               (mls-alist-set block "status" "statement_confirmed")
                                               "proof_status" outcome)
                                              "proof" proof)
                                             acc))))))))
   (set! proof-view
         (mls-alist-set proof-view "proof_counts"
                        '(("verified" . 1) ("unproven" . 1) ("refuted" . 1))))
   (set! proof-view
         (mls-alist-set proof-view "counts"
                        '(("not_formalized" . 0) ("sir_drafted" . 0)
                          ("statement_confirmed" . 3) ("formalization_error" . 0))))
   (set! proof-tree-text (object->string (mls-panel-tree proof-view)))
   (mls-check "proof outcomes appear without replacing statement status"
              (and (mls-test-contains? proof-tree-text "statement confirmed")
                   (mls-test-contains? proof-tree-text "proof verified")
                   (mls-test-contains? proof-tree-text "proof unproven")
                   (mls-test-contains? proof-tree-text "proof refuted")))
   (mls-check "proof marker colors distinguish all outcomes"
              (and (mls-test-contains? proof-tree-text "green")
                   (mls-test-contains? proof-tree-text "orange")
                   (mls-test-contains? proof-tree-text "red")))
   (mls-check-equal "proof outcome counts appear in badge"
                    "MLS: 0 not formalized, 0 SIR drafted, 3 confirmed, 0 errors; proofs: 1 verified, 1 unproven, 1 refuted"
                    (mls-badge-text proof-view))

   (mls-check-equal "one Go to action per block" 4 (mls-test-count tree-text "\"Go to\""))
   (mls-check-equal "one Review action per block" 4 (mls-test-count tree-text "\"Review\""))
   (mls-check "row actions call index entry points"
              (mls-test-contains? tree-text "(mls-panel-go-to-index 3)"))
   (mls-check "status names appear" (and (mls-test-contains? tree-text "SIR drafted")
                                         (mls-test-contains? tree-text "formalization error")))
   (mls-check "error message is in the hover balloon"
              (mls-test-contains? tree-text "unknown identifier widget"))
   (mls-check-equal "badge text"
                    "MLS: 2 not formalized, 1 SIR drafted, 0 confirmed, 1 errors"
                    (mls-badge-text view))
   (mls-check-equal "badge notes a rerun"
                    "MLS: 0 not formalized, 0 SIR drafted, 0 confirmed, 0 errors (rerun needed)"
                    (mls-badge-text (list (cons "counts" (list)) (cons "needs_rerun" #t))))
   (mls-check "empty view says so"
              (mls-test-contains? (object->string
                                   (mls-panel-tree (list (cons "counts" (list))
                                                         (cons "blocks" (vector)))))
                                  "No formal blocks"))

   (load-buffer url)
   (set! mls-current-view view)
   (set! mls-current-document url)
   (mls-check "Go to finds the carrier of a known block" (mls-go-to-block drafted))
   (set! cursor-drafted (cursor-path))
   (mls-check "Go to an unknown block fails"
              (not (mls-go-to-block "00000000-0000-4000-8000-00000000ffff")))
   (mls-check "unknown block reports an error"
              (mls-test-contains? (or mls-last-error "") "not in the document"))
   (mls-check "Go to the later theorem moves the cursor" (mls-go-to-block theorem))
   (set! cursor-theorem (cursor-path))
   (mls-check "Go to follows document order" (mls-path<? cursor-drafted cursor-theorem))
   (mls-check "panel Go to by row index" (mls-panel-go-to-index 1))
   (mls-check "panel row 1 is the definition" (equal? cursor-drafted (cursor-path)))
   (mls-check "panel Go to with a bad index fails" (not (mls-panel-go-to-index 9)))
   (mls-check "panel Review opens a card" (mls-panel-review-index 1))
   (mls-check "panel Review with a bad index fails" (not (mls-panel-review-index 9)))

   (mls-check-equal "render returns the badge"
                    "MLS: 2 not formalized, 1 SIR drafted, 0 confirmed, 1 errors"
                    (mls-render-view view))
   (mls-check "rendering shows the panel"
              (mls-test-contains? (mls-test-body-text (string->url mls-panel-name)) "MLS status"))))
