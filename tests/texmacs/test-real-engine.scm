;;; test-real-engine.scm -- one integration test against the real `mls` binary, in
;;; review-status mode only (no models, no Lean). run.sh sets MLS_REAL_BIN when the
;;; binary exists and advertises review-status; otherwise this file skips.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(mls-test-main
 (lambda ()
   (define real (getenv "MLS_REAL_BIN"))
   (define plugin-loaded (lazy-plugin-force))
   (cond ((or (not real) (string=? real ""))
          (display* "SKIP real engine review-status: the mls binary has no review-status command yet\n"))
         (else
          (setenv "MLS_BIN" real)
          (load-buffer (string->url (mls-test-doc "canonical.tm")))
          (mls-check "review-status succeeds on the real engine" (mls-show-status))
          (mls-check-equal "real view is protocol 1" 1 (mls-json-get mls-current-view "protocol"))
          (mls-check "real view lists blocks" (vector? (mls-json-get mls-current-view "blocks")))
          (mls-check "real view has counts" (pair? (mls-json-get mls-current-view "counts")))))))
