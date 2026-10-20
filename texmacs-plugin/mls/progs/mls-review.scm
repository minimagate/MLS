;;; mls-review.scm -- review cards for SIR confirmation: confirm, reject, edit.
;;;
;;; All logic lives in plain procedures (mls-review-action, mls-sir-from-fields)
;;; so it can be tested headless. Widget buttons only send a message to
;;; mls-review-dispatch. An edit writes a complete SIR JSON file to a temp path and
;;; passes it to `mls review-action --sir`.

(define (mls-view-block uuid)
  "The block with block_id UUID in the current view, or #f."
  (and mls-current-view
       (let loop ((bs (vector->list (mls-json-get mls-current-view "blocks"))))
         (cond ((null? bs) #f)
               ((equal? (mls-field (car bs) "block_id") uuid) (car bs))
               (else (loop (cdr bs)))))))

(define (mls-sir-strings sir key)
  (let ((v (mls-json-get sir key)))
    (if (vector? v)
        (map (lambda (x) (if (string? x) x "")) (vector->list v))
        '())))

(define (mls-sir-string sir key)
  (let ((v (mls-json-get sir key)))
    (if (string? v) v "")))

(define (mls-variable-lines sir)
  (map (lambda (var)
         (string-append (mls-field var "name") " : " (mls-field var "type")))
       (let ((v (mls-json-get sir "variables")))
         (if (vector? v) (vector->list v) '()))))

(define (mls-check-line check)
  (string-append "  " (mls-field check "field") ": " (mls-field check "state")
                 " [" (mls-field check "scope") "] " (mls-field check "detail")))

(define (mls-card-lines block)
  (let* ((sir (mls-json-get block "sir"))
         (checks (mls-json-get block "checks"))
         (errors (mls-json-get block "errors"))
         (lean (mls-json-get block "lean_statement")))
    (append
     (list (string-append "Status: " (mls-status-label (mls-field block "status"))
                          "   Review: " (mls-field block "review"))
           (string-append "Kind: " (mls-field block "kind")
                          "   Label: " (mls-block-label block))
           (mls-field block "message")
           "")
     (if (pair? sir)
         (append
          (list "Variables:")
          (map (lambda (l) (string-append "  " l)) (mls-variable-lines sir))
          (list "Hypotheses:")
          (map (lambda (h) (string-append "  - " h)) (mls-sir-strings sir "hypotheses"))
          (list (string-append "Conclusion: " (mls-sir-string sir "conclusion"))
                "Assumptions made:")
          (map (lambda (a) (string-append "  - " a)) (mls-sir-strings sir "assumptions_made"))
          (list (string-append "Uses: " (mls-join (mls-sir-strings sir "uses") ", "))))
         (list "No SIR yet."))
     (list "" "Structure checks:")
     (if (vector? checks)
         (map mls-check-line (vector->list checks))
         (list "  none"))
     (list "" "Lean statement:"
           (if (string? lean) (string-append "  " lean) "  none"))
     (if (vector? errors)
         (map (lambda (e) (if (string? e) (string-append "error: " e) "")) (vector->list errors))
         '()))))

(define (mls-card-document block)
  "The read-only card body as a TeXmacs document tree."
  (cons 'document (mls-card-lines block)))

(define (mls-variables-text sir)
  (mls-join (mls-variable-lines sir) "\n"))

(define (mls-field-text fields name)
  (let ((hit (assoc name fields)))
    (if (and hit (string? (cdr hit))) (cdr hit) "")))

(define (mls-parse-variable line)
  "A 'name : type' line as an object, or #f when malformed."
  (let* ((colon (string-index line #\:))
         (name (and colon (mls-trim (substring line 0 colon))))
         (type (and colon (mls-trim (substring line (+ colon 1) (string-length line))))))
    (and name type
         (> (string-length name) 0)
         (> (string-length type) 0)
         (list (cons "name" name) (cons "type" type)))))

(define (mls-parse-variables text)
  "Vector of variable objects from the 'name : type' lines of TEXT, or #f on bad input."
  (let loop ((ls (mls-non-blank-lines text)) (acc '()))
    (cond ((null? ls) (list->vector (reverse acc)))
          ((mls-parse-variable (car ls))
           => (lambda (var) (loop (cdr ls) (cons var acc))))
          (else (mls-fail (string-append "bad variable line, use 'name : type': " (car ls)))
                #f))))

(define (mls-alist-set alist key value)
  (cond ((null? alist) (list (cons key value)))
        ((equal? (caar alist) key) (cons (cons key value) (cdr alist)))
        (else (cons (car alist) (mls-alist-set (cdr alist) key value)))))

(define (mls-sir-from-fields base fields)
  "BASE (the block's SIR alist) with the edited fields replaced, or #f after an error.
Keys not in the form (kind, label, uses, ...) are kept from BASE."
  (let ((variables (mls-parse-variables (mls-field-text fields "variables")))
        (conclusion (mls-trim (mls-field-text fields "conclusion"))))
    (cond ((not variables) #f)
          ((string=? conclusion "")
           (mls-fail "the conclusion is empty")
           #f)
          (else
           (let loop ((sir base)
                      (updates
                       (list (cons "variables" variables)
                             (cons "hypotheses"
                                   (list->vector (mls-non-blank-lines
                                                  (mls-field-text fields "hypotheses"))))
                             (cons "conclusion" conclusion)
                             (cons "assumptions_made"
                                   (list->vector (mls-non-blank-lines
                                                  (mls-field-text fields "assumptions")))))))
             (if (null? updates)
                 sir
                 (loop (mls-alist-set sir (caar updates) (cdar updates)) (cdr updates))))))))

(define (mls-run-review uuid action extra)
  "Run `mls review-action` for UUID and ACTION (plus EXTRA arguments) on the saved
document, then re-render. Must run inside mls-with-workdir."
  (let* ((url mls-current-document)
         (args (append (list "review-action" (url->system url) "--block" uuid "--action" action)
                       extra)))
    (and (mls-prepare-document! url)
         (let ((r (mls-engine-call args #f #t)))
           (cond ((mls-result-view r)
                  (and (mls-reload-if-changed! url)
                       (begin (mls-render-view (mls-result-view r)) #t)))
                 (else (mls-fail-result r)))))))

(define (mls-edit-action uuid fields)
  (let* ((block (mls-view-block uuid))
         (base (and block (mls-json-get block "sir"))))
    (cond ((not (pair? base)) (mls-fail "this block has no SIR to edit"))
          ((mls-sir-from-fields base fields)
           => (lambda (sir)
                (let* ((text (mls-json-encode sir))
                       (path (mls-scratch-path "sir.json")))
                  (cond ((not (catch #t (lambda () (mls-json-parse text) #t) (lambda args #f)))
                         (mls-fail "the edited SIR is not valid JSON"))
                        (else
                         (string-save text path)
                         (mls-run-review uuid "edit" (list "--sir" path)))))))
          (else #f))))

(define (mls-review-action uuid action fields)
  "Apply ACTION (confirm, reject or edit) to block UUID. FIELDS is the edit form
as an alist of name to string (edit only). Returns #t on success."
  (cond ((not mls-current-document)
         (mls-fail "no document is loaded in MLS; run Show status first"))
        (else
         (mls-with-workdir
          (lambda ()
            (cond ((member action '("confirm" "reject"))
                   (mls-run-review uuid action '()))
                  ((equal? action "edit") (mls-edit-action uuid fields))
                  (else (mls-fail (string-append "unknown review action " action)))))))))

(define (mls-review-dispatch msg)
  "Entry point for the card buttons. MSG is a list whose first element names the button."
  (let ((kind (car msg)))
    (cond ((equal? kind "close") #t)
          ((equal? kind "edit-open") (mls-review-open-edit (cadr msg)))
          ((equal? kind "edit") (mls-review-action (cadr msg) "edit" (caddr msg)))
          (else (mls-review-action (cadr msg) kind '())))))

(define (mls-form-alist)
  (map cons (form-fields) (form-values)))

(tm-widget ((mls-review-card-widget block) cmd)
  (padded
    (texmacs-output (mls-card-document block) '(style "generic"))
    (bottom-buttons
      ("Confirm" (cmd (list "confirm" (mls-field block "block_id"))))
      ("Reject" (cmd (list "reject" (mls-field block "block_id"))))
      ("Edit" (cmd (list "edit-open" (mls-field block "block_id"))))
      ("Close" (cmd (list "close"))))))

(tm-widget ((mls-review-edit-widget block) cmd)
  (padded
    (form "mls-edit"
      (aligned
        (item (text "Variables (name : type, one per line)")
              (form-input "variables" "text"
                          (list (mls-variables-text (mls-json-get block "sir"))) "30em"))
        (item (text "Hypotheses (one per line)")
              (form-input "hypotheses" "text"
                          (list (mls-join (mls-sir-strings (mls-json-get block "sir") "hypotheses") "\n"))
                          "30em"))
        (item (text "Conclusion")
              (form-input "conclusion" "string"
                          (list (mls-sir-string (mls-json-get block "sir") "conclusion")) "30em"))
        (item (text "Assumptions made (one per line)")
              (form-input "assumptions" "text"
                          (list (mls-join (mls-sir-strings (mls-json-get block "sir") "assumptions_made") "\n"))
                          "30em"))
        (item (text "Uses (read-only)")
              (text (mls-join (mls-sir-strings (mls-json-get block "sir") "uses") ", ")))))
    (bottom-buttons
      ("Save edit" (cmd (list "edit" (mls-field block "block_id") (mls-form-alist))))
      ("Cancel" (cmd (list "close"))))))

(define (mls-review-open uuid)
  "Show the review card for block UUID. Returns #t, or #f after reporting an error."
  (let ((block (mls-view-block uuid)))
    (cond (block
           (dialogue-window (mls-review-card-widget block)
                            (lambda (msg) (mls-review-dispatch msg))
                            "MLS review")
           #t)
          (else (mls-fail "block not found in the current status view")))))

(define (mls-review-open-edit uuid)
  (let ((block (mls-view-block uuid)))
    (cond ((and block (pair? (mls-json-get block "sir")))
           (dialogue-window (mls-review-edit-widget block)
                            (lambda (msg) (mls-review-dispatch msg))
                            "MLS edit SIR")
           #t)
          (block (mls-fail "this block has no SIR to edit"))
          (else (mls-fail "block not found in the current status view")))))
