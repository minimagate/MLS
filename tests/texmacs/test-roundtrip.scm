;;; test-roundtrip.scm -- the sync checks by tree equality, carriers stripped. A document
;;; that TeXmacs cannot save and read back as the same tree (raw data followed by text in
;;; a long-form argument) never reaches the engine, and its file keeps its text. A reload
;;; that changes the text or drops a carrier restores the buffer, puts the file back and fails
;;; the run. A save whose previous text cannot be read writes nothing. A reload that only adds
;;; carriers passes. Everything runs against the stub engine.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(define (rt-fresh name source)
  "Copy the fixture SOURCE to the work file NAME, load it, and return its URL."
  (let ((path (mls-test-doc name)))
    (string-save (mls-test-file-text (mls-test-fixture source)) path)
    (load-buffer (string->url path))
    (string->url path)))

(define (rt-carriers url)
  "Number of mls-id carriers in the buffer of URL (the buffer is made current first)."
  (switch-to-buffer url)
  (length (mls-carriers)))

(define (rt-statuses! name statuses)
  "Write the review-run exit statuses (one per line) to the work file NAME."
  (let ((path (mls-test-doc name)))
    (string-save statuses path)
    (setenv "MLS_STUB_SEQ" path)))

(define (rt-reset-env)
  (setenv "MLS_STUB_SEQ" "")
  (setenv "MLS_STUB_WRITE_IDS_FROM" "")
  (setenv "MLS_STUB_RUN_FROM" "")
  (setenv "MLS_STUB_FAIL" "")
  (setenv "MLS_STUB_VIEW" (mls-test-views "replay.json"))
  (set! mls-last-error #f))

(define (rt-engine-ran?)
  (mls-test-contains? (mls-test-log) "review-run"))

(define (rt-error-has? needle)
  (mls-test-contains? (or mls-last-error "") needle))

;; Read-failure hook: once armed, reads of the work file whose name contains rt-fail-name
;; fail from the read numbered rt-fail-from on (counted from arming). Case 7 fails the save's
;; read of the previous file text (read 2: read 1 is the command's own check). Case 8 fails the
;; read after the engine ran (reads 1-3 are the check, the save's previous text, the sync).
(define rt-real-read-text mls-read-text)
(define rt-fail-name #f)
(define rt-fail-from 0)
(define rt-read-count 0)

(define (rt-faulty-read-text path)
  (cond ((and rt-fail-name (mls-search rt-fail-name path 0))
         (set! rt-read-count (+ rt-read-count 1))
         (if (>= rt-read-count rt-fail-from)
             (error "simulated read failure" path)
             (rt-real-read-text path)))
        (else (rt-real-read-text path))))

(define (rt-arm-read-failure! name from)
  (set! rt-read-count 0)
  (set! rt-fail-from from)
  (set! rt-fail-name name)
  (set! mls-read-text rt-faulty-read-text))

(define (rt-disarm-read-failure!)
  (set! rt-fail-name #f)
  (set! mls-read-text rt-real-read-text))

(mls-test-main
 (lambda ()
   (define work (mls-test-env "MLS_TEST_WORK"))
   (lazy-plugin-force)
   (rt-reset-env)

   ;; 1. The reproducer: TeXmacs loads the text after raw data in a long-form argument as
   ;;    lost, so the saved file would not read back as the buffer. The save is refused
   ;;    before the engine runs; the buffer and the file keep what they had.
   (let* ((url (rt-fresh "raw-run.tm" "raw-data.tm"))
          (doc (mls-test-doc "raw-run.tm"))
          (body-before (mls-test-body-text url))
          (file-before (mls-test-file-text doc))
          (modified-before (buffer-modified? url)))
     (mls-test-clear-log)
     (mls-check "raw data: formalize is refused" (not (mls-formalize 'replay work)))
     (mls-check "raw data: the error says TeXmacs cannot round-trip the document"
                (rt-error-has? "TeXmacs cannot round-trip this document"))
     (mls-check "raw data: the error says MLS did not run" (rt-error-has? "MLS did not run"))
     (mls-check "raw data: the engine is not invoked" (not (rt-engine-ran?)))
     (mls-check-equal "raw data: buffer content intact" body-before (mls-test-body-text url))
     (mls-check-equal "raw data: file on disk unchanged" file-before (mls-test-file-text doc))
     (mls-check-equal "raw data: modified state unchanged" modified-before (buffer-modified? url))
     (mls-check "raw data: show status is refused too" (not (mls-show-status)))
     (mls-check "raw data: show status runs no engine" (not (rt-engine-ran?))))
   (rt-reset-env)

   ;; 2. A normal document still flows: saved, verified, one engine run, four carriers kept.
   (let ((url (rt-fresh "normal.tm" "canonical.tm")))
     (mls-test-clear-log)
     (mls-check "normal document: formalize succeeds" (mls-formalize 'replay work))
     (mls-check-equal "normal document: one engine run" 1 (mls-test-count (mls-test-log) "<review-run>"))
     (mls-check-equal "normal document: four carriers kept" 4 (rt-carriers url)))
   (rt-reset-env)

   ;; 3. A reload that only adds the carrier of a bare block passes.
   (let ((url (rt-fresh "carrier-add.tm" "bare-theorem.tm")))
     (rt-statuses! "carrier-add-seq.txt" "3\n0\n")
     (setenv "MLS_STUB_WRITE_IDS_FROM" (mls-test-fixture "canonical.tm"))
     (mls-check "a reload that only adds a carrier passes" (mls-formalize 'replay work))
     (mls-check-equal "a reload that only adds a carrier leaves no error" #f mls-last-error)
     (mls-check-equal "the added carrier is in the buffer" 4 (rt-carriers url)))
   (rt-reset-env)

   ;; 4. A reload that changes the text restores the buffer, keeps it marked modified, puts
   ;;    the file back to its text from before the command, and fails the run.
   (let* ((url (rt-fresh "text-change.tm" "canonical.tm"))
          (doc (mls-test-doc "text-change.tm"))
          (body-before (mls-test-body-text url)))
     (mls-prepare-document! url)
     (let ((file-before (mls-test-file-text doc)))
       (setenv "MLS_STUB_RUN_FROM" (mls-test-fixture "external.tm"))
       (mls-check "a run whose reload changes the text fails" (not (mls-formalize 'replay work)))
       (mls-check "the error names the text change" (rt-error-has? "changed the document text"))
       (mls-check "the error says the buffer was restored" (rt-error-has? "your buffer was restored"))
       (mls-check "the error says the file was put back" (rt-error-has? "the file was put back"))
       (mls-check-equal "the buffer is restored to its text" body-before (mls-test-body-text url))
       (mls-check-equal "the restored buffer keeps four carriers" 4 (rt-carriers url))
       (mls-check "the restored buffer stays marked modified" (buffer-modified? url))
       (mls-check-equal "the file is put back byte for byte" file-before (mls-test-file-text doc))
       (mls-check "the file has no engine rewrite"
                  (not (mls-test-contains? (mls-test-file-text doc) "EXTERNAL REWRITE")))))
   (rt-reset-env)

   ;; 5. The reviewer's loss: the engine writes raw data followed by text, which TeXmacs
   ;;    reads back without that text. The reload is refused and the buffer keeps its text.
   (let* ((url (rt-fresh "raw-reload.tm" "canonical.tm"))
          (body-before (mls-test-body-text url)))
     (rt-statuses! "raw-reload-seq.txt" "3\n0\n")
     (setenv "MLS_STUB_WRITE_IDS_FROM" (mls-test-fixture "raw-data.tm"))
     (mls-check "a reload that loses text after raw data is refused" (not (mls-formalize 'replay work)))
     (mls-check "the error names the text change" (rt-error-has? "changed the document text"))
     (mls-check-equal "the buffer keeps its text" body-before (mls-test-body-text url))
     (mls-check-equal "the buffer keeps four carriers" 4 (rt-carriers url)))
   (rt-reset-env)

   ;; 6. A reload that keeps the text but drops a carrier is refused.
   (let ((url (rt-fresh "carrier-drop.tm" "canonical.tm")))
     (rt-statuses! "carrier-drop-seq.txt" "3\n0\n")
     (setenv "MLS_STUB_WRITE_IDS_FROM" (mls-test-fixture "bare-theorem.tm"))
     (mls-check "a reload that drops a carrier is refused" (not (mls-formalize 'replay work)))
     (mls-check "the error names the lost carrier" (rt-error-has? "lost mls-id carriers"))
     (mls-check-equal "the buffer keeps four carriers" 4 (rt-carriers url)))
   (rt-reset-env)

   ;; 7. The previous file text cannot be read. The save is refused before TeXmacs writes
   ;;    anything: the file keeps its bytes, the buffer keeps its text and modified state, the
   ;;    error says so, and the engine does not run.
   (let* ((doc (mls-test-doc "unreadable-prev.tm"))
          (url (string->url doc)))
     (string-save (mls-test-file-text (mls-test-fixture "raw-data.tm")) doc)
     (load-buffer url)
     (let ((file-before (mls-test-file-text doc))
           (body-before (mls-test-body-text url))
           (modified-before (buffer-modified? url)))
       (mls-test-clear-log)
       (rt-arm-read-failure! "unreadable-prev.tm" 2)
       (let ((refused (not (mls-formalize 'replay work))))
         (rt-disarm-read-failure!)
         (mls-check "unreadable previous text: formalize is refused" refused))
       (mls-check "unreadable previous text: the error says the previous text could not be read"
                  (rt-error-has? "could not read the previous file text"))
       (mls-check "unreadable previous text: the error says nothing was saved"
                  (rt-error-has? "the document was not saved"))
       (mls-check "unreadable previous text: the engine is not invoked" (not (rt-engine-ran?)))
       (mls-check-equal "unreadable previous text: the file is byte-identical"
                        file-before (mls-test-file-text doc))
       (mls-check-equal "unreadable previous text: the buffer body is unchanged"
                        body-before (mls-test-body-text url))
       (mls-check-equal "unreadable previous text: the modified state is unchanged"
                        modified-before (buffer-modified? url))))
   (rt-reset-env)

   ;; 8. The document file cannot be read after the engine ran. The reload is refused with
   ;;    the read error and nothing is written: the file and the buffer keep their text.
   (let* ((url (rt-fresh "unreadable-after.tm" "canonical.tm"))
          (doc (mls-test-doc "unreadable-after.tm")))
     (mls-prepare-document! url)
     (let ((file-before (mls-test-file-text doc))
           (body-before (mls-test-body-text url)))
       (mls-test-clear-log)
       (rt-arm-read-failure! "unreadable-after.tm" 4)
       (let ((refused (not (mls-formalize 'replay work))))
         (rt-disarm-read-failure!)
         (mls-check "unreadable after the engine ran: formalize is refused" refused))
       (mls-check "unreadable after the engine ran: the engine ran" (rt-engine-ran?))
       (mls-check "unreadable after the engine ran: the error says the file could not be read"
                  (rt-error-has? "could not read the document file"))
       (mls-check-equal "unreadable after the engine ran: the file is unchanged"
                        file-before (mls-test-file-text doc))
       (mls-check-equal "unreadable after the engine ran: the buffer body is unchanged"
                        body-before (mls-test-body-text url))))
   (rt-reset-env)))
