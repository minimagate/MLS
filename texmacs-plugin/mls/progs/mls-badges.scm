;;; mls-badges.scm -- status pills beside every formal block heading.
;;;
;;; The style package plugins/mls/packages/mls-badges.ts makes render-theorem call
;;; (mls-badge id color) through extern while TeXmacs typesets, so theorem, lemma,
;;; proposition, corollary and definition headings get pills computed from the engine
;;; view. Badges compute no status: they only present the view and the live dirty set.
;;;
;;; The package is attached to the MLS document in memory only. The glue procedures
;;; buffer-save, buffer-export and print-to-file are wrapped so the package is removed
;;; while TeXmacs writes the file, an autosave, an export or a printout, then attached
;;; again; the saved document and its exports never contain it. Live snapshots sent to
;;; the engine are serialized without it (mls-badges-strip-tree).
;;;
;;; Two pill shapes keep the two statuses apart: the statement pill is filled, the
;;; proof pill is outlined. Each pill has an icon and a word, so colour is never the
;;; only signal. Clicking a pill runs an index-free command that takes only a
;;; validated block UUID.

(use-modules (utils library cursor))

(define mls-badges-package "mls-badges")
(define mls-badges-dirty '())      ; alist: block UUID -> time it was first seen dirty
(define mls-badges-ticking? #f)

;; Palettes: (fill line ink) per tone, for light and dark pages.
(define mls-badges-light
  '((neutral "#f1efe8" "#b4b2a9" "#444441")
    (blue "#e6f1fb" "#85b7eb" "#0c447c")
    (amber "#faeeda" "#ef9f27" "#633806")
    (green "#eaf3de" "#97c459" "#27500a")
    (red "#fcebeb" "#f09595" "#791f1f")))

(define mls-badges-dark
  '((neutral "#444441" "#888780" "#d3d1c7")
    (blue "#0c447c" "#378add" "#b5d4f4")
    (amber "#633806" "#ba7517" "#fac775")
    (green "#27500a" "#639922" "#c0dd97")
    (red "#791f1f" "#e24b4a" "#f7c1c1")))

;; Icons, as TeXmacs Unicode escapes. Only glyphs the default sans fonts carry: the
;; heavy cross and warning sign render as their escape text.
(define mls-icon-check "<#2713>")
(define mls-icon-cross "<#00D7>")
(define mls-icon-open "<#25CB>")
(define mls-icon-half "<#25D0>")
(define mls-icon-spin "<#21BB>")
(define mls-icon-warn "!")
(define mls-icon-dots "<#2026>")

;;; Block identity and colour helpers

(define (mls-badge-uuid? s)
  "True for a lowercase or uppercase 8-4-4-4-12 hexadecimal UUID string."
  (and (string? s)
       (= (string-length s) 36)
       (let loop ((i 0))
         (or (= i 36)
             (let ((c (string-ref s i)))
               (and (if (memv i '(8 13 18 23))
                        (char=? c #\-)
                        (or (char-numeric? c)
                            (and (char-ci>=? c #\a) (char-ci<=? c #\f))))
                    (loop (+ i 1))))))))

(define (mls-badge-string t)
  (let ((s (cond ((string? t) t)
                 ((tree? t) (tree->stree t))
                 (else #f))))
    (if (string? s) s "")))

(define (mls-badge-hex-value s)
  (let ((n (string->number s 16)))
    (if n n 0)))

(define (mls-badge-dark-ink? color)
  "True when COLOR, the text colour at the badge, is light: the page is dark."
  (cond ((and (= (string-length color) 7) (char=? (string-ref color 0) #\#))
         (let ((r (mls-badge-hex-value (substring color 1 3)))
               (g (mls-badge-hex-value (substring color 3 5)))
               (b (mls-badge-hex-value (substring color 5 7))))
           (> (+ (* 299 r) (* 587 g) (* 114 b)) 128000)))
        ((member color '("white" "light grey" "pastel grey")) #t)
        (else #f)))

(define (mls-badge-tone palette tone)
  (cdr (assq tone palette)))

;;; Choosing the pills of a block (pure: no view lookup, no time)

(define (mls-badge-view-block id)
  (let* ((view (mls-panel-active-view))
         (blocks (and view (mls-json-get view "blocks"))))
    (and (vector? blocks)
         (let loop ((i 0))
           (cond ((= i (vector-length blocks)) #f)
                 ((equal? (mls-field (vector-ref blocks i) "block_id") id)
                  (vector-ref blocks i))
                 (else (loop (+ i 1))))))))

(define (mls-badge-differs? block)
  (let* ((outcome (mls-json-get block "decision_differs"))
         (value (and (pair? outcome) (mls-json-get outcome "value"))))
    (pair? value)))

(define (mls-badge-elapsed-label seconds)
  (cond ((not (number? seconds)) "")
        ((< seconds 2) "")
        ((< seconds 60) (string-append " " (number->string seconds) "s"))
        (else (string-append " " (number->string (quotient seconds 60)) "m"))))

(define (mls-badge-specs block working elapsed)
  "The pills for BLOCK as (shape tone icon label action) lists. WORKING is true when
the live engine is processing the block; ELAPSED is its age in seconds or #f. SHAPE
is filled or outline; ACTION is open or retry."
  (let* ((status (mls-field block "status"))
         (review (mls-field block "review"))
         (proof (mls-proof-status block))
         (busy (mls-badge-elapsed-label elapsed))
         (statement
          (cond ((and working (not (equal? status "statement_confirmed")))
                 (list 'filled 'blue mls-icon-spin
                       (string-append "formalizing" mls-icon-dots busy) 'open))
                ((equal? status "not_formalized")
                 (list 'filled 'neutral mls-icon-open "not formalized" 'open))
                ((equal? status "sir_drafted")
                 (if (member review '("confirmed" "edited"))
                     (list 'filled 'amber mls-icon-half "waiting on dependencies" 'open)
                     (list 'filled 'amber mls-icon-half "review draft" 'open)))
                ((equal? status "statement_confirmed")
                 (list 'filled 'green mls-icon-check "statement" 'open))
                ((equal? status "formalization_error")
                 (list 'filled 'red mls-icon-cross "error" 'open))
                (else #f)))
         (proof-pill
          (cond ((and working (equal? status "statement_confirmed"))
                 (list 'outline 'blue mls-icon-spin
                       (string-append "proving" mls-icon-dots busy) 'open))
                ((equal? proof "verified")
                 (list 'outline 'green mls-icon-check "proof verified" 'open))
                ((equal? proof "unproven")
                 (list 'outline 'neutral "?" "proof open" 'open))
                ((equal? proof "refuted")
                 (list 'outline 'red mls-icon-cross "refuted" 'open))
                (else #f)))
         (differs (and (mls-badge-differs? block)
                       (list 'filled 'amber mls-icon-warn "may differ" 'open)))
         (rerun (and (eq? (mls-json-get block "needs_rerun") #t) (not working)
                     (list 'filled 'amber mls-icon-spin "rerun" 'retry))))
    (filter (lambda (x) x) (list statement proof-pill differs rerun))))

(define (mls-badge-pending-specs)
  "Pills for a block the live engine has not reported yet."
  (list (list 'filled 'neutral mls-icon-dots "queued" 'open)))

;;; Rendering

(define (mls-badge-command action id)
  (string-append (if (eq? action 'retry) "(mls-badge-retry " "(mls-badge-open ")
                 "\"" id "\")"))

(define (mls-badge-pill spec id palette)
  (let* ((shape (car spec))
         (colors (mls-badge-tone palette (cadr spec)))
         (fill (if (eq? shape 'outline) "" (car colors)))
         (line (cadr colors))
         (ink (caddr colors))
         (label (string-append (caddr spec) " " (cadddr spec))))
    `(mls-chip ,fill ,line ,ink
               (action ,label ,(mls-badge-command (list-ref spec 4) id)))))

(define (mls-badge-render specs id dark?)
  (if (null? specs)
      ""
      (let ((palette (if dark? mls-badges-dark mls-badges-light)))
        `(concat (hspace "0.5spc")
                 ,@(apply append
                          (map (lambda (spec)
                                 (list (mls-badge-pill spec id palette) '(hspace "0.35fn")))
                               specs))
                 (hspace "0.4spc")))))

(define (mls-badge-working id)
  (assoc id mls-badges-dirty))

(define (mls-badge-live?)
  (and (defined? 'mls-live-connected?) mls-live-connected?))

(tm-define (mls-badge id-tree color-tree)
  (:secure #t)
  (catch #t
    (lambda ()
      (let ((id (mls-badge-string id-tree))
            (dark? (mls-badge-dark-ink? (mls-badge-string color-tree))))
        (if (not (mls-badge-uuid? id))
            ""
            (let* ((block (mls-badge-view-block id))
                   (working (mls-badge-working id))
                   (elapsed (and working (- (current-time) (cdr working)))))
              (cond (block (mls-badge-render (mls-badge-specs block working elapsed) id dark?))
                    ((and (mls-badge-live?) (mls-panel-active-view))
                     (mls-badge-render (mls-badge-pending-specs) id dark?))
                    (else ""))))))
    (lambda args "")))

;;; Pill actions

(tm-define (mls-badge-open id)
  (:secure #t)
  (if (mls-badge-uuid? id)
      (mls-review-open id)
      (mls-fail "not an MLS block")))

(tm-define (mls-badge-retry id)
  (:secure #t)
  (cond ((not (mls-badge-uuid? id)) (mls-fail "not an MLS block"))
        ((and (mls-badge-live?) (defined? 'mls-live-retry)) (mls-live-retry id))
        (else (mls-review-open id))))

;;; Live progress

(define (mls-badges-set-dirty! ids)
  "Record the live engine's dirty block set, keeping the start time of blocks that
were already dirty."
  (let ((now (current-time)))
    (set! mls-badges-dirty
          (map (lambda (id)
                 (or (assoc id mls-badges-dirty) (cons id now)))
               (filter mls-badge-uuid? ids))))
  (mls-badges-start-ticking))

(define (mls-badges-clear-dirty!)
  (set! mls-badges-dirty '()))

(define (mls-badges-start-ticking)
  "While blocks are being processed, redraw every few seconds so elapsed times move."
  (when (and (pair? mls-badges-dirty) (not mls-badges-ticking?))
    (set! mls-badges-ticking? #t)
    (delayed (:pause 3000) (mls-badges-tick))))

(define (mls-badges-tick)
  (set! mls-badges-ticking? #f)
  (when (and (pair? mls-badges-dirty) (mls-badge-live?))
    (catch #t (lambda () (mls-badges-redraw)) (lambda args #f))
    (mls-badges-start-ticking)))

;;; Attaching the package in memory

(define (mls-badges-attached? url)
  "Asks the editor: buffer-get still reports the style the buffer was loaded with."
  (and url (buffer-exists? url)
       (with-buffer url
         (and (member mls-badges-package (get-style-list)) #t))))

(define (mls-badges-strip-tree doc)
  "DOC (a whole-document tree) with the badge package removed from its style list."
  (let ((stree (tree->stree doc)))
    (if (not (and (pair? stree) (assq 'style (cdr stree))))
        doc
        (tm->tree
         (cons (car stree)
               (map (lambda (x)
                      (if (and (pair? x) (eq? (car x) 'style) (pair? (cdr x))
                               (pair? (cadr x)) (eq? (car (cadr x)) 'tuple))
                          (list 'style
                                (cons 'tuple
                                      (filter (lambda (s) (not (equal? s mls-badges-package)))
                                              (cdr (cadr x)))))
                          x))
                    (cdr stree)))))))

(define (mls-badges-quietly thunk)
  (if (defined? 'mls-live-with-suppressed-notifications)
      (mls-live-with-suppressed-notifications thunk)
      (thunk)))

(define (mls-badges-set-modified! url modified?)
  (if modified? (buffer-pretend-modified url) (buffer-pretend-saved url)))

(define (mls-badges-change-style! url add?)
  "Add or remove the package on URL's buffer, keeping its modified flag."
  (let ((modified? (buffer-modified? url)))
    (mls-badges-quietly
     (lambda ()
       (with-buffer url
         (if add?
             (set-style-list (append (get-style-list) (list mls-badges-package)))
             (set-style-list (filter (lambda (s) (not (equal? s mls-badges-package)))
                                     (get-style-list)))))))
    (mls-badges-set-modified! url modified?)))

(define (mls-badges-attach! url)
  (when (and url (buffer-exists? url) (not (mls-badges-attached? url)))
    (mls-badges-change-style! url #t)))

(define (mls-badges-detach! url)
  (when (mls-badges-attached? url)
    (mls-badges-change-style! url #f)))

(define (mls-badges-without url thunk)
  "Run THUNK with the package removed from URL's buffer, then attach it again. The
buffer's modified flag after THUNK is kept, so a save still reads as saved."
  (if (not (mls-badges-attached? url))
      (thunk)
      (let ((result #f))
        (mls-badges-detach! url)
        (dynamic-wind
          (lambda () #t)
          (lambda () (set! result (thunk)))
          (lambda ()
            (when (buffer-exists? url)
              (let ((modified? (buffer-modified? url)))
                (mls-badges-change-style! url #t)
                (mls-badges-set-modified! url modified?)))))
        result)))

;; Wrap the glue procedures that write a buffer out. A second load of this module keeps
;; the originals captured by the first, so the wrappers never nest.
(define mls-badges-glue-buffer-save
  (if (defined? 'mls-badges-glue-buffer-save) mls-badges-glue-buffer-save buffer-save))
(define mls-badges-glue-buffer-export
  (if (defined? 'mls-badges-glue-buffer-export) mls-badges-glue-buffer-export buffer-export))
(define mls-badges-glue-print-to-file
  (if (defined? 'mls-badges-glue-print-to-file) mls-badges-glue-print-to-file print-to-file))

(set! buffer-save
      (lambda (url)
        (mls-badges-without url (lambda () (mls-badges-glue-buffer-save url)))))
(set! buffer-export
      (lambda (url to fm)
        (mls-badges-without url (lambda () (mls-badges-glue-buffer-export url to fm)))))
(set! print-to-file
      (lambda (name)
        (mls-badges-without (current-buffer)
                            (lambda () (mls-badges-glue-print-to-file name)))))

;;; Redrawing

(define (mls-badges-document)
  (cond ((and (mls-badge-live?) (defined? 'mls-live-url) mls-live-url) mls-live-url)
        (else mls-current-document)))

(define (mls-badges-redraw)
  "Attach the package to the MLS document and retypeset it when it is on screen."
  (let ((url (mls-badges-document)))
    (when (and url (buffer-exists? url))
      (mls-badges-attach! url)
      (when (equal? (url->string (current-buffer)) (url->string url))
        (update-current-buffer)))))
