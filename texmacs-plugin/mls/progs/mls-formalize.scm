;;; mls-formalize.scm -- the commands: Formalize document (replay or live) and
;;; Show status. Each command runs in a private working directory and saves the
;;; document before the engine reads it (mls-prepare-document!). A formalize run runs
;;; `mls review-run`, handles the missing_ids retry (exit status 3: `mls --write-ids`,
;;; reload, one retry), reloads when the engine changed the file, and renders the
;;; view. A reload that fails aborts the run. Live runs need the menu toggle; replay
;;; needs an absolute recordings directory.

(define (mls-check-replay-dir dir)
  (cond ((or (not (string? dir)) (string=? (mls-trim dir) ""))
         (mls-fail "choose a recordings directory for replay")
         #f)
        ((not (and (> (string-length dir) 0) (char=? (string-ref dir 0) #\/)))
         (mls-fail "the recordings directory must be an absolute path")
         #f)
        ((not (file-is-directory? dir))
         (mls-fail (string-append "not a directory: " dir))
         #f)
        (else #t)))

(define (mls-review-run-args path live? dir)
  (append (list "review-run" path)
          (if live? (list "--live") (list "--replay" dir))))

(define (mls-batch-panel-available?)
  "Do not let a one-shot command replace the panel owned by a live session."
  (if (and (defined? 'mls-live-connected?) mls-live-connected?)
      (begin
        (mls-fail "stop the MLS live session before running Show status or Formalize")
        #f)
      #t))

(define (mls-write-ids-and-retry url args live?)
  "Exit status 3 (missing_ids): assign ids with --write-ids, reload, retry once."
  (let ((written (mls-engine-call (list "--write-ids" (url->system url)) #f #f)))
    (cond ((not (= (mls-result-status written) 0)) written)
          ((not (mls-reload-document! url (mls-recorded-file-text url)))
           (mls-result 1 #f mls-last-error))
          (else (mls-engine-call args live? #t)))))

(define (mls-formalize mode dir)
  "Formalize the open document. MODE is 'replay (DIR is the recordings directory)
or 'live. Returns #t when a view was rendered, #f after reporting an error."
  (let ((live? (eq? mode 'live)))
    (cond ((and live? (not (mls-live-allowed?)))
           (mls-fail "live model calls are off; enable \"Allow live model calls\" in the MLS menu")
           #f)
          ((not (mls-batch-panel-available?)) #f)
          ((and (not live?) (not (mls-check-replay-dir dir))) #f)
          (else
           (mls-with-workdir
            (lambda ()
              (let ((url (mls-document-url)))
                (and url
                     (mls-prepare-document! url)
                     (mls-run-formalize url live? dir)))))))))

(define (mls-run-formalize url live? dir)
  (let* ((args (mls-review-run-args (url->system url) live? dir))
         (initial (mls-engine-call args live? #t))
         (result (if (= (mls-result-status initial) 3)
                     (mls-write-ids-and-retry url args live?)
                     initial)))
    (cond ((not (mls-result-view result))
           (mls-fail-result result))
          ((not (mls-reload-if-changed! url)) #f)
          (else
           (set! mls-current-document url)
           (mls-render-view (mls-result-view result))
           #t))))

(tm-define (mls-show-status)
  (:synopsis "Show the MLS status of the document")
  (and (mls-batch-panel-available?)
       (mls-with-workdir
        (lambda ()
          (let ((url (mls-document-url)))
            (and url
                 (mls-prepare-document! url)
                 (let ((r (mls-engine-call (list "review-status" (url->system url)) #f #t)))
                   (cond ((mls-result-view r)
                          (set! mls-current-document url)
                          (mls-render-view (mls-result-view r))
                          #t)
                         (else (mls-fail-result r))))))))))

(tm-define (mls-formalize-replay)
  (:synopsis "Formalize the document from recorded replies")
  (:interactive #t)
  (interactive
   (lambda (dir)
     (and (mls-check-replay-dir dir)
          (begin
            (mls-set-replay-dir! dir)
            (mls-formalize 'replay dir))))
   (list "Recordings directory" "string" (mls-replay-dir))))

(tm-define (mls-formalize-live)
  (:synopsis "Formalize the document with live model calls")
  (mls-formalize 'live #f))
