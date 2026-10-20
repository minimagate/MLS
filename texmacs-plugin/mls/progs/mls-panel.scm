;;; mls-panel.scm -- the MLS status panel (auxiliary buffer tmfs://aux/mls-status)
;;; and the footer badge. Shows the engine view only; computes no statuses.
;;; Document markers are not possible without changing the saved file, so each
;;; block is a row: coloured dot (hover balloon with the message and errors),
;;; label, kind, status, and the actions Go to and Review.
;;; Actions call index-based entry points so no engine text is ever evaluated.

(define mls-current-view #f)

(define (mls-field block key)
  "String field KEY of BLOCK, or \"\" when absent or null."
  (let ((v (mls-json-get block key)))
    (if (string? v) v "")))

(define (mls-count view status)
  (let* ((counts (and view (mls-json-get view "counts")))
         (v (and counts (mls-json-get counts status))))
    (if (number? v) v 0)))

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

(define (mls-badge-text view)
  "Footer badge: counts per status, with a rerun note when the engine asks for one."
  (string-append
   "MLS: "
   (number->string (mls-count view "not_formalized")) " not formalized, "
   (number->string (mls-count view "sir_drafted")) " SIR drafted, "
   (number->string (mls-count view "statement_confirmed")) " confirmed, "
   (number->string (mls-count view "formalization_error")) " errors"
   (if (eq? (mls-json-get view "needs_rerun") #t) " (rerun needed)" "")))

(define (mls-balloon-text block)
  (let* ((message (mls-field block "message"))
         (errors (mls-json-get block "errors"))
         (error-lines (if (vector? errors)
                          (map (lambda (e) (if (string? e) (string-append "error: " e) ""))
                               (vector->list errors))
                          '())))
    (mls-join (cons message error-lines) "\n")))

(define (mls-block-label block)
  (let ((label (mls-json-get block "label")))
    (if (string? label) label (mls-field block "block_id"))))

(define (mls-panel-row block index)
  (let* ((status (mls-field block "status"))
         (dot `(with "color" ,(mls-status-color status) "●")))
    `(concat (hover-balloon ,dot ,(mls-balloon-text block) "right" "Bottom")
             "  " ,(mls-block-label block)
             "  " ,(mls-field block "kind")
             "  " ,(mls-status-label status)
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
  (let* ((blocks (and mls-current-view (mls-json-get mls-current-view "blocks")))
         (ok (and (vector? blocks) (>= index 0) (< index (vector-length blocks)))))
    (if ok (vector-ref blocks index) #f)))

(define (mls-render-view view)
  "Show VIEW: store it, rebuild the panel, show it, and set the footer badge.
Returns the badge text."
  (set! mls-current-view view)
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
