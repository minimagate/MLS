;;; mls-panel.scm -- the MLS status panel (auxiliary buffer tmfs://aux/mls-status)
;;; and the footer badge. Shows the engine view only; computes no statuses.
;;; Document markers are not possible without changing the saved file, so each
;;; block is a row: coloured dot (hover balloon with the message and errors),
;;; label, kind, status, and the actions Go to and Review.
;;; Actions call index-based entry points so no engine text is ever evaluated.

(define mls-current-view #f)

(define (mls-panel-active-view)
  "The live view owns panel actions while a persistent live connection is active."
  (if (and (defined? 'mls-live-connected?) mls-live-connected?
           (defined? 'mls-live-view) mls-live-view)
      mls-live-view
      mls-current-view))

(define (mls-field block key)
  "String field KEY of BLOCK, or \"\" when absent or null."
  (let ((v (mls-json-get block key)))
    (if (string? v) v "")))

(define (mls-count view status)
  (let* ((counts (and view (mls-json-get view "counts")))
         (v (and counts (mls-json-get counts status))))
    (if (number? v) v 0)))

(define (mls-proof-status block)
  (let* ((compact (mls-json-get block "proof_status"))
         (proof (mls-json-get block "proof"))
         (detail (and proof (mls-json-get proof "status")))
         (nested (and detail (mls-json-get detail "status"))))
    (cond ((string? compact) compact)
          ((string? detail) detail)
          ((string? nested) nested)
          (else #f))))

(define (mls-proof-count view status)
  (let* ((counts (and view (mls-json-get view "proof_counts")))
         (known (and counts (mls-json-get counts status)))
         (blocks (and view (mls-json-get view "blocks"))))
    (if (number? known) known
        (if (not (vector? blocks)) 0
            (let loop ((i 0) (n 0))
              (if (= i (vector-length blocks)) n
                  (loop (+ i 1)
                        (+ n (if (equal? (mls-proof-status (vector-ref blocks i)) status) 1 0)))))))))

(define (mls-status-label status)
  (cond ((equal? status "not_formalized") "not formalized")
        ((equal? status "sir_drafted") "SIR drafted")
        ((equal? status "statement_confirmed") "statement confirmed")
        ((equal? status "formalization_error") "formalization error")
        (else status)))

(define (mls-status-color status)
  (cond ((equal? status "not_formalized") "dark grey")
        ((equal? status "sir_drafted") "orange")
        ((equal? status "statement_confirmed") "blue")
        ((equal? status "formalization_error") "red")
        (else "black")))

(define (mls-proof-color status)
  (cond ((equal? status "verified") "green")
        ((equal? status "unproven") "orange")
        ((equal? status "refuted") "red")
        (else #f)))

(define (mls-badge-text view)
  "Footer badge: counts per status, with a rerun note when the engine asks for one."
  (string-append
   "MLS: "
   (number->string (mls-count view "not_formalized")) " not formalized, "
   (number->string (mls-count view "sir_drafted")) " SIR drafted, "
   (number->string (mls-count view "statement_confirmed")) " confirmed, "
   (number->string (mls-count view "formalization_error")) " errors"
   (if (> (+ (mls-proof-count view "verified")
             (mls-proof-count view "unproven")
             (mls-proof-count view "refuted")) 0)
       (string-append "; proofs: " (number->string (mls-proof-count view "verified")) " verified, "
                      (number->string (mls-proof-count view "unproven")) " unproven, "
                      (number->string (mls-proof-count view "refuted")) " refuted")
       "")
   (if (eq? (mls-json-get view "needs_rerun") #t) " (rerun needed)" "")))

(define (mls-balloon-text block)
  (let* ((message (mls-field block "message"))
         (errors (mls-json-get block "errors"))
         (error-lines (if (vector? errors)
                          (map (lambda (e) (if (string? e) (string-append "error: " e) ""))
                               (vector->list errors))
                          '())))
    (mls-join (append (list message) error-lines (mls-decision-lines block)) "\n")))

(define (mls-block-label block)
  (let ((label (mls-json-get block "label")))
    (if (string? label) label (mls-field block "block_id"))))

(define (mls-decision-reason-label reason)
  (cond ((equal? reason "disagrees") "disagrees")
        ((equal? reason "uncertain") "uncertain")
        ((equal? reason "unchecked") "unchecked")
        (else "flagged for review")))

(define (mls-decision-lines block)
  "Human-review hints supplied by the daemon. These lines do not encode status."
  (let* ((flags (mls-json-get block "decision_fields"))
         (outcome (mls-json-get block "decision_differs"))
         (value (and outcome (mls-json-get outcome "value")))
         (probability (and (pair? value) (mls-json-get value "probability")))
         (warning (if (pair? value)
                      (list (string-append
                             "Warning: SIR may differ from source text"
                             (if (number? probability)
                                 (string-append " (probability "
                                                (number->string probability) ")")
                                 "")))
                      '()))
         (field-lines
          (if (vector? flags)
              (map (lambda (flag)
                     (string-append "Review flag: " (mls-field flag "field")
                                    " (" (mls-decision-reason-label
                                          (mls-field flag "reason")) ")"))
                   (vector->list flags))
              '())))
    (append warning field-lines)))

(define (mls-panel-row block index)
  (let* ((status (mls-field block "status"))
         (proof (mls-proof-status block))
         (dot `(with "color" ,(or (mls-proof-color proof) (mls-status-color status)) "●")))
    `(concat (hover-balloon ,dot ,(mls-balloon-text block) "right" "Bottom")
             "  " ,(mls-block-label block)
             "  " ,(mls-field block "kind")
             "  " ,(mls-status-label status)
             ,(if (mls-proof-color proof) (string-append "  proof " proof) "")
             "  " (action "Go to" ,(string-append "(mls-panel-go-to-index " (number->string index) ")"))
             "  " (action "Review" ,(string-append "(mls-panel-review-index " (number->string index) ")")))))

(define (mls-panel-tree view)
  "The panel body as a TeXmacs tree (a Scheme list) for the given protocol view."
  (let* ((blocks (vector->list (mls-json-get view "blocks")))
         (rows (let loop ((bs blocks) (i 0) (acc '()))
                 (if (null? bs)
                     (reverse acc)
                     (loop (cdr bs) (+ i 1) (cons (mls-panel-row (car bs) i) acc))))))
    `(document
      (with "font-series" "bold" "MLS status")
      ,(mls-badge-text view)
      ,@(if (null? rows) (list "No formal blocks in this document.") rows))))

(define (mls-panel-block index)
  (let* ((view (mls-panel-active-view))
         (blocks (and view (mls-json-get view "blocks")))
         (ok (and (vector? blocks) (>= index 0) (< index (vector-length blocks)))))
    (if ok (vector-ref blocks index) #f)))

(define (mls-render-view view)
  "Show VIEW: store it, rebuild the panel, show it, and set the footer badge.
Returns the badge text."
  (set! mls-current-view view)
  (mls-badges-redraw)
  (buffer-set-body mls-panel-name (tm->tree (mls-panel-tree view)))
  (switch-to-buffer (string->url mls-panel-name))
  (let ((badge (mls-badge-text view)))
    (set-message badge "MLS")
    badge))

(tm-define (mls-panel-go-to-index index)
  (:secure #t)
  (let ((block (mls-panel-block index)))
    (if block
        (mls-go-to-block (mls-field block "block_id"))
        (mls-fail "no block at that row; run Show status again"))))

(tm-define (mls-panel-review-index index)
  (:secure #t)
  (let ((block (mls-panel-block index)))
    (if block
        (mls-review-open (mls-field block "block_id"))
        (mls-fail "no block at that row; run Show status again"))))
