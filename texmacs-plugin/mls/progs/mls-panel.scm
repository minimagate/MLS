;;; mls-panel.scm -- the body of the MLS side panel (kept in the auxiliary buffer
;;; tmfs://aux/mls-status, shown beside the document by mls-ui.scm) and the footer
;;; badge. Shows the engine view only; computes no statuses. Each block is a row: its
;;; title (hover for the message and errors), the same pills as in the document, a
;;; detail line, and the actions Go to, Review and, when useful, Retry.
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

;;; Panel body. Structure blocks MLS does not formalize (sections, proofs) are hidden
;;; while they have no status. Blocks that need a person come first.

(define mls-panel-quiet-kinds
  '("section" "subsection" "subsubsection" "paragraph" "proof" "commentary"))

(define (mls-panel-shown? block)
  (not (and (member (mls-field block "kind") mls-panel-quiet-kinds)
            (equal? (mls-field block "status") "not_formalized"))))

(define (mls-panel-attention? block)
  (let ((status (mls-field block "status")))
    (or (and (equal? status "sir_drafted")
             (not (member (mls-field block "review") '("confirmed" "edited"))))
        (equal? status "formalization_error")
        (eq? (mls-json-get block "needs_rerun") #t)
        (mls-badge-differs? block)
        (equal? (mls-proof-status block) "refuted"))))

(define (mls-kind-title kind)
  (if (> (string-length kind) 0)
      (string-append (string (char-upcase (string-ref kind 0)))
                     (substring kind 1 (string-length kind)))
      "Block"))

(define (mls-panel-title block)
  (let ((label (mls-json-get block "label")))
    (if (and (string? label) (> (string-length label) 0))
        (string-append (mls-kind-title (mls-field block "kind")) "  " label)
        (mls-kind-title (mls-field block "kind")))))

(define (mls-panel-detail block)
  "One short line: the first error, else the engine message."
  (let ((errors (mls-json-get block "errors")))
    (if (and (vector? errors) (> (vector-length errors) 0) (string? (vector-ref errors 0)))
        (vector-ref errors 0)
        (mls-field block "message"))))

(define (mls-panel-retry? block)
  (or (eq? (mls-json-get block "needs_rerun") #t)
      (equal? (mls-field block "status") "formalization_error")))

(define (mls-panel-row block index)
  "Title (hover for messages and errors), pills, a detail line and the row actions."
  (let* ((id (mls-field block "block_id"))
         (working (and (mls-badge-uuid? id) (mls-badge-working id)))
         (elapsed (and working (- (current-time) (cdr working))))
         (i (number->string index))
         (detail (mls-panel-detail block)))
    (append
     (list `(hover-balloon (strong ,(mls-panel-title block)) ,(mls-balloon-text block)
                           "right" "Bottom")
           `(concat ,@(mls-badge-pills (mls-badge-specs block working elapsed) #f)))
     (if (> (string-length detail) 0) (list `(with "font-size" "0.84" ,detail)) '())
     (list `(concat (action "Go to" ,(string-append "(mls-panel-go-to-index " i ")"))
                    "   " (action "Review" ,(string-append "(mls-panel-review-index " i ")"))
                    ,@(if (mls-panel-retry? block)
                          `("   " (action "Retry" ,(string-append "(mls-panel-retry-index " i ")")))
                          '()))
           ""))))

(define (mls-panel-summary shown)
  "Counts of the shown blocks as plain pills."
  (let* ((count (lambda (pred) (length (filter (lambda (b) (pred (car b))) shown))))
         (status-is (lambda (s) (lambda (b) (equal? (mls-field b "status") s))))
         (proof-is (lambda (s) (lambda (b) (equal? (mls-proof-status b) s))))
         (items (filter (lambda (x) (> (car x) 0))
                        (list (list (count (status-is "statement_confirmed")) 'green 'filled "confirmed")
                              (list (count (status-is "sir_drafted")) 'amber 'filled "to review")
                              (list (count (status-is "formalization_error")) 'red 'filled "errors")
                              (list (count (status-is "not_formalized")) 'neutral 'filled "not formalized")
                              (list (count (proof-is "verified")) 'green 'outline "proved")
                              (list (count (proof-is "unproven")) 'neutral 'outline "proof open")
                              (list (count (proof-is "refuted")) 'red 'outline "refuted")))))
    `(concat ,@(mls-badge-pills
                (map (lambda (x)
                       (list (caddr x) (cadr x) (number->string (car x)) (cadddr x) 'open))
                     items)
                #f))))

(define (mls-panel-section title rows)
  (if (null? rows)
      '()
      (cons `(strong ,title)
            (apply append (map (lambda (b) (mls-panel-row (car b) (cdr b))) rows)))))

(define (mls-panel-tree view)
  "The panel body as a TeXmacs tree (a Scheme list) for VIEW, which may be #f."
  (let* ((blocks (if (and view (vector? (mls-json-get view "blocks")))
                     (vector->list (mls-json-get view "blocks"))
                     '()))
         (indexed (let loop ((bs blocks) (i 0) (acc '()))
                    (if (null? bs)
                        (reverse acc)
                        (loop (cdr bs) (+ i 1) (cons (cons (car bs) i) acc)))))
         (shown (filter (lambda (b) (mls-panel-shown? (car b))) indexed))
         (attention (filter (lambda (b) (mls-panel-attention? (car b))) shown))
         (others (filter (lambda (b) (not (mls-panel-attention? (car b)))) shown))
         (notices (append (mls-ui-problem-card) (mls-ui-error-banner)
                          (mls-ui-diagnostics-lines))))
    `(document
      ,@(mls-ui-header)
      ""
      ,@(if (null? notices) '() (append notices (list "")))
      ,@(cond ((not view)
               (list "No results yet."
                     "Start MLS to check the theorems, lemmas and definitions of this document."))
              ((null? shown)
               (list "No formal blocks in this document."
                     "Write a theorem, lemma or definition: MLS picks it up as you type."))
              (else
               (append (list (mls-panel-summary shown) "")
                       (mls-panel-section "Needs your attention" attention)
                       (mls-panel-section (if (null? attention) "Blocks" "Other blocks")
                                          others)))))))

(define (mls-panel-block index)
  (let* ((view (mls-panel-active-view))
         (blocks (and view (mls-json-get view "blocks")))
         (ok (and (vector? blocks) (>= index 0) (< index (vector-length blocks)))))
    (if ok (vector-ref blocks index) #f)))

(define (mls-render-view view)
  "Show VIEW from a one-shot command: store it as a snapshot, redraw the pills, show
the panel beside the document, and set the footer badge. Returns the badge text."
  (set! mls-current-view view)
  (mls-ui-view-updated!)
  (set! mls-ui-problem #f)
  (set! mls-ui-phase 'snapshot)
  (mls-badges-redraw)
  (mls-ui-show-panel)
  (let ((badge (mls-badge-text view)))
    (set-message badge "MLS")
    badge))

(tm-define (mls-panel-go-to-index index)
  (:secure #t)
  (let ((block (mls-panel-block index)))
    (if block
        (mls-go-to-block (mls-field block "block_id"))
        (mls-fail "no block at that row; refresh the MLS panel"))))

(tm-define (mls-panel-review-index index)
  (:secure #t)
  (let ((block (mls-panel-block index)))
    (if block
        (mls-review-open (mls-field block "block_id"))
        (mls-fail "no block at that row; refresh the MLS panel"))))

(tm-define (mls-panel-retry-index index)
  (:secure #t)
  (let ((block (mls-panel-block index)))
    (if block
        (mls-badge-retry (mls-field block "block_id"))
        (mls-fail "no block at that row; refresh the MLS panel"))))
