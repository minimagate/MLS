;;; test-real-review.scm -- one end-to-end M3 test: the TeXmacs plugin drives the REAL
;;; `mls` engine headless through its own procedures (Formalize by replay, the review
;;; actions confirm and reject) against the pinned local Lean runtime.
;;;
;;; Gated: runs only when MLS_LEAN_TESTS=1 and run.sh found a real engine (MLS_REAL_BIN,
;;; set when target/debug/mls exists and advertises review-status). Otherwise it prints
;;; SKIP and passes. The engine is reached through a small logging wrapper that execs the
;;; real binary, so the argument lists and MLS_LIVE of every call are on record. Every
;;; document is a copy in this run's temporary directory; the recordings are a copy of
;;; tests/fixtures/recordings/pipeline/success, deleted before the review actions, which
;;; must not need them.
;;;

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(define real-thm-first "00000000-0000-4000-8000-000000000001")
(define real-thm-second "00000000-0000-4000-8000-000000000002")

(define (real-repo)
  (string-append (mls-test-env "MLS_TEST_DIR") "/../.."))

(define (real-base) (string-append (mls-test-env "MLS_TEST_WORK") "/real"))

(define (real-log-path) (string-append (real-base) "/engine.log"))

(define (real-log)
  (if (file-exists? (real-log-path)) (string-load (real-log-path)) ""))

(define (real-lines-with needle)
  "The engine log lines that contain NEEDLE."
  (let loop ((ls (mls-split-lines (real-log))) (acc '()))
    (cond ((null? ls) (reverse acc))
          ((mls-test-contains? (car ls) needle) (loop (cdr ls) (cons (car ls) acc)))
          (else (loop (cdr ls) acc)))))

(define (real-every-line? lines pred)
  (cond ((null? lines) #t)
        ((pred (car lines)) (real-every-line? (cdr lines) pred))
        (else #f)))

(define (real-write-wrapper! path)
  "An sh wrapper that logs its arguments and MLS_LIVE, then execs the real engine."
  (string-save
   (string-append
    "#!/bin/sh\n"
    "{ printf 'CALL'; for a in \"$@\"; do printf ' <%s>' \"$a\"; done; "
    "printf ' LIVE=%s\\n' \"${MLS_LIVE-unset}\"; } >> '" (real-log-path) "'\n"
    "exec '" (getenv "MLS_REAL_BIN") "' \"$@\"\n")
   path)
  (system (string-append "chmod 755 '" path "'")))

(define (real-body-of text)
  "The text from the body marker on, or #f when there is none."
  (let ((open (mls-search "<\\body>" text 0)))
    (and open (substring text open (string-length text)))))

(define (real-canonical text)
  "TEXT with the canonical TeXmacs layout: a blank line after the header and after
<style|generic>. The body, and so every block and its mls-id carrier, is unchanged.
The repository fixture has no blank lines, and TeXmacs imports it with an empty body."
  (let ((body (real-body-of text)))
    (if body
        (string-append "<TeXmacs|2.1.4>\n\n<style|generic>\n\n" body)
        text)))

(define (real-setup! name)
  "A canonical copy of the success document and a copy of its recordings under
real/NAME. Returns (document-path . recordings-path)."
  (let* ((dir (string-append (real-base) "/" name))
         (doc (string-append dir "/doc.tm"))
         (rec (string-append dir "/rec"))
         (source (string-append (real-repo) "/tests/fixtures/lean/pipeline/success.tm")))
    (system (string-append "mkdir -p '" dir "'"))
    (string-save (real-canonical (mls-test-file-text source)) doc)
    (system (string-append "cp -R '" (real-repo) "/tests/fixtures/recordings/pipeline/success' '" rec "'"))
    (cons doc rec)))

(define (real-status uuid)
  (let ((b (mls-view-block uuid))) (if b (mls-field b "status") "missing")))

(define (real-message uuid)
  (let ((b (mls-view-block uuid))) (if b (mls-field b "message") "")))

(define (real-view-blocks-count)
  (let ((bs (and mls-current-view (mls-json-get mls-current-view "blocks"))))
    (if (vector? bs) (vector-length bs) -1)))

(define (real-block-report)
  "One line per block of the current view: label, status, message and errors."
  (let ((bs (and mls-current-view (mls-json-get mls-current-view "blocks"))))
    (if (vector? bs)
        (mls-join (map (lambda (b) (string-append "  " (mls-block-label b) ": " (mls-balloon-text b)))
                       (vector->list bs))
                  "\n")
        "  no view")))

(define (real-drafted-both?)
  (and (= (real-view-blocks-count) 2)
       (equal? (real-status real-thm-first) "sir_drafted")
       (equal? (real-status real-thm-second) "sir_drafted")))

(define (real-carrier-snapshot url)
  "The mls-id carriers of the buffer at URL as (uuid . text) pairs. The buffer is made
current first: rendering a view switches to the status panel."
  (switch-to-buffer url)
  (map (lambda (hit) (cons (car hit) (object->string (tree->stree (cdr hit)))))
       (mls-carriers)))

(define (real-open path)
  (load-buffer (string->url path)))

(define (real-check-document-intact name path url before)
  "The file on disk keeps both carriers and the block text; the buffer carriers match BEFORE."
  (let ((text (mls-test-file-text path)))
    (mls-check (string-append name ": file keeps the first mls-id carrier")
               (mls-test-contains? text (string-append "mls-id|" real-thm-first)))
    (mls-check (string-append name ": file keeps the second mls-id carrier")
               (mls-test-contains? text (string-append "mls-id|" real-thm-second)))
    (mls-check (string-append name ": file keeps the first block text")
               (mls-test-contains? text "Truth holds."))
    (mls-check (string-append name ": file keeps the second block text")
               (mls-test-contains? text "Truth still holds by "))
    (mls-check (string-append name ": buffer carriers are identical to the start")
               (equal? before (real-carrier-snapshot url)))
    (mls-check (string-append name ": .mls/review store is written next to the document")
               (file-exists? (string-append (mls-test-dirname path) "/.mls/review/doc.tm.store.json")))))

(define real-ready
  (let ((bin (getenv "MLS_REAL_BIN")))
    (and (equal? (getenv "MLS_LEAN_TESTS") "1")
         bin
         (not (string=? bin ""))
         (file-exists? bin))))

(if (not real-ready)
    (begin
      (display* "SKIP real review: needs MLS_LEAN_TESTS=1 and target/debug/mls (make test-texmacs builds it)\n")
      (display* "SUMMARY pass=0 fail=0\n"))
    (mls-test-main
     (lambda ()
       (lazy-plugin-force)
       (setenv "MLS_STUB_FAIL" "")
       (setenv "MLS_STUB_SEQ" "")
       (setenv "MLS_STUB_RUN_FROM" "")
       (setenv "MLS_STUB_WRITE_IDS_FROM" "")
       (system (string-append "mkdir -p '" (real-base) "'"))
       (real-write-wrapper! (string-append (real-base) "/mls-wrap.sh"))
       (setenv "MLS_BIN" (string-append (real-base) "/mls-wrap.sh"))
       (set! mls-last-error #f)

       ;; 1-4. Formalize by replay, then confirm thm:second alone, then thm:first.
       (let* ((setup (real-setup! "main"))
              (doc (car setup))
              (rec (cdr setup))
              (url (string->url doc)))
         (mls-check "replay recordings are copied" (file-exists? (string-append rec "/call-0003.json")))
         (mls-check "document copy has the fixture body unchanged"
                    (equal? (real-body-of (mls-test-file-text doc))
                            (real-body-of (mls-test-file-text (string-append (real-repo) "/tests/fixtures/lean/pipeline/success.tm")))))
         (real-open doc)
         (let ((before (real-carrier-snapshot url)))
           (mls-check "formalize by replay succeeds" (mls-formalize 'replay rec))
           (mls-check "review-run replays the recordings without MLS_LIVE"
                      (and (pair? (real-lines-with (string-append "<review-run> <" doc "> <--replay> <" rec ">")))
                           (pair? (real-lines-with "LIVE=unset"))))
           (mls-check-equal "view lists two blocks" 2 (real-view-blocks-count))
           (cond
            ((not (real-drafted-both?))
             (display* "  block report after the replay run:\n" (real-block-report) "\n")
             (mls-check "replay drafts both blocks (review steps skipped; see the block report)" #f))
            (else
             (mls-check-equal "first block is SIR drafted" "sir_drafted" (real-status real-thm-first))
             (mls-check-equal "second block is SIR drafted" "sir_drafted" (real-status real-thm-second))
             (mls-check-equal "counts: two SIR drafted" 2 (mls-count mls-current-view "sir_drafted"))
             (mls-check-equal "badge after the replay run"
                              "MLS: 0 not formalized, 2 SIR drafted, 0 confirmed, 0 errors"
                              (mls-badge-text mls-current-view))
             (mls-check "status panel lists both labels"
                        (let ((panel (mls-test-body-text (string->url mls-panel-name))))
                          (and (mls-test-contains? panel "thm:first")
                               (mls-test-contains? panel "thm:second"))))

             ;; The review actions must not need the recordings: remove them first.
             (system (string-append "rm -rf '" rec "'"))
             (mls-check "replay directory is gone before the review actions" (not (file-exists? rec)))

             (mls-check "confirm of thm:second succeeds" (mls-review-action real-thm-second "confirm" '()))
             (mls-check "thm:second waits for its dependency"
                        (mls-test-contains? (real-message real-thm-second) "waiting for dependency confirmation"))
             (mls-check-equal "thm:second is still SIR drafted" "sir_drafted" (real-status real-thm-second))
             (mls-check-equal "no block confirmed yet" 0 (mls-count mls-current-view "statement_confirmed"))

             (mls-check "confirm of thm:first succeeds" (mls-review-action real-thm-first "confirm" '()))
             (mls-check-equal "thm:first is statement_confirmed" "statement_confirmed" (real-status real-thm-first))
             (mls-check-equal "thm:second is statement_confirmed" "statement_confirmed" (real-status real-thm-second))
             (mls-check-equal "counts: two confirmed" 2 (mls-count mls-current-view "statement_confirmed"))
             (mls-check-equal "counts: none drafted" 0 (mls-count mls-current-view "sir_drafted"))
             (mls-check-equal "badge after the confirmations"
                              "MLS: 0 not formalized, 0 SIR drafted, 2 confirmed, 0 errors"
                              (mls-badge-text mls-current-view))
             (let ((actions (real-lines-with "<review-action>")))
               (mls-check-equal "two review-action calls reached the engine" 2 (length actions))
               (mls-check "review-action has no --replay or --live flag"
                          (real-every-line? actions
                                            (lambda (l) (not (or (mls-test-contains? l "<--replay>")
                                                                 (mls-test-contains? l "<--live>"))))))
               (mls-check "review-action runs without MLS_LIVE"
                          (real-every-line? actions
                                            (lambda (l) (mls-test-contains? l "LIVE=unset")))))
             (real-check-document-intact "after confirm" doc url before))))

       ;; 5. Reject one block in a fresh copy: not formalized, rerun needed.
       (let* ((setup (real-setup! "reject"))
              (doc (car setup))
              (rec (cdr setup))
              (url (string->url doc)))
         (real-open doc)
         (let ((before (real-carrier-snapshot url)))
           (cond
            ((not (and (mls-formalize 'replay rec) (real-drafted-both?)))
             (display* "  block report of the fresh copy:\n" (real-block-report) "\n")
             (mls-check "fresh copy drafts both blocks by replay (review steps skipped)" #f))
            (else
             (mls-check "fresh copy drafts both blocks by replay" #t)
             (mls-check "reject of thm:first succeeds" (mls-review-action real-thm-first "reject" '()))
             (mls-check-equal "rejected block is not formalized" "not_formalized" (real-status real-thm-first))
             (mls-check-equal "the other block stays SIR drafted" "sir_drafted" (real-status real-thm-second))
             (mls-check-equal "needs_rerun is shown" #t (mls-json-get mls-current-view "needs_rerun"))
             (mls-check-equal "badge says a rerun is needed"
                              "MLS: 1 not formalized, 1 SIR drafted, 0 confirmed, 0 errors (rerun needed)"
                              (mls-badge-text mls-current-view))
             (real-check-document-intact "after reject" doc url before)))))))))
