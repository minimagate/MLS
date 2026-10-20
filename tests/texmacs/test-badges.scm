;;; test-badges.scm -- status pills beside formal block headings: which pills each
;;; status gets, the palette, live progress, the UUID guard on pill commands, and that
;;; the in-memory badge package never reaches a saved file, an autosave or an export.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(define bd-definition "520997b8-c353-44cb-bba8-5e69745c0656")
(define bd-draft "e3220339-8782-4a00-8952-345d0005ab01")
(define bd-verified "e688a3ab-4e5d-4b7e-9c57-c857a7e07741")
(define bd-working "7a8ae695-2a47-4e3d-b5cb-664566c24911")
(define bd-error "20126ddc-bfe5-4ff3-a604-a8cf7a75f411")
(define bd-refuted "2aefa595-4494-4a0f-9be1-eb81b12b7f0a")
(define bd-open "37206045-447a-4559-8d22-dde83d5d212d")
(define bd-new "d6f2ab0b-d1b2-489d-8e75-6dfe3ee9bd70")

(define (bd-labels id working)
  "The (shape tone label action) of each pill of block ID in the current view."
  (map (lambda (spec) (list (car spec) (cadr spec) (cadddr spec) (list-ref spec 4)))
       (mls-badge-specs (mls-badge-view-block id) working #f)))

(define (bd-text x)
  (object->string x))

(define (bd-fresh name)
  (let ((path (mls-test-doc name)))
    (string-save (mls-test-file-text (mls-test-fixture "badges.tm")) path)
    (load-buffer (string->url path))
    (switch-to-buffer (string->url path))
    (string->url path)))

(mls-test-main
 (lambda ()
   (lazy-plugin-force)
   (set! mls-current-view (mls-json-parse (mls-test-file-text (mls-test-views "badges.json"))))

   ;; Pill choice per status. The statement pill is filled, the proof pill outlined.
   (mls-check-equal "confirmed definition: one filled statement pill"
                    '((filled green "statement" open)) (bd-labels bd-definition #f))
   (mls-check-equal "draft with a decision difference: review pill and may-differ pill"
                    '((filled amber "review draft" open) (filled amber "may differ" open))
                    (bd-labels bd-draft #f))
   (mls-check-equal "verified proof: statement pill and outlined proof pill"
                    '((filled green "statement" open) (outline green "proof verified" open))
                    (bd-labels bd-verified #f))
   (mls-check-equal "formalization error with rerun: error pill and a retry pill"
                    '((filled red "error" open) (filled amber "rerun" retry))
                    (bd-labels bd-error #f))
   (mls-check-equal "refuted proof is red and outlined"
                    '((filled green "statement" open) (outline red "refuted" open))
                    (bd-labels bd-refuted #f))
   (mls-check-equal "unproven proof reads as open, not as a failure"
                    '((filled green "statement" open) (outline neutral "proof open" open))
                    (bd-labels bd-open #f))
   (mls-check-equal "not formalized is neutral"
                    '((filled neutral "not formalized" open)) (bd-labels bd-new #f))
   (mls-check "a block in the live dirty set shows formalizing, not its old status"
              (let ((pills (bd-labels bd-working (cons bd-working 0))))
                (and (= (length pills) 1)
                     (equal? (cadr (car pills)) 'blue)
                     (mls-test-contains? (caddr (car pills)) "formalizing"))))
   (mls-check "a confirmed block in the dirty set keeps its statement and shows proving"
              (let ((pills (bd-labels bd-definition (cons bd-definition 0))))
                (and (equal? (car pills) '(filled green "statement" open))
                     (mls-test-contains? (caddr (cadr pills)) "proving"))))
   (mls-check-equal "elapsed time label" " 12s" (mls-badge-elapsed-label 12))
   (mls-check-equal "no elapsed label in the first second" "" (mls-badge-elapsed-label 1))

   ;; The extern entry point.
   (let ((pills (mls-badge (string->tree bd-verified) (string->tree "black"))))
     (mls-check "extern returns chips with clickable open actions"
                (and (mls-test-contains? (bd-text pills) "mls-chip")
                     (mls-test-contains? (bd-text pills)
                                         (string-append "(mls-badge-open \\\"" bd-verified "\\\")")))))
   (mls-check "light page gets the light palette"
              (mls-test-contains? (bd-text (mls-badge (string->tree bd-verified) (string->tree "black")))
                                  "#eaf3de"))
   (mls-check "dark page gets the dark palette"
              (mls-test-contains? (bd-text (mls-badge (string->tree bd-verified) (string->tree "#e4e4e4")))
                                  "#27500a"))
   (mls-check-equal "a block without a view entry has no pills outside a live session"
                    "" (mls-badge (string->tree "00000000-0000-4000-8000-000000000000")
                                  (string->tree "black")))
   (mls-check-equal "a carrier-less theorem (uninitialised mls-id) has no pills"
                    "" (mls-badge (string->tree "<uninit>") (string->tree "black")))

   ;; The UUID guard: pill commands are Scheme source, so only plain UUIDs pass.
   (mls-check "uuid guard accepts a UUID" (mls-badge-uuid? bd-definition))
   (mls-check "uuid guard rejects a quote injection"
              (not (mls-badge-uuid? "520997b8-c353-44cb-bba8-5e69745c06\")")))
   (mls-check "uuid guard rejects non-hex characters"
              (not (mls-badge-uuid? "520997b8-c353-44cb-bba8-5e69745c065z")))
   (mls-check-equal "an injected id renders no pills"
                    "" (mls-badge (string->tree "x\") (system \"touch pwned\") (\"") (string->tree "black")))
   (mls-check-equal "opening a pill with a bad id is refused" #f (mls-badge-open "../../etc"))

   ;; The package lives in memory only.
   (let* ((url (bd-fresh "badges-save.tm"))
          (path (url->system url)))
     (set! mls-current-document url)
     (mls-badges-redraw)
     (mls-check "redraw attaches the badge package to the MLS document"
                (mls-badges-attached? url))
     (mls-check "attaching does not mark the document modified" (not (buffer-modified? url)))
     (buffer-pretend-modified url)
     (buffer-save url)
     (mls-check "a direct buffer-save writes no badge package"
                (not (mls-test-contains? (string-load path) "mls-badges")))
     (mls-check "the package is attached again after the save" (mls-badges-attached? url))
     (mls-check "the document reads as saved after the save" (not (buffer-modified? url)))
     (buffer-pretend-modified url)
     (save-buffer url)
     (mls-check "Save writes no badge package"
                (not (mls-test-contains? (string-load path) "mls-badges")))
     (mls-check "the saved style is the document's own"
                (mls-test-contains? (string-load path) "<style|generic>"))
     ;; autosave-buffer skips a buffer unchanged since its last autosave; its write is
     ;; this buffer-export to the ~ file.
     (buffer-export url (url-autosave url "~") "texmacs")
     (let ((backup (url->system (url-autosave url "~"))))
       (mls-check "autosave writes no badge package"
                  (and (file-exists? backup)
                       (not (mls-test-contains? (string-load backup) "mls-badges")))))
     (mls-check "the package is attached again after the autosave" (mls-badges-attached? url))

     (let ((html (string-append (mls-test-env "MLS_TEST_WORK") "/badges.html"))
           (tex (string-append (mls-test-env "MLS_TEST_WORK") "/badges.tex")))
       (export-buffer-main url (string->url html) "html" (list :overwrite))
       (export-buffer-main url (string->url tex) "latex" (list :overwrite))
       (mls-check "HTML export has no pills"
                  (and (file-exists? html)
                       (not (mls-test-contains? (string-load html) "proof verified"))
                       (not (mls-test-contains? (string-load html) "mls-badges"))))
       (mls-check "LaTeX export has no pills"
                  (and (file-exists? tex)
                       (not (mls-test-contains? (string-load tex) "proof verified"))
                       (not (mls-test-contains? (string-load tex) "mls-chip")))))
     (mls-check "the package is attached again after the exports" (mls-badges-attached? url))

     (mls-check "the live snapshot sent to the engine has no badge package"
                (not (mls-test-contains? (serialize-texmacs (mls-badges-strip-tree (buffer-get url)))
                                         "mls-badges")))
     (mls-check "the carriers survive the save"
                (mls-test-contains? (string-load path) bd-verified)))))
