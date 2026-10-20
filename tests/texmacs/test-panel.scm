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
   (lazy-plugin-force)
   (set! view (mls-json-parse (mls-test-file-text (mls-test-views "replay.json"))))
   (set! tree-text (object->string (mls-panel-tree view)))

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
