;;; test-formalize.scm -- the Formalize and Show status flows against the stub engine:
;;; the save before each engine command, the missing_ids retry, reloads after engine
;;; changes, the restore when TeXmacs cannot import a reloaded file, the conflict with
;;; an external edit, the file guards, live gating and the empty-import guard.
;;; Each case works on its own copy of a fixture so that no state carries over.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(define (log-line-has? log needles)
  "True when one log line contains every string in NEEDLES."
  (let loop ((ls (mls-split-lines log)))
    (cond ((null? ls) #f)
          ((every-needle? (car ls) needles) #t)
          (else (loop (cdr ls))))))

(define (every-needle? line needles)
  (cond ((null? needles) #t)
        ((mls-search (car needles) line 0) (every-needle? line (cdr needles)))
        (else #f)))

(define (external-path) (mls-test-fixture "external.tm"))

(define (reset-env)
  (setenv "MLS_STUB_SEQ" "")
  (setenv "MLS_STUB_WRITE_IDS_FROM" "")
  (setenv "MLS_STUB_RUN_FROM" "")
  (setenv "MLS_STUB_FAIL" "")
  (setenv "MLS_STUB_VIEW" (mls-test-views "replay.json"))
  (set! mls-last-error #f))

(define (fresh name source)
  "Copy the fixture SOURCE to the work file NAME, load it, and return its URL."
  (let ((path (mls-test-doc name)))
    (string-save (mls-test-file-text (mls-test-fixture source)) path)
    (load-buffer (string->url path))
    (string->url path)))

(define (carrier-count url)
  "Number of mls-id carriers in the buffer of URL (the buffer is made current first)."
  (switch-to-buffer url)
  (length (mls-carriers)))

(define (set-engine-statuses! name statuses)
  "Write the review-run exit statuses (one per line) to the work file NAME."
  (let ((path (mls-test-doc name)))
    (string-save statuses path)
    (setenv "MLS_STUB_SEQ" path)))

(define (theorem-edit! url word)
  (switch-to-buffer url)
  (let ((hit (assoc "e32a226a-c78b-4321-9eba-e14b4c734ed4" (mls-carriers))))
    (tree-set! (cdr hit) 2 (tm->tree (list 'theorem word)))))

(mls-test-main
 (lambda ()
   (define work (mls-test-env "MLS_TEST_WORK"))
   (define log-path (mls-test-env "MLS_STUB_LOG"))
   (lazy-plugin-force)

   ;; 1. replay flow: the document is saved, one review-run with --replay, panel and badge.
   (reset-env)
   (let* ((url (fresh "replay-flow.tm" "canonical.tm"))
          (doc (mls-test-doc "replay-flow.tm")))
     (mls-test-clear-log)
     (mls-check "replay formalize returns success" (mls-formalize 'replay work))
     (let ((log (mls-test-log)))
       (mls-check "replay runs review-run with --replay DIR"
                  (mls-test-contains? log (string-append "<review-run> <" doc "> <--replay> <" work ">")))
       (mls-check "replay runs without MLS_LIVE" (mls-test-contains? log "LIVE=unset"))
       (mls-check "replay adds --output" (mls-test-contains? log "<--output>")))
     (mls-check-equal "replay leaves no error" #f mls-last-error)
     (mls-check-equal "badge after replay"
                      "MLS: 2 not formalized, 1 SIR drafted, 0 confirmed, 1 errors"
                      (mls-badge-text mls-current-view))
     (mls-check "panel buffer holds the status as a snapshot"
                (let ((panel (mls-test-body-text (string->url "tmfs://aux/mls-status"))))
                  (and (mls-test-contains? panel "Snapshot")
                       (mls-test-contains? panel "Definition"))))
     (mls-check "the document stays the current buffer"
                (equal? (url->string (current-buffer)) (url->string url))))

   ;; 2. unsaved edit before the first run (no sync record yet): refused before anything is
   ;;    written, so the edit is not overwritten by MLS. After the user saves in TeXmacs, the
   ;;    run proceeds, keeps the edit, and handles the missing_ids retry.
   (reset-env)
   (let* ((url (fresh "unsaved.tm" "canonical.tm"))
          (doc (mls-test-doc "unsaved.tm")))
     (theorem-edit! url "UNSAVEDEDIT")
     (set-engine-statuses! "seq-unsaved.txt" "3\n0\n")
     (mls-test-clear-log)
     (mls-check "unsaved edit: the first run is refused" (not (mls-formalize 'replay work)))
     (mls-check "unsaved edit: the refusal says to save or revert"
                (mls-test-contains? (or mls-last-error "") "save or revert it in TeXmacs"))
     (mls-check "unsaved edit: the refused run calls no engine"
                (not (mls-test-contains? (mls-test-log) "review-run")))
     (mls-check "unsaved edit: the file on disk is not written"
                (not (mls-test-contains? (mls-test-file-text doc) "UNSAVEDEDIT")))
     (mls-check "unsaved edit: the buffer keeps the edit"
                (mls-test-contains? (mls-test-body-text url) "UNSAVEDEDIT"))
     (mls-check "unsaved edit: saving in TeXmacs succeeds" (not (buffer-save url)))
     (mls-check "unsaved edit: run with missing_ids succeeds" (mls-formalize 'replay work))
     (mls-check "unsaved edit: --write-ids saw the edit on disk"
                (mls-test-contains? (mls-test-file-text (string-append log-path ".write-ids-seen"))
                                    "UNSAVEDEDIT"))
     (mls-check "unsaved edit: buffer keeps the edit after the reload"
                (mls-test-contains? (mls-test-body-text url) "UNSAVEDEDIT"))
     (mls-check "unsaved edit: the file keeps the edit"
                (mls-test-contains? (mls-test-file-text (mls-test-doc "unsaved.tm")) "UNSAVEDEDIT")))
   (reset-env)

   ;; 3. paths with spaces and a single quote reach the engine intact.
   (let ((spaced (mls-test-doc "my docs/it's here.tm")))
     (fresh "my docs/it's here.tm" "canonical.tm")
     (mls-test-clear-log)
     (mls-check "spaced path formalizes" (mls-formalize 'replay work))
     (mls-check "spaced path arrives as one argument"
                (mls-test-contains? (mls-test-log) (string-append "<" spaced ">"))))

   ;; 4. missing_ids: exit 3, --write-ids adds the carrier of the bare block, reload from
   ;;    disk (the text is unchanged, so the reload is verified), one retry.
   (reset-env)
   (let ((url (fresh "missing-ids.tm" "bare-theorem.tm"))
         (doc (mls-test-doc "missing-ids.tm")))
     (set-engine-statuses! "seq-missing.txt" "3\n0\n")
     (setenv "MLS_STUB_WRITE_IDS_FROM" (mls-test-fixture "canonical.tm"))
     (mls-test-clear-log)
     (mls-check "missing_ids flow succeeds on the retry" (mls-formalize 'replay work))
     (let ((log (mls-test-log)))
       (mls-check "write-ids is invoked on the document"
                  (mls-test-contains? log (string-append "<--write-ids> <" doc ">")))
       (mls-check-equal "review-run is retried once" 2 (mls-test-count log "<review-run>")))
     (mls-check-equal "buffer reloaded with the carrier added" 4 (carrier-count url))
     (mls-check "buffer reloaded from the rewritten file"
                (mls-test-contains? (mls-test-body-text url) "Every widget is a widget")))
   (reset-env)

   ;; 5. missing_ids twice: reported as an error, nothing more is attempted. The write-ids
   ;;    rewrite keeps the text, so the first reload is verified.
   (fresh "missing-twice.tm" "canonical.tm")
   (set-engine-statuses! "seq-twice.txt" "3\n3\n")
   (setenv "MLS_STUB_WRITE_IDS_FROM" (mls-test-fixture "canonical.tm"))
   (mls-check "second missing_ids is refused" (not (mls-formalize 'replay work)))
   (mls-check "second missing_ids is reported"
              (mls-test-contains? (or mls-last-error "") "missing_ids"))
   (reset-env)

   ;; 6. engine failure: error shown, no write-ids, body unchanged.
   (let* ((url (fresh "failure.tm" "canonical.tm"))
          (before (mls-test-body-text url)))
     (setenv "MLS_STUB_FAIL" "2")
     (mls-test-clear-log)
     (mls-check "failing engine is reported as failure" (not (mls-formalize 'replay work)))
     (mls-check "failure message names the status" (mls-test-contains? (or mls-last-error "") "status 2"))
     (mls-check "failure message carries the engine message"
                (mls-test-contains? (or mls-last-error "") "stub failed on purpose"))
     (mls-check "failure does not run write-ids" (not (mls-test-contains? (mls-test-log) "--write-ids")))
     (mls-check-equal "failure leaves the body unchanged" before (mls-test-body-text url)))
   (reset-env)

   ;; 7. live: refused while the toggle is off, then MLS_LIVE=1 for the child only.
   (fresh "live.tm" "canonical.tm")
   (mls-test-clear-log)
   (mls-check "live refused while toggle is off" (not (mls-formalize 'live #f)))
   (mls-check "refused live run calls no engine" (not (mls-test-contains? (mls-test-log) "review-run")))
   (mls-check "refusal names the toggle" (mls-test-contains? (or mls-last-error "") "Allow live model calls"))
   (mls-toggle-live)
   (mls-test-clear-log)
   (mls-check "live allowed after toggle" (mls-formalize 'live #f))
   (mls-check "live run passes --live with MLS_LIVE=1 on the same call"
              (log-line-has? (mls-test-log) (list "<review-run>" "<--live>" "LIVE=1")))
   (mls-toggle-live)
   (mls-test-clear-log)
   (mls-check "replay after toggling live off" (mls-formalize 'replay work))
   (mls-check "replay never sets MLS_LIVE" (not (mls-test-contains? (mls-test-log) "LIVE=1")))

   ;; 8. empty-import guard: in-memory body empty, file on disk not: refuse to save.
   (reset-env)
   (let* ((url (fresh "noncanonical-guard.tm" "noncanonical.tm"))
          (file-before (mls-test-file-text (mls-test-doc "noncanonical-guard.tm"))))
     (mls-check "guard fixture loads as an empty body" (mls-body-empty? (mls-body-stree url)))
     (mls-test-clear-log)
     (mls-check "guard refuses the formalize run" (not (mls-formalize 'replay work)))
     (mls-check "guard message names the refusal" (mls-test-contains? (or mls-last-error "") "refusing to save"))
     (mls-check "guard prevents the engine call" (not (mls-test-contains? (mls-test-log) "review-run")))
     (mls-check-equal "guard leaves the file unchanged" file-before
                      (mls-test-file-text (mls-test-doc "noncanonical-guard.tm"))))

   ;; 9. reload guard: a blank file on disk must not replace a non-empty buffer.
   (reset-env)
   (let ((url (fresh "blank-guard.tm" "canonical.tm")))
     (string-save "<TeXmacs|2.1.5>\n\n<style|generic>\n\n<\\body>\n</body>\n" (mls-test-doc "blank-guard.tm"))
     (mls-check "reload refused for a blank file" (not (mls-reload-document! url #f)))
     (mls-check-equal "refused reload keeps the four blocks" 4 (carrier-count url)))

   ;; 10. file guard: text on disk with no body markers is never saved over or reloaded.
   (let ((url (fresh "unknown-guard.tm" "canonical.tm"))
         (junk "not a tm document at all\n"))
     (string-save junk (mls-test-doc "unknown-guard.tm"))
     (mls-test-clear-log)
     (mls-check "reload refused without a body" (not (mls-reload-document! url #f)))
     (mls-check "save refused without a body" (not (mls-prepare-document! url)))
     (mls-check "formalize refused without a body" (not (mls-formalize 'replay work)))
     (mls-check "refused commands run no engine" (not (mls-test-contains? (mls-test-log) "review-run")))
     (mls-check-equal "refused save leaves the file alone" junk
                      (mls-test-file-text (mls-test-doc "unknown-guard.tm")))
     (mls-check-equal "buffer keeps its four blocks" 4 (carrier-count url)))

   ;; 11. missing_ids retry, external non-canonical rewrite: reload fails, buffer restored,
   ;;     file put back to its text before the command, no retry, no success.
   (reset-env)
   (let ((url (fresh "restore-retry.tm" "canonical.tm"))
         (doc (mls-test-doc "restore-retry.tm")))
     (mls-prepare-document! url)
     (let ((file-before (mls-test-file-text doc)))
       (set-engine-statuses! "seq-restore.txt" "3\n0\n")
       (setenv "MLS_STUB_WRITE_IDS_FROM" (mls-test-fixture "external-noncanonical.tm"))
       (mls-test-clear-log)
       (mls-check "non-canonical rewrite during the retry fails the run" (not (mls-formalize 'replay work)))
       (mls-check "the error says the file could not be imported"
                  (mls-test-contains? (or mls-last-error "") "could not be imported"))
       (mls-check "the error says the file was put back"
                  (mls-test-contains? (or mls-last-error "") "the file was put back"))
       (mls-check-equal "no retry after the failed reload" 1 (mls-test-count (mls-test-log) "<review-run>"))
       (mls-check-equal "buffer restored with its four blocks" 4 (carrier-count url))
       (mls-check "buffer restored with its text"
                  (mls-test-contains? (mls-test-body-text url) "A widget is useful"))
       (mls-check "restored buffer stays marked modified" (buffer-modified? url))
       (mls-check-equal "file put back byte for byte" file-before (mls-test-file-text doc))
       (mls-check "file has no engine rewrite"
                  (not (mls-test-contains? (mls-test-file-text doc) "NONCANONICAL REWRITE")))))
   (reset-env)

   ;; 12. the engine rewrites the document into a layout TeXmacs cannot import: the run
   ;;     fails, the buffer is restored, no view is rendered.
   (let ((url (fresh "restore-run.tm" "canonical.tm")))
     (setenv "MLS_STUB_RUN_FROM" (mls-test-fixture "external-noncanonical.tm"))
     (mls-check "post-run non-canonical rewrite fails the run" (not (mls-formalize 'replay work)))
     (mls-check "post-run error says the file could not be imported"
                (mls-test-contains? (or mls-last-error "") "could not be imported"))
     (mls-check-equal "post-run restore keeps the four blocks" 4 (carrier-count url)))
   (reset-env)

   ;; 13. the engine rewrites the document with the carrier added (text unchanged): the
   ;;     reload is verified and the run succeeds.
   (let ((url (fresh "run-rewrite.tm" "bare-theorem.tm")))
     (setenv "MLS_STUB_RUN_FROM" (mls-test-fixture "canonical.tm"))
     (mls-check "post-run carrier-only rewrite reloads and succeeds" (mls-formalize 'replay work))
     (mls-check-equal "buffer shows the engine's carrier" 4 (carrier-count url)))
   (reset-env)

   ;; 14. external text change between two runs, no buffer edits: the reload would change
   ;;     the text, so it is refused, the buffer keeps its text, and no engine call happens.
   (let ((url (fresh "external-change.tm" "canonical.tm"))
         (doc (mls-test-doc "external-change.tm")))
     (mls-check "first run succeeds" (mls-formalize 'replay work))
     (string-save (mls-test-file-text (external-path)) doc)
     (mls-test-clear-log)
     (mls-check "second run after an external text change is refused" (not (mls-formalize 'replay work)))
     (mls-check "the refusal says the text changed"
                (mls-test-contains? (or mls-last-error "") "changed the document text"))
     (mls-check "the refused run calls no engine" (not (mls-test-contains? (mls-test-log) "review-run")))
     (mls-check "buffer keeps its own text" (not (mls-test-contains? (mls-test-body-text url) "EXTERNAL REWRITE")))
     (mls-check-equal "buffer keeps its four carriers" 4 (carrier-count url))
     (mls-check "the external change is still on disk"
                (mls-test-contains? (mls-test-file-text doc) "EXTERNAL REWRITE")))

   ;; 15. external change while the buffer has edits: refused as a conflict, nothing
   ;;     overwritten, no engine call.
   (let ((url (fresh "conflict.tm" "canonical.tm"))
         (doc (mls-test-doc "conflict.tm")))
     (mls-check "first run before the conflict succeeds" (mls-formalize 'replay work))
     (theorem-edit! url "BUFFEREDIT")
     (string-save (mls-test-file-text (external-path)) doc)
     (mls-test-clear-log)
     (mls-check "conflict refused" (not (mls-formalize 'replay work)))
     (mls-check "conflict is reported"
                (mls-test-contains? (or mls-last-error "") "changed on disk"))
     (mls-check "conflict runs no engine" (not (mls-test-contains? (mls-test-log) "review-run")))
     (mls-check "buffer edit is kept" (mls-test-contains? (mls-test-body-text url) "BUFFEREDIT"))
     (mls-check "disk change is kept" (mls-test-contains? (mls-test-file-text doc) "EXTERNAL REWRITE")))
   (reset-env)

   ;; 16. Show status asks the engine for the status of the saved file. With no sync record yet,
   ;;     an unsaved edit is refused first; after the user saves in TeXmacs, it runs.
   (let ((url (fresh "status.tm" "canonical.tm")))
     (theorem-edit! url "STATUSEDIT")
     (mls-test-clear-log)
     (mls-check "show status refuses an unsaved first edit" (not (mls-show-status)))
     (mls-check "show status runs no engine before the save" (not (mls-test-contains? (mls-test-log) "<review-status>")))
     (mls-check "show status: saving in TeXmacs succeeds" (not (buffer-save url)))
     (mls-check "show status succeeds" (mls-show-status))
     (mls-check "show status runs review-status on the document"
                (mls-test-contains? (mls-test-log) (string-append "<review-status> <" (mls-test-doc "status.tm") ">")))
     (mls-check "show status sees the saved edit"
                (mls-test-contains? (mls-test-file-text (mls-test-doc "status.tm")) "STATUSEDIT")))
   (reset-env)))
