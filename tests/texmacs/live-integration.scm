;;; Persistent live-client integration: uncarried source, dynamic IDs, annotations, and
;;; revision acknowledgement. Run through run-live.sh with the offscreen Qt loop.
(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(define live-document-url #f)
(define live-initial-text #f)
(define live-before-change-text #f)
(define live-new-id #f)

(define (live-log) (mls-test-file-text (mls-test-env "MLS_LIVE_STUB_LOG")))
(define (live-order-log) (mls-test-file-text (mls-test-env "MLS_LIVE_STUB_ORDER_LOG")))
(define (live-engine-log) (mls-test-file-text (mls-test-env "MLS_STUB_LOG")))
(define (live-unique? items)
  (let loop ((rest items) (seen '()))
    (cond ((null? rest) #t)
          ((member (car rest) seen) #f)
          (else (loop (cdr rest) (cons (car rest) seen))))))
(define (live-new-carrier-id)
  (let loop ((items (mls-carriers)))
    (cond ((null? items) #f)
          ((mls-test-contains? (object->string (tree->stree (cdar items)))
                               "A theorem added during the live session.")
           (caar items))
          (else (loop (cdr items))))))

(define (live-new-carrier-ids)
  (let loop ((items (map car (mls-carriers))) (ids '()))
    (cond ((null? items) (reverse ids))
          ((member (car items)
                   '("c654c3b7-09ef-48dd-add4-741f0790a71a"
                     "a98f524a-4ad6-4b0d-92d2-15b37d08f555"
                     "e32a226a-c78b-4321-9eba-e14b4c734ed4"
                     "be971836-2c21-4dfb-9128-ed7591e24db3"))
           (loop (cdr items) ids))
          (else (loop (cdr items) (cons (car items) ids))))))

(define (live-new-carrier-still-stable?)
  (and live-new-id (equal? live-new-id (live-new-carrier-id))))

(define (live-focus-new-block)
  (let ((hit (and live-new-id (assoc live-new-id (mls-carriers)))))
    (and hit (tree-go-to (cdr hit) :start))))

(define (live-finish)
  (mls-check "live updates retain focus on the editing buffer"
             (equal? (url->string (current-buffer)) (url->string live-document-url)))
  (let ((saved-document mls-current-document)
        (saved-view mls-current-view))
    (set! mls-current-document #f)
    (set! mls-current-view #f)
    (mls-check "live panel actions use their dedicated URL and view"
               (and (equal? (mls-field (mls-panel-block 1) "block_id")
                            "a98f524a-4ad6-4b0d-92d2-15b37d08f555")
                    (equal? (mls-field (mls-view-block "a98f524a-4ad6-4b0d-92d2-15b37d08f555")
                                       "block_id")
                            "a98f524a-4ad6-4b0d-92d2-15b37d08f555")
                    (mls-go-to-block "a98f524a-4ad6-4b0d-92d2-15b37d08f555")
                    (equal? (url->string (current-buffer)) (url->string live-document-url))))
    (set! mls-current-document saved-document)
    (set! mls-current-view saved-view))
  (mls-check "daemon received write-ids before the live open"
             (mls-test-contains? (live-engine-log) "CALL <--write-ids>"))
  (mls-check "daemon received initialization and revisioned didOpen with existing IDs"
             (and (mls-test-contains? (live-log) "\"method\": \"initialize\", \"params\": {}")
                  (mls-test-contains? (live-log) "\"method\": \"didOpen\"")
                  (mls-test-contains? (live-log) "c654c3b7-09ef-48dd-add4-741f0790a71a")
                  (mls-test-contains? (live-log) "a98f524a-4ad6-4b0d-92d2-15b37d08f555")
                  (mls-test-contains? (live-log) "e32a226a-c78b-4321-9eba-e14b4c734ed4")
                  (mls-test-contains? (live-log) "be971836-2c21-4dfb-9128-ed7591e24db3")))
  (mls-check "serialized Unicode reached daemon as ASCII escapes"
             (mls-test-contains? (live-log) "\\u00c3"))
  (mls-check "new formal block received a stable unique carrier before didChange"
             (and live-new-id
                  (live-new-carrier-still-stable?)
                  (live-unique? (map car (mls-carriers)))
                  (mls-test-contains? (live-log) live-new-id)
                  (equal? live-before-change-text
                          (mls-strip-carriers (mls-body-stree live-document-url)))))
  (mls-check "live didChange leaves new carriers unsaved until the user saves"
             (let ((saved (mls-test-file-text (mls-test-doc "canonical.tm"))))
               (and (pair? (live-new-carrier-ids))
                    (not (mls-test-contains? saved (car (live-new-carrier-ids)))))))
  (mls-check "normal TeXmacs save persists the live theorem carrier"
             (and (not (buffer-save live-document-url))
                  (let ((saved (mls-test-file-text (mls-test-doc "canonical.tm"))))
                    (let loop ((ids (live-new-carrier-ids)))
                      (or (null? ids)
                          (and (mls-test-contains? saved (car ids))
                               (loop (cdr ids))))))))
  (mls-check "block status rendered decision review hints without changing review state"
             (let* ((block (mls-live-view-block "a98f524a-4ad6-4b0d-92d2-15b37d08f555"))
                    (panel (object->string (tree->stree (buffer-get-body mls-panel-name))))
                    (card (object->string (mls-card-lines block)))
                    (warning "Warning: SIR may differ from source text"))
               (and block
                    (equal? (mls-field block "status") "statement_confirmed")
                    (equal? (mls-field block "review") "confirmed")
                    (mls-test-contains? panel "conclusion.quantifier")
                    (mls-test-contains? panel "disagrees")
                    (mls-test-contains? panel warning)
                    (mls-test-contains? card "conclusion.quantifier")
                    (mls-test-contains? card "disagrees")
                    (mls-test-contains? card warning))))
  (mls-check "live status panel has an explicit open action"
             (and (mls-live-show-panel)
                  (equal? (url->string (current-buffer)) mls-panel-name)))
  (switch-to-buffer live-document-url)
  (mls-check "daemon rejected didChange without advancing acknowledged revision"
             (and (= mls-live-revision 2)
                  (equal? mls-live-last-error "test rejection")
                  (= (mls-test-count (live-log) "\"method\": \"didChange\"") 2)))
  (mls-check "confirm used persistent connection without parsing unrelated edit fields"
             (and (= (mls-test-count (live-log) "\"method\": \"review\"") 1)
                  (= (mls-test-count (live-log) "\"method\": \"cursor\"") 2)
                  (mls-test-contains? (live-log) "\"revision\": 2, \"block\"")
                  (mls-test-contains? (live-log) "\"action\": \"confirm\"")))
  (mls-check "no cursor or review request used the old revision while didChange was pending"
             (string=? (live-order-log) ""))
  (mls-live-stop)
  (delayed (:pause 350)
    (display* "SUMMARY pass=" mls-test-pass " fail=" mls-test-fail "\n")
    (quit-TeXmacs)))

(define (live-wait-until ready? budget-ms then)
  "Call THEN once READY? holds, or after BUDGET-MS milliseconds, polling every 100 ms.
TeXmacs and the stub daemon can take several seconds to start, so a fixed pause races."
  (if (or (ready?) (<= budget-ms 0))
      (then)
      (delayed (:pause 100) (live-wait-until ready? (- budget-ms 100) then))))

(define (live-run)
  (let* ((path (mls-test-doc "canonical.tm"))
         (url (string->url path)))
    (set! live-document-url url)
    (load-buffer url)
    (switch-to-buffer url)
    (mls-check "live fixture begins with an uncarried formal block"
               (null? (mls-carriers)))
    (set! live-initial-text (mls-strip-carriers (mls-body-stree url)))
    (mls-toggle-live)
    (mls-check "live session starts after guarded ID persistence" (mls-live-start))
    (mls-check "review action is blocked before didOpen acknowledgement"
               (not (mls-live-review-action
                     "a98f524a-4ad6-4b0d-92d2-15b37d08f555" "confirm" '())))
    (live-wait-until
     (lambda () (and (= mls-live-revision 1) mls-live-view))
     20000
     (lambda ()
      (switch-to-buffer url)
      (when (not (= mls-live-revision 1))
        (display* "  live-last-error: " (object->string mls-live-last-error) "\n"))
      (mls-check "initialize and open handshake completed" (= mls-live-revision 1))
      (mls-check "initial status view is tagged with the acknowledged revision"
                 (= mls-live-view-revision mls-live-revision))
      (mls-check "live view accepted" (and mls-live-view (mls-view? mls-live-view)))
      (mls-check "startup write-ids persisted carriers"
                 (and (mls-test-contains? (mls-test-file-text (mls-test-doc "canonical.tm"))
                                         "mls-id|a98f524a-4ad6-4b0d-92d2-15b37d08f555")
                      (mls-test-contains? (mls-test-file-text (mls-test-doc "canonical.tm"))
                                         "mls-id|e32a226a-c78b-4321-9eba-e14b4c734ed4")))
      (mls-check "startup write-ids preserved carrier-stripped document text"
                 (equal? live-initial-text
                         (mls-strip-carriers (mls-body-stree live-document-url))))
      (mls-check "open added no carriers to already carried blocks"
                 (equal? live-initial-text (mls-strip-carriers (mls-body-stree url))))
      (mls-check "live panel action opens the current view"
                 (and (mls-live-show-panel)
                      (equal? (url->string (current-buffer)) mls-panel-name)))
      (let* ((other-url (string->url (mls-test-doc "other.tm")))
             (document-before mls-current-document)
             (view-before mls-live-view)
             (panel-before (tree->stree (buffer-get-body mls-panel-name)))
             (calls-before (mls-test-count (live-engine-log) "CALL")))
        (load-buffer other-url)
        (switch-to-buffer other-url)
        (mls-check "batch status and formalize cannot replace a live-bound panel"
                   (and (not (mls-show-status))
                        (not (mls-formalize 'replay
                                            (mls-test-dirname
                                             (mls-test-fixture "views/replay.json"))))))
        (mls-check "blocked batch commands preserve live document, view and panel"
                   (and (equal? (url->string mls-current-document)
                                (url->string document-before))
                        (equal? mls-live-view view-before)
                        (equal? (tree->stree (buffer-get-body mls-panel-name)) panel-before)
                        (= calls-before (mls-test-count (live-engine-log) "CALL")))))
      (switch-to-buffer url)
      ;; Add a theorem node while the live connection is active.
      (let* ((body (buffer-get-body url))
             ;; TeXmacs exposes this fixture's body as document -> concat;
             ;; append sibling blocks to the concat, not past document arity.
             (blocks (if (and (= (tree-arity body) 1)
                              (tree-compound? (tree-ref body 0))
                              (eq? (tree-label (tree-ref body 0)) 'concat))
                         (tree-ref body 0)
                         body)))
        (tree-insert! blocks (tree-arity blocks)
                      '((theorem "A theorem added during the live session."))))
      (set! mls-live-dirty? #t)
      (set! live-before-change-text (mls-strip-carriers (mls-body-stree url)))
      (mls-check "live review action waits for the pending source acknowledgement"
                 (and (not (mls-live-review-action
                            "a98f524a-4ad6-4b0d-92d2-15b37d08f555" "confirm" '()))
                      (= (mls-test-count (live-log) "\"method\": \"review\"") 0)))
      (delayed (:pause 180)
        (set! live-new-id (live-new-carrier-id))
        (live-focus-new-block)
        (delayed (:pause 800)
          (mls-check "accepted retry advances the acknowledged revision once"
                     (= mls-live-revision 2))
          (mls-check "review actions wait for the matching live status view"
                     (= mls-live-view-revision mls-live-revision))
          (mls-check "dynamic theorem receives a stable unique carrier"
                     (and (= (length (live-new-carrier-ids)) 1)
                          (live-unique? (map car (mls-carriers)))
                          (live-new-carrier-still-stable?)))
          (mls-check "daemon received exact carrier and the new theorem source"
                     (mls-test-contains? (live-log) live-new-id))
          (mls-check "confirm ignores invalid form fields and sends only the action"
                     (mls-live-review-action
                      "a98f524a-4ad6-4b0d-92d2-15b37d08f555" "confirm"
                      '(("conclusion" . ""))))
          (delayed (:pause 250) (live-finish))))))))

(catch #t
  (lambda () (lazy-plugin-force) (live-run))
  (lambda args
    (display* "ERROR " (object->string args) "\n")
    (display* "SUMMARY pass=" mls-test-pass " fail=" (+ 1 mls-test-fail) "\n")
    (quit-TeXmacs)))
