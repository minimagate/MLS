;;; test-engine.scm -- the engine layer: every command runs in its own private working
;;; directory (mode 0700, unpredictable name under the TeXmacs home, removed afterwards
;;; on success, failure and error), and the replay directory must be an absolute path.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(define (mls-test-mode-is? path mode)
  "True when the directory PATH has exactly the permission string MODE (e.g. drwx------)."
  (= 0 (system (string-append "test \"$(ls -ld " (mls-sh-quote path) " | cut -c1-10)\" = " (mls-sh-quote mode)))))

(mls-test-main
 (lambda ()
   (define work (mls-test-env "MLS_TEST_WORK"))
   (define base (string-append (mls-test-env "TEXMACS_HOME_PATH") "/system/tmp/mls"))
   (lazy-plugin-force)
   (setenv "MLS_STUB_SEQ" "")
   (setenv "MLS_STUB_WRITE_IDS_FROM" "")
   (setenv "MLS_STUB_RUN_FROM" "")
   (setenv "MLS_STUB_FAIL" "")
   (setenv "MLS_STUB_VIEW" (mls-test-views "replay.json"))
   (set! mls-last-error #f)
   (string-save (mls-test-file-text (mls-test-fixture "canonical.tm")) (mls-test-doc "engine.tm"))
   (load-buffer (string->url (mls-test-doc "engine.tm")))

   ;; 1. a formalize run: the --output directory is private while the engine runs, gone after.
   (mls-test-clear-log)
   (mls-check "run succeeds" (mls-formalize 'replay work))
   (let* ((log (mls-test-log))
          (dir (mls-test-dirname (mls-test-arg-after log "--output"))))
     (mls-check "--output lies in the MLS work base under the TeXmacs home"
                (and (> (string-length dir) (string-length base))
                     (mls-prefix? (string-append base "/") dir)))
     (mls-check "the directory name is unpredictable (not the bare base)" (not (string=? dir base)))
     (mls-check "the directory was mode 0700 while the engine ran"
                (mls-test-contains? log "DIRMODE drwx------"))
     (mls-check "the directory is removed after the run" (not (file-exists? dir)))
     (mls-check "the work base is empty after the run"
                (= 0 (system (string-append "test -z \"$(ls -A " (mls-sh-quote base) ")\"")))))

   ;; 2. the directory is removed when the engine fails.
   (setenv "MLS_STUB_FAIL" "2")
   (mls-test-clear-log)
   (mls-check "failing run fails" (not (mls-formalize 'replay work)))
   (let ((dir (mls-test-dirname (mls-test-arg-after (mls-test-log) "--output"))))
     (mls-check "the directory is removed after a failed run" (not (file-exists? dir))))
   (setenv "MLS_STUB_FAIL" "")

   ;; 3. Show status has its own private directory too.
   (mls-test-clear-log)
   (mls-check "show status succeeds" (mls-show-status))
   (mls-check "show status runs in a mode 0700 directory"
              (mls-test-contains? (mls-test-log) "DIRMODE drwx------"))

   ;; 4. each command gets a different directory.
   (mls-test-clear-log)
   (mls-formalize 'replay work)
   (let ((first (mls-test-dirname (mls-test-arg-after (mls-test-log) "--output"))))
     (mls-test-clear-log)
     (mls-formalize 'replay work)
     (let ((second (mls-test-dirname (mls-test-arg-after (mls-test-log) "--output"))))
       (mls-check "each command gets a new directory" (not (string=? first second)))))

   ;; 5. with-workdir: mode 0700 inside, removed when the body raises an error.
   (let ((seen #f) (mode-ok #f))
     (catch #t
       (lambda ()
         (mls-with-workdir
          (lambda ()
            (set! seen mls-workdir)
            (set! mode-ok (mls-test-mode-is? mls-workdir "drwx------"))
            (error "boom from the body"))))
       (lambda args #t))
     (mls-check "work directory is mode 0700 in the body" mode-ok)
     (mls-check "work directory is removed after an error" (and seen (not (file-exists? seen))))
     (mls-check "no work directory is left current after an error" (not mls-workdir)))

   ;; 6. replay directory must be absolute; a leading dash is never passed to the engine.
   (mls-test-clear-log)
   (mls-check "relative replay directory is refused" (not (mls-formalize 'replay "recordings")))
   (mls-check "the refusal says the directory must be absolute"
              (mls-test-contains? (or mls-last-error "") "absolute"))
   (mls-check "a leading dash is refused" (not (mls-check-replay-dir "-x")))
   (mls-check "an empty replay directory is refused" (not (mls-check-replay-dir "")))
   (mls-check "no engine call for a refused replay directory"
              (not (mls-test-contains? (mls-test-log) "review-run")))
   (mls-check "an absolute replay directory is accepted" (mls-check-replay-dir work))))
