;;; common.scm -- helpers shared by the headless TeXmacs test files.
;;; run.sh runs each test file in its own TeXmacs process with a fresh
;;; TEXMACS_HOME_PATH. A test prints one PASS or FAIL line per check, and
;;; SUMMARY pass=N fail=M at the end. An uncaught error prints ERROR.

(define mls-test-pass 0)
(define mls-test-fail 0)

(define (mls-test-env name)
  (or (getenv name) (error "missing test environment variable" name)))

(define (mls-check name ok)
  (if ok
      (begin (set! mls-test-pass (+ mls-test-pass 1))
             (display* "PASS " name "\n"))
      (begin (set! mls-test-fail (+ mls-test-fail 1))
             (display* "FAIL " name "\n"))))

(define (mls-check-equal name expected actual)
  (if (equal? expected actual)
      (mls-check name #t)
      (begin
        (mls-check name #f)
        (display* "  expected: " (object->string expected) "\n")
        (display* "  actual:   " (object->string actual) "\n"))))

(define (mls-test-main body)
  (catch #t
    (lambda ()
      (body)
      (display* "SUMMARY pass=" mls-test-pass " fail=" mls-test-fail "\n"))
    (lambda args
      (display* "ERROR " (object->string args) "\n")
      (display* "SUMMARY pass=" mls-test-pass " fail=" (+ 1 mls-test-fail) "\n"))))

(define (mls-test-doc name)
  (string-append (mls-test-env "MLS_TEST_WORK") "/" name))

(define (mls-test-open name)
  (load-buffer (string->url (mls-test-doc name))))

(define (mls-test-log)
  (let ((path (mls-test-env "MLS_STUB_LOG")))
    (if (file-exists? path) (string-load path) "")))

(define (mls-test-clear-log)
  (let ((path (mls-test-env "MLS_STUB_LOG")))
    (if (file-exists? path) (delete-file path))))

(define (mls-test-contains? text needle)
  (if (mls-search needle text 0) #t #f))

(define (mls-test-fixture name)
  (string-append (mls-test-env "MLS_TEST_FIXTURES") "/" name))

(define (mls-test-views name)
  (string-append (mls-test-env "MLS_TEST_FIXTURES") "/views/" name))

(define (mls-test-file-text path)
  (string-load path))

(define (mls-test-count text needle)
  "Number of non-overlapping occurrences of NEEDLE in TEXT."
  (let loop ((start 0) (n 0))
    (let ((hit (mls-search needle text start)))
      (if hit (loop (+ hit (string-length needle)) (+ n 1)) n))))

(define (mls-test-body-text url)
  "The body of the buffer at URL as stree text (for substring checks)."
  (object->string (tree->stree (buffer-get-body url))))

(define (mls-test-arg-after log flag)
  "The argument that follows FLAG in the first stub log line that has it, or #f."
  (let ((hit (mls-search (string-append "<" flag "> <") log 0)))
    (and hit
         (let* ((start (+ hit (string-length flag) 4))
                (end (mls-search ">" log start)))
           (and end (substring log start end))))))

(define (mls-test-dirname path)
  (let loop ((i (- (string-length path) 1)))
    (cond ((< i 0) ".")
          ((char=? (string-ref path i) #\/) (substring path 0 i))
          (else (loop (- i 1))))))
