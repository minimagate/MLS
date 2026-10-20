;;; mls-live.scm -- persistent JSON-RPC client for `mls serve`.
;;;
;;; TeXmacs frames connection output, so this module only accepts text carried by
;;; document/concat/text nodes. The server is trusted to emit verbatim: ASCII JSON;
;;; no reply text is evaluated as Scheme.

(use-modules (part part-shared)
             (utils plugins plugin-eval)
             (utils library cursor))

(define mls-live-language "mls")
(define mls-live-connection-session "default")
(define mls-live-url #f)
(define mls-live-document #f)
(define mls-live-revision 0)
(define mls-live-pending-open #f)
(define mls-live-pending-change #f)
(define mls-live-request-id 0)
(define mls-live-requests '())
(define mls-live-source #f)
(define mls-live-dirty? #f)
(define mls-live-cursor-dirty? #t)
(define mls-live-active #f)
(define mls-live-connected? #f)
(define mls-live-notifications '())
(define mls-live-suppress? #f)
(define mls-live-incoming "")
(define mls-live-view #f)
(define mls-live-view-revision 0)
(define mls-live-last-error #f)

(define (mls-live-pad-hex n)
  (let ((s (number->string n 16)))
    (string-append (make-string (max 0 (- 4 (string-length s))) #\0) s)))

(define (mls-live-ascii text)
  "Return an ASCII-only string, escaping every non-ASCII character as JSON Unicode."
  (apply string-append
         (map (lambda (c)
                (let ((cp (char->integer c)))
                  (cond ((< cp 128) (string c))
                        ((<= cp #xffff)
                         (string-append "\\u" (mls-live-pad-hex cp)))
                        (else
                         (let* ((n (- cp #x10000))
                                (hi (+ #xd800 (quotient n 1024)))
                                (lo (+ #xdc00 (remainder n 1024))))
                           (string-append "\\u" (mls-live-pad-hex hi)
                                          "\\u" (mls-live-pad-hex lo)))))))
              (string->list text))))

(define (mls-live-encode object)
  (mls-live-ascii (mls-json-encode object)))

(define (mls-live-launcher)
  (string-append "env MLS_LIVE=1 "
                 (mls-sh-quote (or (getenv "MLS_LIVE_BIN") (mls-engine-path)))
                 " serve --live"))

(plugin-configure mls
  (:require #t)
  (:launch ,(mls-live-launcher)))

(define (mls-live-current-document?)
  (and mls-live-url
       (equal? (url->string (current-buffer)) (url->string mls-live-url))))

(define (mls-live-active-block)
  (and (mls-live-current-document?)
       (let ((carrier (tree-innermost mls-carrier? #t)))
         (and carrier
              (>= (tree-arity carrier) 3)
              (let ((uuid (tree->stree (tree-ref carrier 1))))
                (and (string? uuid) uuid))))))

(define (mls-live-with-suppressed-notifications thunk)
  (let ((previous mls-live-suppress?))
    (dynamic-wind
      (lambda () (set! mls-live-suppress? #t))
      thunk
      (lambda () (set! mls-live-suppress? previous)))))

(define (mls-live-with-focus thunk)
  (let ((focus (current-buffer)))
    (dynamic-wind
      (lambda () #t)
      thunk
      (lambda ()
        (unless (equal? (url->string (current-buffer)) (url->string focus))
          (switch-to-buffer focus))))))

(define mls-live-formal-labels '(definition theorem lemma proposition corollary))

(define (mls-live-formal? tree)
  (and (tree-compound? tree) (memq (tree-label tree) mls-live-formal-labels)))

(define (mls-live-base62-value text)
  (let loop ((chars (string->list text)) (value 0))
    (if (null? chars) value
        (let* ((c (car chars))
               (digit (cond ((and (char>=? c #\0) (char<=? c #\9))
                             (- (char->integer c) (char->integer #\0)))
                            ((and (char>=? c #\A) (char<=? c #\Z))
                             (+ 10 (- (char->integer c) (char->integer #\A))))
                            ((and (char>=? c #\a) (char<=? c #\z))
                             (+ 36 (- (char->integer c) (char->integer #\a))))
                            (else (error "unexpected TeXmacs unique-id character" c)))))
          (loop (cdr chars) (+ (* value 62) digit))))))

(define (mls-live-hex-pad value width)
  (let ((text (number->string value 16)))
    (string-append (make-string (max 0 (- width (string-length text))) #\0) text)))

(define (mls-live-fresh-id)
  "Encode TeXmacs' unique token as a UUID accepted by the live document parser."
  (let* ((token (create-unique-id))
         (seed (mls-live-base62-value (substring token 1 (string-length token))))
         (value (modulo seed (- (expt 2 122) 1)))
         (low-90 (modulo value (expt 2 90)))
         (low-74 (modulo low-90 (expt 2 74)))
         (low-62 (modulo low-74 (expt 2 62)))
         (high-32 (quotient value (expt 2 90)))
         (next-16 (quotient low-90 (expt 2 74)))
         (next-12 (quotient low-74 (expt 2 62)))
         (next-14 (quotient low-62 (expt 2 48)))
         (low-48 (modulo low-62 (expt 2 48))))
    (string-append (mls-live-hex-pad high-32 8) "-"
                   (mls-live-hex-pad next-16 4) "-"
                   (mls-live-hex-pad (+ #x4000 next-12) 4) "-"
                   (mls-live-hex-pad (+ #x8000 next-14) 4) "-"
                   (mls-live-hex-pad low-48 12))))

(define (mls-live-tree-carrier-ids root)
  (let walk ((tree root))
    (if (not (tree-compound? tree)) '()
        (append (if (mls-carrier? tree)
                    (list (tree->stree (tree-ref tree 1)))
                    '())
                (apply append (map walk (tree-children tree)))))))

(define (mls-live-assign-ids! url)
  "Wrap uncarried formal nodes in mls-id carriers. Existing carriers are boundaries."
  (mls-live-with-suppressed-notifications
   (lambda ()
     (let ((root (buffer-get url))
           (used (mls-live-tree-carrier-ids (buffer-get url)))
           (added 0))
       (define (wrap-child! parent index child)
         (let* ((id (let retry ((candidate (mls-live-fresh-id)))
                      (if (member candidate used)
                          (retry (mls-live-fresh-id))
                          candidate)))
                (carrier (tm->tree `(with "mls-id" ,id ,(tree->stree child)))))
           (tree-set! parent index carrier)
           (set! used (cons id used))
           (set! added (+ added 1))))
       (define (walk parent)
         (when (tree-compound? parent)
           (let loop ((index 0))
             (when (< index (tree-arity parent))
               (let ((child (tree-ref parent index)))
                 (cond ((mls-carrier? child) #t)
                       ((mls-live-formal? child)
                        (wrap-child! parent index child))
                       ((tree-compound? child) (walk child)))
                 (loop (+ index 1)))))))
       (walk root)
       added))))

(define (mls-live-fresh-source url)
  "Assign IDs to new formal blocks, then serialize the full in-memory buffer,
including its TeXmacs header/style but not the in-memory badge package."
  (mls-live-assign-ids! url)
  (serialize-texmacs (mls-badges-strip-tree (buffer-get url))))

(define (mls-live-next-id)
  (set! mls-live-request-id (+ mls-live-request-id 1))
  mls-live-request-id)

(define (mls-live-send method params . before-write)
  (let* ((id (mls-live-next-id))
         (request `(("jsonrpc" . "2.0") ("id" . ,id)
                    ("method" . ,method) ("params" . ,params)))
         (text (string-append (mls-live-encode request) "\n")))
    (set! mls-live-requests
          (cons (cons id (list method mls-live-document
                               (mls-json-get params "revision")))
                mls-live-requests))
    ;; Install pending state before writing in case a fast child replies through
    ;; connection-notify before this function returns.
    (when (pair? before-write) ((car before-write) id))
    (connection-write-string mls-live-language mls-live-connection-session text)
    id))

(define (mls-live-open)
  (let ((url mls-live-url))
    (and url
         (begin
           (set! mls-live-revision 0)
           (set! mls-live-source #f)
           (set! mls-live-pending-open #f)
           (set! mls-live-pending-change #f)
           (set! mls-live-dirty? #f)
           (set! mls-live-cursor-dirty? #t)
           (buffer-attach-notifier url)
           (let* ((source (mls-live-fresh-source url))
                  (revision 1)
                  (id (mls-live-send "didOpen"
                                     `(("document" . ,mls-live-document)
                                       ("revision" . ,revision)
                                       ("source" . ,source)
                                       ("active" . ,(or (mls-live-active-block) 'null))))))
             (set! mls-live-pending-open (list id revision source))
             id)))))

(define (mls-live-persist-existing-ids! url)
  "Run the guarded save, --write-ids and verified reload sequence before didOpen."
  (mls-live-with-focus
   (lambda ()
     (mls-with-workdir
      (lambda ()
        (and (mls-prepare-document! url)
             (let ((written (mls-engine-call (list "--write-ids" (url->system url)) #f #f)))
               (cond ((not (= (mls-result-status written) 0))
                      (mls-fail-result written))
                     ((not (mls-reload-document! url (mls-recorded-file-text url))) #f)
                     (else #t)))))))))

(tm-define (mls-live-start)
  (:synopsis "Start or connect the MLS live session")
  (cond ((not (mls-live-allowed?))
         (mls-fail "enable Allow live model calls before starting a live session"))
        (mls-live-connected? #t)
        (else
         (let ((url (mls-document-url)))
           (and url
                (and (mls-live-persist-existing-ids! url)
                     (begin
                       (set! mls-live-url url)
                       (set! mls-live-document (url->system url))
                       (set! mls-current-document url)
                       (set! mls-live-connected? #t)
                       (set! mls-live-requests '())
                       (set! mls-live-notifications '())
                       (set! mls-live-view #f)
                       (set! mls-live-view-revision 0)
                       (connection-start mls-live-language mls-live-connection-session)
                       ;; Let the connection layer finish spawning the child before the first write.
                       (delayed (:pause 400) (mls-live-send "initialize" '()))
                       (delayed (:pause 100) (mls-live-tick))
                       (set-message "MLS live session starting" "MLS")
                       #t)))))))

(define (mls-live-stop)
  "Send the tested server shutdown request, then stop TeXmacs' child connection."
  (when mls-live-connected?
    (catch #t
      (lambda () (mls-live-send "shutdown" '()))
      (lambda args #f))
    (delayed (:pause 150)
      (catch #t (lambda () (connection-stop mls-live-language mls-live-connection-session))
              (lambda args #f))
      (set! mls-live-connected? #f)
      (set! mls-live-url #f)
      (set! mls-live-document #f)
      (set! mls-live-pending-open #f)
      (set! mls-live-pending-change #f)
      (set! mls-live-requests '())
      (mls-badges-clear-dirty!)
      (mls-badges-redraw)
      (set-message "MLS live session stopped" "MLS")))
  #t)

(define (mls-live-find-request id)
  (let ((hit (assoc id mls-live-requests)))
    (and hit (cdr hit))))

(define (mls-live-drop-request id)
  (set! mls-live-requests
        (filter (lambda (entry) (not (equal? (car entry) id))) mls-live-requests)))

(define (mls-live-extract-json text)
  "Extract complete top-level JSON objects, even when TeXmacs coalesces frames."
  (let loop ((i 0) (start #f) (depth 0) (quoted? #f) (escaped? #f) (acc '()))
    (if (>= i (string-length text))
        (let ((tail (if start (substring text start (string-length text)) "")))
          (set! mls-live-incoming tail)
          (reverse acc))
        (let ((c (string-ref text i)))
          (cond
           ((not start)
            (if (char=? c #\{)
                (loop (+ i 1) i 1 #f #f acc)
                (loop (+ i 1) #f 0 #f #f acc)))
           (quoted?
            (cond (escaped? (loop (+ i 1) start depth #t #f acc))
                  ((char=? c #\\) (loop (+ i 1) start depth #t #t acc))
                  ((char=? c #\") (loop (+ i 1) start depth #f #f acc))
                  (else (loop (+ i 1) start depth #t #f acc))))
           ((char=? c #\") (loop (+ i 1) start depth #t #f acc))
           ((char=? c #\{) (loop (+ i 1) start (+ depth 1) #f #f acc))
           ((char=? c #\})
            (if (= depth 1)
                (loop (+ i 1) #f 0 #f #f
                      (cons (substring text start (+ i 1)) acc))
                (loop (+ i 1) start (- depth 1) #f #f acc)))
           (else (loop (+ i 1) start depth #f #f acc)))))))

(define (mls-live-text tree)
  "Extract text only from expected TeXmacs framing containers."
  (let ((stree (tree->stree tree)))
    (let walk ((x stree))
      (cond ((string? x) x)
            ((and (pair? x) (memq (car x) '(document concat)))
             (apply string-append (map walk (cdr x))))
            (else "")))))

(define (mls-live-result-matches? result revision)
  (and (equal? mls-live-document (mls-json-get result "document"))
       (equal? revision (mls-json-get result "revision"))))

(define (mls-live-reject-pending-change message)
  (set! mls-live-pending-change #f)
  ;; The daemon did not acknowledge this source. Keep both source and cursor
  ;; pending so the same next revision, plus the latest selection, can be retried.
  (set! mls-live-dirty? #t)
  (set! mls-live-cursor-dirty? #t)
  (set! mls-live-last-error message)
  (set-message (string-append "MLS live: " message) "MLS error"))

(define (mls-live-handle-response message)
  (let* ((id (mls-json-get message "id"))
         (pending (and id (mls-live-find-request id)))
         (method (and pending (car pending))))
    (when pending
      (mls-live-drop-request id)
      (cond
       ((mls-json-get message "error")
        (let* ((error-object (mls-json-get message "error"))
               (reason (or (mls-json-get error-object "message") "request failed")))
          (cond ((equal? method "didChange")
                 (when (and mls-live-pending-change
                            (equal? id (car mls-live-pending-change)))
                   (mls-live-reject-pending-change reason)))
                ((equal? method "didOpen")
                 (set! mls-live-pending-open #f)
                 (set! mls-live-last-error reason)
                 (set-message (string-append "MLS live: " reason) "MLS error"))
                (else
                 (set! mls-live-last-error reason)
                 (set-message (string-append "MLS live: " reason) "MLS error")))))
       ((equal? method "initialize") (mls-live-open))
       ((equal? method "didOpen")
        (let* ((result (mls-json-get message "result"))
               (open mls-live-pending-open))
          (when (and open (equal? id (car open))
                     (mls-live-result-matches? result (cadr open)))
            (set! mls-live-revision (cadr open))
            (set! mls-live-source (caddr open))
            (set! mls-live-pending-open #f)
            (set! mls-live-cursor-dirty? #t)
            (mls-live-poll-buffer))))
       ((equal? method "didChange")
        (let* ((result (mls-json-get message "result"))
               (change mls-live-pending-change))
          (if (and change (equal? id (car change))
                   (mls-live-result-matches? result (cadr change)))
              (begin
                (set! mls-live-revision (cadr change))
                (set! mls-live-source (caddr change))
                (set! mls-live-pending-change #f)
                (set! mls-live-cursor-dirty? #t)
                (mls-live-poll-buffer))
              (when (and change (equal? id (car change)))
                (mls-live-reject-pending-change "daemon acknowledged a different document revision")))))
       (else #t)))))

(define (mls-live-merge-decision-annotations view data)
  "Copy safe decision highlights into each matching block as inert JSON data."
  (let* ((annotations (mls-json-get data "decision_annotations"))
         (fields (and annotations (mls-json-get annotations "fields")))
         (differs (and annotations (mls-json-get annotations "differs")))
         (blocks (mls-json-get view "blocks")))
    (if (not (vector? blocks)) view
        (mls-alist-set
         view "blocks"
         (list->vector
          (map (lambda (block)
                 (let ((id (mls-field block "block_id")))
                   (mls-alist-set
                    (mls-alist-set block "decision_fields"
                                   (or (mls-json-get fields id) #()))
                    "decision_differs"
                    (or (mls-json-get differs id) 'null))))
               (vector->list blocks)))))))

(define (mls-live-accept-notification message)
  (let* ((method (mls-json-get message "method"))
         (params (mls-json-get message "params"))
         (document (mls-json-get params "document"))
         (revision (mls-json-get params "revision"))
         (data (mls-json-get params "data")))
    (when (and (string? method) (string? document) (number? revision)
               (equal? document mls-live-document)
               (= revision mls-live-revision))
      (cond ((equal? method "blockStatus")
             (let ((view (mls-live-merge-decision-annotations
                          (mls-json-get data "view") data)))
               (when (mls-view? view)
                 (let ((dirty (mls-json-get data "dirty")))
                   (mls-badges-set-dirty! (if (vector? dirty) (vector->list dirty) '())))
                 (set! mls-current-view view)
                 (set! mls-live-view view)
                 (set! mls-live-view-revision revision)
                 (set! mls-live-notifications
                       (mls-alist-set mls-live-notifications "view" view))
                 (mls-live-render-view view))))
            ((equal? method "reviewRequest")
             (set! mls-live-notifications
                   (mls-alist-set mls-live-notifications "reviewRequest" data))
             (when mls-live-view (mls-live-render-view mls-live-view)))
            ((equal? method "diagnostics")
             (set! mls-live-notifications
                   (mls-alist-set mls-live-notifications "diagnostics" data))
             (when mls-live-view
               (set! mls-live-view (mls-live-apply-diagnostics mls-live-view data))
               (set! mls-current-view mls-live-view)
               (mls-live-render-view mls-current-view)))))))

(define (mls-live-handle-json text)
  (for-each
   (lambda (json)
     (catch #t
       (lambda ()
         (let ((message (mls-json-parse json)))
           (if (mls-json-get message "method")
               (mls-live-accept-notification message)
               (mls-live-handle-response message))))
       (lambda args
         (set! mls-live-last-error (object->string args))
         (set-message "MLS live: ignored malformed daemon output" "MLS error"))))
   (mls-live-extract-json (string-append mls-live-incoming text))))

(tm-define (connection-notify lan ses channel tree)
  (former lan ses channel tree)
  (when (and mls-live-connected? (equal? lan mls-live-language)
             (equal? ses mls-live-connection-session) (equal? channel "output"))
    (let ((text (mls-live-text tree)))
      (when (> (string-length text) 0)
        (mls-live-handle-json text)))))

(define (mls-live-poll-buffer)
  (when (and mls-live-connected? mls-live-document mls-live-url)
    (let ((url mls-live-url))
      (when (and url (url-exists? url))
        (when (and mls-live-dirty? (not mls-live-pending-open)
                   (not mls-live-pending-change)
                   (> mls-live-revision 0))
          (let ((source (mls-live-fresh-source url)))
            (if (equal? source mls-live-source)
                (set! mls-live-dirty? #f)
                (let ((revision (+ mls-live-revision 1)))
                  (mls-live-send "didChange"
                                 `(("document" . ,mls-live-document)
                                   ("revision" . ,revision)
                                   ("source" . ,source)
                                   ("active" . ,(or (mls-live-active-block) 'null)))
                                 (lambda (request-id)
                                   (set! mls-live-pending-change
                                         (list request-id revision source))))
                  (set! mls-live-dirty? #f)
                  (set! mls-live-cursor-dirty? #t)))))
        (let ((active (mls-live-active-block)))
          (when (and (mls-live-current-document?)
                     (> mls-live-revision 0)
                     (not mls-live-pending-open)
                     (not mls-live-pending-change)
                     (not mls-live-dirty?)
                     (or mls-live-cursor-dirty? (not (equal? active mls-live-active))))
            (mls-live-send "cursor"
                           `(("document" . ,mls-live-document)
                             ("revision" . ,mls-live-revision)
                             ("active" . ,(or active 'null))))
            (set! mls-live-active active)
            (set! mls-live-cursor-dirty? #f)))))))

(define (mls-live-tick)
  (when mls-live-connected?
    (catch #t (lambda () (mls-live-poll-buffer))
            (lambda args #f))
    (delayed (:pause 100) (mls-live-tick))))

(tm-define (buffer-notify event tree modification)
  (former event tree modification)
  (when (and (not mls-live-suppress?) mls-live-connected?
             (mls-live-current-document?))
    (set! mls-live-dirty? #t)))

(tm-define (keyboard-press key time)
  (former key time)
  (when (and mls-live-connected? (mls-live-current-document?))
    (set! mls-live-dirty? #t)))

(tm-define (notify-cursor-moved status)
  (former status)
  (when (and mls-live-connected? (mls-live-current-document?))
    (set! mls-live-cursor-dirty? #t)))

(define (mls-live-render-view view)
  "Refresh the established status panel while leaving the user's buffer focused."
  (mls-live-with-focus
   (lambda ()
     (set! mls-current-view view)
     (mls-live-with-suppressed-notifications
      (lambda () (buffer-set-body mls-panel-name (tm->tree (mls-panel-tree view)))))
     (mls-badges-redraw)
     (let ((badge (mls-badge-text view)))
       (set-message badge "MLS")
       badge))))

(tm-define (mls-live-show-panel)
  (:synopsis "Open the live MLS status panel")
  (:secure #t)
  (cond (mls-live-view (mls-render-view mls-live-view))
        (else (mls-fail "the live session has not received a status view yet"))))

(define (mls-live-apply-diagnostics view data)
  "Merge per-block diagnostics into existing panel marker errors."
  (let* ((blocks (mls-json-get view "blocks"))
         (errors (mls-json-get data "blocks")))
    (if (not (and (vector? blocks) (vector? errors))) view
        (let ((updated
               (map (lambda (block)
                      (let* ((id (mls-field block "block_id"))
                             (hit (let loop ((xs (vector->list errors)))
                                    (cond ((null? xs) #f)
                                          ((equal? id (mls-field (car xs) "block")) (car xs))
                                          (else (loop (cdr xs))))))
                             (messages (and hit (mls-json-get hit "errors")))
                             (old (mls-json-get block "errors")))
                        (if (not (vector? messages)) block
                            (mls-alist-set block "errors"
                                           (list->vector
                                            (append (if (vector? old) (vector->list old) '())
                                                    (vector->list messages)))))))
                    (vector->list blocks))))
          (mls-alist-set view "blocks" (list->vector updated))))))

(define (mls-live-view-block uuid)
  (and mls-live-view
       (let ((blocks (mls-json-get mls-live-view "blocks")))
         (and (vector? blocks)
              (let loop ((items (vector->list blocks)))
                (cond ((null? items) #f)
                      ((equal? uuid (mls-field (car items) "block_id")) (car items))
                      (else (loop (cdr items)))))))))

(define (mls-live-review-action uuid action fields)
  (and mls-live-connected?
       (mls-live-review-ready?)
       (let* ((block (mls-live-view-block uuid))
              (sir (and block (equal? action "edit")
                        (mls-sir-from-fields (mls-json-get block "sir") fields)))
              (params `(("document" . ,mls-live-document)
                        ("revision" . ,mls-live-revision)
                        ("block" . ,uuid) ("action" . ,action))))
         (cond ((equal? action "edit")
                (and sir (mls-live-send "review"
                                        (mls-alist-set params "sir" sir))))
               ((member action '("confirm" "reject"))
                (mls-live-send "review" params))
               (else #f)))))

(define (mls-live-review-ready?)
  "Only allow row actions against the exact source acknowledged by the daemon."
  (cond ((or (not mls-live-url) (not mls-live-document)
             (= mls-live-revision 0)
             (not (= mls-live-view-revision mls-live-revision))
             mls-live-pending-open mls-live-pending-change mls-live-dirty?)
         (set! mls-live-last-error "document changes are still syncing")
         (set-message "MLS live: document changes are still syncing; retry this review action after the status updates" "MLS")
         #f)
        (else
         (let ((source (mls-live-fresh-source mls-live-url)))
           (if (equal? source mls-live-source)
               #t
               (begin
                 (set! mls-live-dirty? #t)
                 (set! mls-live-last-error "document changes are still syncing")
                 (set-message "MLS live: document changes are still syncing; retry this review action after the status updates" "MLS")
                 #f))))))

(define (mls-live-retry uuid)
  "Ask the daemon to formalize block UUID again at the acknowledged revision."
  (and mls-live-connected?
       (mls-live-review-ready?)
       (mls-live-send "retry"
                      `(("document" . ,mls-live-document)
                        ("revision" . ,mls-live-revision)
                        ("block" . ,uuid)))))
