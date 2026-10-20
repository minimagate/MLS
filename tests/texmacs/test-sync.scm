;;; test-sync.scm -- the first MLS command for a document (no sync record yet) runs only when
;;; the file on disk already reads back as the buffer, carriers stripped. An external edit
;;; made after the document was opened, and an unsaved edit in the buffer, are refused before
;;; anything is written, so neither is overwritten. An unchanged file proceeds. After a sync,
;;; an external edit is still refused and the file kept. Everything runs against the stub engine.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(define (sy-fresh name)
  "Copy the canonical fixture to the work file NAME, load it, and return its URL."
  (let ((path (mls-test-doc name)))
    (string-save (mls-test-file-text (mls-test-fixture "canonical.tm")) path)
    (load-buffer (string->url path))
    (string->url path)))

(define (sy-insert-marker! path marker)
  "Add MARKER as text before </body> in the file at PATH, as an external editor would."
  (let* ((text (string-load path))
         (at (mls-search "</body>" text 0)))
    (string-save (string-append (substring text 0 at) marker "\n" (substring text at (string-length text)))
                 path)))

(define (sy-engine-ran?)
  (mls-test-contains? (mls-test-log) "<review-status>"))

(define (sy-reset!)
  (setenv "MLS_STUB_VIEW" (mls-test-views "replay.json"))
  (setenv "MLS_STUB_FAIL" "")
  (set! mls-last-error #f)
  (mls-test-clear-log))

(mls-test-main
 (lambda ()
   (define work (mls-test-env "MLS_TEST_WORK"))
   (lazy-plugin-force)
   (sy-reset!)

   ;; A. The reviewer's probe: an external edit after open, then the first command. The
   ;;    command is refused, the engine does not run, and the file and buffer are unchanged.
   (let* ((url (sy-fresh "ext.tm"))
          (doc (mls-test-doc "ext.tm"))
          (body-before (mls-test-body-text url)))
     (sy-insert-marker! doc "EXTERNAL-EDIT-A")
     (let ((file-before (mls-test-file-text doc)))
       (mls-check "external edit after open: the first command is refused" (not (mls-show-status)))
       (mls-check "external edit after open: the error names the difference"
                  (mls-test-contains? (or mls-last-error "") "the file on disk differs from this buffer"))
       (mls-check "external edit after open: the engine is not invoked" (not (sy-engine-ran?)))
       (mls-check-equal "external edit after open: file on disk unchanged" file-before (mls-test-file-text doc))
       (mls-check "external edit after open: the marker survives on disk"
                  (mls-test-contains? (mls-test-file-text doc) "EXTERNAL-EDIT-A"))
       (mls-check-equal "external edit after open: buffer unchanged" body-before (mls-test-body-text url))))
   (sy-reset!)

   ;; B. An unsaved edit in the buffer is refused the same way; the file is not written.
   (let* ((url (sy-fresh "unsaved.tm"))
          (doc (mls-test-doc "unsaved.tm"))
          (file-before (mls-test-file-text doc)))
     (buffer-set-body url (tm->tree '(document "BUFFER-ONLY-EDIT")))
     (mls-check "unsaved buffer edit: the first command is refused" (not (mls-show-status)))
     (mls-check "unsaved buffer edit: the engine is not invoked" (not (sy-engine-ran?)))
     (mls-check-equal "unsaved buffer edit: file on disk unchanged" file-before (mls-test-file-text doc))
     (mls-check "unsaved buffer edit: buffer keeps its edit"
                (mls-test-contains? (mls-test-body-text url) "BUFFER-ONLY-EDIT")))
   (sy-reset!)

   ;; C. An unchanged file proceeds on the first command.
   (let ((url (sy-fresh "clean.tm")))
     (mls-check "unchanged file: the first command proceeds" (mls-show-status))
     (mls-check-equal "unchanged file: no error" #f mls-last-error)
     (mls-check "unchanged file: the engine runs" (sy-engine-ran?)))
   (sy-reset!)

   ;; D. Control: after a sync, an external edit is refused and the file kept.
   (let* ((url (sy-fresh "synced.tm"))
          (doc (mls-test-doc "synced.tm")))
     (mls-check "synced document: the first command proceeds" (mls-show-status))
     (sy-insert-marker! doc "EXTERNAL-EDIT-B")
     (mls-check "external edit after a sync: refused" (not (mls-show-status)))
     (mls-check "external edit after a sync: file kept"
                (mls-test-contains? (mls-test-file-text doc) "EXTERNAL-EDIT-B")))))
