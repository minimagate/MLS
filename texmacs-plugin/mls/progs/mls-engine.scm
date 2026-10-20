;;; mls-engine.scm -- runs the mls engine as a child process and reads its JSON.
;;;
;;; The engine binary is MLS_BIN, else the "mls-engine" preference, else `mls`
;;; on PATH. Every argument is shell-quoted. Each user command (Formalize, Show
;;; status, a review action) runs inside one private working directory created by
;;; mktemp -d under $TEXMACS_HOME_PATH/system/tmp/mls (mode 0700, unpredictable name).
;;; stdout, stderr, the --output JSON and any SIR file live there, and the directory
;;; is removed when the command ends, on success or failure. Results are plain lists
;;; (status view error); the plugin never evaluates engine output.
;;; Live model calls: MLS_LIVE=1 is set only for the child process of a live run,
;;; and only after the user allowed live model calls. That permission is remembered in
;;; the "mls-live-consent" preference and can be revoked in the MLS panel settings.

(use-modules (ice-9 popen))

(define mls-last-error #f)
(define mls-workdir #f)      ; private directory of the command in progress, or #f
(define mls-file-counter 0)

(define (mls-engine-path)
  (let ((env (getenv "MLS_BIN"))
        (pref (get-preference "mls-engine")))
    (cond ((and (string? env) (> (string-length env) 0)) env)
          ((and (string? pref) (> (string-length pref) 0)) pref)
          (else "mls"))))

(define (mls-live-allowed?)
  "True after the user allowed live model calls. Off until then; remembered after."
  (equal? (get-preference "mls-live-consent") "on"))

(define (mls-set-live-allowed! on?)
  (set-preference "mls-live-consent" (if on? "on" "off")))

(define (mls-toggle-live)
  (mls-set-live-allowed! (not (mls-live-allowed?))))

(define (mls-replay-dir)
  (let ((dir (get-preference "mls-replay-dir")))
    (if (string? dir) dir "")))

(define (mls-set-replay-dir! dir)
  (set-preference "mls-replay-dir" dir))

(define (mls-prefix? prefix s)
  (and (>= (string-length s) (string-length prefix))
       (string=? (substring s 0 (string-length prefix)) prefix)))

(define (mls-workdir-base)
  (string-append (getenv "TEXMACS_HOME_PATH") "/system/tmp/mls"))

(define (mls-ensure-dir dir)
  (if (not (file-exists? dir))
      (mkdir dir #o700)))

(define (mls-make-workdir)
  "A new private directory for one command, created by mktemp -d (mode 0700, unique
name, created exclusively). Returns its path, or #f."
  (let ((base (mls-workdir-base)))
    (and (catch #t
           (lambda ()
             (mls-ensure-dir (string-append (getenv "TEXMACS_HOME_PATH") "/system/tmp"))
             (mls-ensure-dir base)
             #t)
           (lambda args #f))
         (let* ((port (open-input-pipe
                       (string-append "mktemp -d "
                                      (mls-sh-quote (string-append base "/work.XXXXXXXXXX")))))
                (line (read-line port)))
           (close-pipe port)
           (and (string? line)
                (mls-prefix? (string-append base "/") line)
                (file-is-directory? line)
                line)))))

(define (mls-remove-workdir dir)
  "Remove DIR (only when it lies under the MLS work base)."
  (if (and dir (mls-prefix? (string-append (mls-workdir-base) "/") dir))
      (system (string-append "rm -rf -- " (mls-sh-quote dir)))))

(define (mls-with-workdir thunk)
  "Run THUNK with a fresh private working directory for one command. The directory is
removed afterwards, also when THUNK fails. Returns THUNK's value, or #f when no
directory could be created."
  (let ((dir (mls-make-workdir)))
    (cond ((not dir)
           (mls-fail "could not create a private temporary directory")
           #f)
          (else
           (set! mls-workdir dir)
           (let ((result (catch #t
                           (lambda () (thunk))
                           (lambda (key . args)
                             (mls-remove-workdir dir)
                             (set! mls-workdir #f)
                             (apply throw key args)))))
             (mls-remove-workdir dir)
             (set! mls-workdir #f)
             result)))))

(define (mls-scratch-path tag)
  "A fresh file name inside the current command's private directory."
  (set! mls-file-counter (+ mls-file-counter 1))
  (string-append mls-workdir "/" tag "-" (number->string mls-file-counter)))

(define (mls-read-text path)
  "Contents of PATH as a string. Raises an error when PATH is missing or cannot be read,
so that an unreadable file is never mistaken for an empty one."
  (if (file-exists? path)
      (catch #t
             (lambda () (string-load path))
             (lambda args (error "MLS cannot read the file" path)))
      (error "MLS cannot read the file" path)))

(define (mls-remove-file path)
  (if (file-exists? path)
      (catch #t (lambda () (delete-file path)) (lambda args #f))))

(define (mls-shell-command args live?)
  "The shell line for ARGS. MLS_LIVE is set for this command only, or unset."
  (string-append (if live? "MLS_LIVE=1 " "unset MLS_LIVE; ")
                 (mls-sh-quote (mls-engine-path))
                 (apply string-append
                        (map (lambda (a) (string-append " " (mls-sh-quote a))) args))))

(define (mls-result status view error)
  (list status view error))
(define (mls-result-status r) (car r))
(define (mls-result-view r) (cadr r))
(define (mls-result-error r) (caddr r))

(define (mls-view? v)
  "A protocol-1 view: has a blocks array. Error objects are not views."
  (and (pair? v)
       (eqv? (mls-json-get v "protocol") 1)
       (vector? (mls-json-get v "blocks"))))

(define (mls-first-line s)
  (let ((lines (mls-non-blank-lines s)))
    (if (null? lines) "" (car lines))))

(define (mls-failure-text status parsed stderr)
  (let ((message (and parsed (mls-json-get parsed "message")))
        (code (and parsed (mls-json-get parsed "error"))))
    (string-append
     "engine exited with status " (number->string status)
     (cond ((and (string? code) (string? message)) (string-append " (" code "): " message))
           ((string? message) (string-append ": " message))
           ((> (string-length (mls-first-line stderr)) 0)
            (string-append ": " (mls-first-line stderr)))
           (else ""))
     (if (= status 127) "; set MLS_BIN or put mls on PATH" ""))))

(define (mls-engine-call args live? output?)
  "Run the engine with ARGS inside the current command's private directory. When
OUTPUT? is true the engine writes its JSON to a fresh --output file there. Must be
called inside mls-with-workdir. Returns (mls-result status view error): view is the
parsed protocol view on success, or #t when no output was requested and the command
succeeded."
  (cond
   ((not mls-workdir) (mls-result 1 #f "internal: no working directory for the engine"))
   (else
    (let* ((stdout-file (mls-scratch-path "stdout"))
           (stderr-file (mls-scratch-path "stderr"))
           (output (and output? (mls-scratch-path "output.json")))
           (all-args (if output (append args (list "--output" output)) args))
           (status (system (string-append (mls-shell-command all-args live?)
                                          " > " (mls-sh-quote stdout-file)
                                          " 2> " (mls-sh-quote stderr-file))))
           ;; stderr only adds detail to a failure message; the exit status is still reported
           ;; when it cannot be read.
           (stderr (catch #t (lambda () (mls-read-text stderr-file)) (lambda args "")))
           (parsed (and output (file-exists? output)
                        (catch #t (lambda () (mls-json-parse (mls-read-text output)))
                               (lambda args #f)))))
      (cond ((and (= status 0) (not output))
             (mls-result 0 #t #f))
            ((and (= status 0) (mls-view? parsed))
             (mls-result 0 parsed #f))
            ((= status 0)
             (mls-result 0 #f "engine finished but wrote no readable view"))
            (else
             (mls-result status #f (mls-failure-text status parsed stderr))))))))

(define (mls-fail message)
  "Show MESSAGE as a user-visible error and return #f."
  (set! mls-last-error message)
  (set-message (string-append "MLS: " message) "MLS error")
  (when (defined? 'mls-ui-error!) (mls-ui-error! message))
  #f)

(define (mls-fail-result r)
  (mls-fail (mls-result-error r)))
