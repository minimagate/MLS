;;; mls-document.scm -- the open .tm document: which buffer MLS acts on, the save and
;;; reload protocol around engine commands, the file guard, and block carrier lookup.
;;;
;;; The engine reads and writes the file on disk; the buffer is only ever brought in
;;; line with it by TeXmacs (buffer-save, buffer-load). The only change the engine may
;;; make to a document is adding mls-id carriers, so every sync is checked by tree
;;; equality with the carriers stripped (mls-strip-carriers). The protocol (see
;;; docs/research/m3-texmacs.md sections 0.2 and 3):
;;;   1. Before every engine command that reads or writes the document, the buffer is
;;;      saved (mls-prepare-document!). The saved file is read back with tree-import,
;;;      which replaces no buffer, and must equal the buffer. If it does not, the
;;;      previous file text is put back, the buffer is left as it was, and the command
;;;      is refused before the engine runs. The previous file text is read before the
;;;      save; if it cannot be read, nothing is written. A file changed on disk since the last sync
;;;      is reloaded first, but only when the buffer has no edits since then; otherwise
;;;      the command is refused. With no sync record yet (the first command for a
;;;      document), the file on disk must already read back as the buffer, or the
;;;      command is refused before anything is written. Buffer-modified? is not relied on.
;;;   2. A reload (mls-reload-document!) is only done when the buffer matches the last
;;;      sync. The reloaded body must equal the recorded body (carriers stripped) and
;;;      keep every carrier. If not, the kept tree is restored, the buffer stays marked
;;;      modified, the file is put back to the text it had before the command, and the
;;;      run is an error.
;;;   3. The file guard (mls-file-state) refuses any save or reload when the file has
;;;      text but no readable body, and any save or reload that would empty a body
;;;      that is not empty on the other side.

(define mls-panel-name "tmfs://aux/mls-status")
(define mls-records '())          ; alist: path -> (file-text text-tree) at last sync
(define mls-current-document #f)  ; URL of the document the last command acted on

(define (mls-panel-buffer? url)
  (equal? (url->string url) mls-panel-name))

(define (mls-main-buffer)
  "The document the user works on: the current buffer, or, while the MLS panel has
focus, the document shown in the window (else the last MLS document)."
  (let ((cur (current-buffer)))
    (if (not (mls-panel-buffer? cur))
        cur
        (or (catch #t
              (lambda ()
                (let ((shown (window->buffer (current-window))))
                  (and shown (not (url-none? shown)) (not (mls-panel-buffer? shown)) shown)))
              (lambda args #f))
            mls-current-document))))

(define (mls-document-url)
  "The document MLS acts on (mls-main-buffer). Returns #f after reporting why it
cannot be used."
  (let ((url (mls-main-buffer)))
    (cond ((not url) (mls-fail "no document yet: open a saved .tm file first") #f)
          ((not (url-exists? url)) (mls-fail "save the document to a file first") #f)
          (else url))))

(define (mls-body-stree url)
  (tree->stree (buffer-get-body url)))

(define (mls-body-empty? stree)
  "True when the in-memory body holds no text."
  (and (pair? stree)
       (eq? (car stree) 'document)
       (let loop ((xs (cdr stree)))
         (cond ((null? xs) #t)
               ((and (string? (car xs)) (mls-blank? (car xs))) (loop (cdr xs)))
               (else #f)))))

(define (mls-file-state text)
  "State of the .tm TEXT: 'blank when it has no body text, 'content when its body has
text, and 'unknown when it has text but the body markers cannot be found."
  (if (mls-blank? text)
      'blank
      (let* ((open-at (mls-search "body>" text 0))
             (close-at (and open-at (mls-search "</body>" text open-at))))
        (cond ((not (and open-at close-at (>= close-at (+ open-at 5)))) 'unknown)
              ((mls-blank? (substring text (+ open-at 5) close-at)) 'blank)
              (else 'content)))))

(define (mls-file-text url)
  (mls-read-text (url->system url)))

(define (mls-guarded-file-text url)
  "The file text of URL, or #f after reporting that it cannot be read. Nothing is written."
  (or (catch #t (lambda () (mls-file-text url)) (lambda args #f))
      (begin (mls-fail "could not read the document file; nothing was changed") #f)))

(define (mls-carrier-stree? t)
  "True when the stree T is an mls-id carrier: (with \"mls-id\" UUID BODY)."
  (and (pair? t)
       (eq? (car t) 'with)
       (= (length t) 4)
       (equal? (cadr t) "mls-id")))

(define (mls-strip-carriers stree)
  "STREE with every mls-id carrier removed. A carrier's content is a document, and it is
spliced into the enclosing document or concat, so a block reads the same with or
without its carrier."
  (cond ((not (pair? stree)) stree)
        ((mls-carrier-stree? stree) (mls-strip-carriers (cadddr stree)))
        ((memq (car stree) '(document concat))
         (cons (car stree) (mls-strip-children (cdr stree))))
        (else (map mls-strip-carriers stree))))

(define (mls-strip-children xs)
  "The list XS with its carriers removed, as in mls-strip-carriers."
  (cond ((null? xs) '())
        ((mls-carrier-stree? (car xs))
         (let ((content (cadddr (car xs))))
           (append (if (and (pair? content) (eq? (car content) 'document))
                       (mls-strip-children (cdr content))
                       (list (mls-strip-carriers content)))
                   (mls-strip-children (cdr xs)))))
        (else (cons (mls-strip-carriers (car xs)) (mls-strip-children (cdr xs))))))

(define (mls-carrier-uuids stree)
  "The mls-id UUIDs of the carriers in STREE, in document order."
  (cond ((not (pair? stree)) '())
        ((mls-carrier-stree? stree)
         (cons (caddr stree) (mls-carrier-uuids (cadddr stree))))
        (else (apply append (map mls-carrier-uuids stree)))))

(define (mls-text-tree url)
  "The body of URL as a stree with the mls-id carriers stripped."
  (mls-strip-carriers (mls-body-stree url)))

(define (mls-same-text? a b)
  "Equality of two text trees. Two empty bodies are equal whatever their form."
  (or (equal? a b)
      (and (mls-body-empty? a) (mls-body-empty? b))))

(define (mls-import-body-stree url)
  "The body of the file at URL as TeXmacs parses it (carriers included), or #f. The
file is read with tree-import, so no buffer is replaced."
  (catch #t
    (lambda ()
      (let* ((doc (tree->stree (tree-import url "texmacs")))
             (body (and (pair? doc) (assq 'body (cdr doc)))))
        (and body (pair? (cdr body)) (cadr body))))
    (lambda args #f)))

(define (mls-saved-text-tree url)
  "The text tree of the file at URL as TeXmacs reads it back, or #f."
  (let ((body (mls-import-body-stree url)))
    (and body (mls-strip-carriers body))))

(define (mls-record url)
  "(file-text text-tree) at the last sync of URL, or #f when there is none."
  (let ((hit (assoc (url->system url) mls-records)))
    (and hit (cdr hit))))

(define (mls-recorded-file-text url)
  "The file text recorded at the last sync of URL, or #f. Before an engine command, this
is the file text the command started from."
  (let ((record (mls-record url)))
    (and record (car record))))

(define (mls-mark-synced! url)
  "Record that the file on disk and the buffer body of URL now agree. Returns #f, with the
error reported, when the file cannot be read."
  (let ((file (mls-guarded-file-text url)))
    (and file
         (let* ((path (url->system url))
                (entry (list file (mls-text-tree url)))
                (hit (assoc path mls-records)))
           (if hit
               (set-cdr! hit entry)
               (set! mls-records (cons (cons path entry) mls-records)))
           #t))))

(define (mls-restore-body! url stree)
  "Put STREE back as the body of URL and keep it marked modified."
  (buffer-set-body url (tm->tree stree))
  (buffer-pretend-modified url))

(define (mls-restore-file! url text)
  "Write TEXT back to the file of URL, byte for byte. Returns #t when the file then reads
back as TEXT. Returns #f when TEXT is #f, or when the file cannot be written or read."
  (and text
       (catch #t
              (lambda ()
                (string-save text (url->system url))
                (string=? (mls-file-text url) text))
              (lambda args #f))))

(define (mls-carriers-kept? kept loaded)
  "True when every carrier of the body KEPT is still in the body LOADED."
  (let ((after (mls-carrier-uuids loaded)))
    (let loop ((uuids (mls-carrier-uuids kept)))
      (cond ((null? uuids) #t)
            ((member (car uuids) after) (loop (cdr uuids)))
            (else #f)))))

(define (mls-reload-problem kept loaded expected)
  "#f when the reloaded body LOADED is EXPECTED with the carriers of KEPT still in
place. Otherwise the reason, as a message for the user."
  (cond ((and (mls-body-empty? loaded) (not (mls-body-empty? expected)))
         "the reloaded file could not be imported by TeXmacs")
        ((not (mls-same-text? (mls-strip-carriers loaded) expected))
         "the reloaded file changed the document text (only mls-id carriers may be added)")
        ((not (mls-carriers-kept? kept loaded))
         "the reloaded file lost mls-id carriers")
        (else #f)))

(define (mls-reload-document! url before)
  "Reload URL from disk. Refused unless the buffer matches the last sync. The reloaded
body must equal the recorded body, carriers stripped, and keep every carrier. If not, the
kept tree is restored, the file is put back to BEFORE (the file text the MLS command
started from, or #f when there is none), and the reload is an error. Returns #t on success."
  (let* ((file (mls-guarded-file-text url))
         (state (and file (mls-file-state file)))
         (kept (mls-body-stree url))
         (record (mls-record url))
         (expected (if record (cadr record) (mls-strip-carriers kept))))
    (cond ((not file) #f)
          ((and record (not (equal? (mls-strip-carriers kept) expected)))
           (mls-fail "refusing to reload: the document has edits that were not saved")
           #f)
          ((eq? state 'unknown)
           (mls-fail "refusing to reload: the file on disk has no readable body (check the .tm layout)")
           #f)
          ((and (eq? state 'blank) (not (mls-body-empty? kept)))
           (mls-fail "refusing to reload: the file on disk is empty but the open document is not")
           #f)
          ((buffer-load url)
           (mls-fail "could not reload the document from disk")
           #f)
          (else
           (let ((problem (mls-reload-problem kept (mls-body-stree url) expected)))
             (cond ((not problem)
                    (mls-mark-synced! url))
                   (else
                    (mls-restore-body! url kept)
                    (let ((file-back (mls-restore-file! url before)))
                      (mls-fail (string-append
                                 problem
                                 "; your buffer was restored and is still marked modified; "
                                 (cond (file-back
                                        "the file was put back to the text it had before the MLS command")
                                       (before
                                        "the file could not be put back and still holds the rewrite")
                                       (else
                                        "no earlier file text was recorded, so the file was left as it is"))))
                      #f))))))))

(define (mls-save-document! url)
  "Save the buffer of URL, then read the file back with TeXmacs: its body must equal the
buffer with carriers stripped. The previous file text is read before anything is written:
if it cannot be read, the save is refused and nothing is written, so the buffer and the
file are left as they were. If the save does not round-trip, the previous file text is put
back, the buffer and its modified state are left as they were, and the save is an error,
so the engine does not run. Returns #t when saved and verified."
  (let ((previous (catch #t (lambda () (mls-file-text url)) (lambda args #f))))
    (if (not previous)
        (begin
          (mls-fail "could not read the previous file text, so the document was not saved and the file was left as it is; MLS did not run")
          #f)
        (let ((modified (buffer-modified? url))
              (expected (mls-text-tree url)))
          (cond ((buffer-save url)
                 (mls-fail "could not save the document")
                 #f)
                ((mls-same-text? (mls-saved-text-tree url) expected) #t)
                (else
                 (string-save previous (url->system url))
                 (if modified (buffer-pretend-modified url))
                 (mls-fail "TeXmacs cannot round-trip this document (content after raw data in a long-form argument?); MLS did not run")
                 #f))))))

(define (mls-disk-matches-buffer? url state)
  "True when the file on disk, read back by TeXmacs, is the body of URL with the carriers
stripped. A blank file matches a document with an empty body. STATE is the file state."
  (or (and (eq? state 'blank) (mls-body-empty? (mls-body-stree url)))
      (mls-same-text? (mls-text-tree url) (mls-saved-text-tree url))))

(define (mls-prepare-document! url)
  "Make the file on disk hold the buffer before an engine command. Returns #t when it
does. Without a sync record, the file on disk must already read back as the buffer
(carriers stripped): an external edit made after the document was opened, or an
unsaved edit in the buffer, is refused before anything is written, so it is never
overwritten. With a record, a file changed on disk since the last sync is reloaded when
the buffer has no edits since then (nothing is lost); otherwise the command is refused
as a conflict."
  (let* ((file (catch #t (lambda () (mls-file-text url)) (lambda args #f)))
         (state (and file (mls-file-state file)))
         (record (mls-record url)))
    (cond ((not file)
           (mls-fail "could not read the document file; MLS did not run")
           #f)
          ((eq? state 'unknown)
           (mls-fail "refusing to save: the file on disk has no readable body (check the .tm layout)")
           #f)
          ((and record (not (string=? file (car record))))
           (cond ((mls-same-text? (mls-text-tree url) (cadr record))
                  (mls-reload-document! url file))
                 (else
                  (mls-fail "the file changed on disk while the document has unsaved edits; revert the document or save a copy, then run MLS again")
                  #f)))
          ((and (mls-body-empty? (mls-body-stree url)) (not (eq? state 'blank)))
           (mls-fail "refusing to save: the open document has an empty body but the file on disk does not (check the .tm layout)")
           #f)
          ((and (not record) (not (mls-disk-matches-buffer? url state)))
           (mls-fail "the file on disk differs from this buffer; save or revert it in TeXmacs, then retry")
           #f)
          ((mls-save-document! url)
           (mls-mark-synced! url))
          (else #f))))

(define (mls-reload-if-changed! url)
  "After an engine command: reload when the engine changed the file on disk. Returns #t
when the buffer matches the file afterwards. The file text before the command is the one
recorded at the last sync; a refused reload puts that text back. An unreadable file is a
refusal, with nothing written."
  (let ((record (mls-record url))
        (file (mls-guarded-file-text url)))
    (cond ((not file) #f)
          ((and record (string=? file (car record))) #t)
          (else (mls-reload-document! url (mls-recorded-file-text url))))))

(define (mls-carrier? t)
  (and (tree-compound? t)
       (eq? (tree-label t) 'with)
       (>= (tree-arity t) 3)
       (equal? (tree->stree (tree-ref t 0)) "mls-id")))

(define (mls-carriers)
  "All (uuid . carrier-tree) pairs of the current buffer, in document order."
  (let ((acc '()))
    (let walk ((t (buffer-tree)))
      (when (mls-carrier? t)
        (set! acc (cons (cons (tree->stree (tree-ref t 1)) t) acc)))
      (when (tree-compound? t)
        (for-each walk (tree-children t))))
    (reverse acc)))

(define (mls-go-to-block uuid)
  "Move the cursor to the carrier of block UUID in the document. Returns #t if found."
  (let ((url (if (and (defined? 'mls-live-connected?) mls-live-connected?)
                 mls-live-url
                 mls-current-document)))
    (cond ((not url) (mls-fail "no document is open in MLS"))
          (else
           (switch-to-buffer url)
           (let ((hit (assoc uuid (mls-carriers))))
             (cond (hit (tree-go-to (cdr hit) :start) #t)
                   (else (mls-fail (string-append "block " uuid " is not in the document")))))))))
