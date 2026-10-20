;;; test-json.scm -- the pure JSON reader and writer, text helpers, and fixtures.

(load (string-append (getenv "MLS_TEST_DIR") "/common.scm"))

(define (json-error? text)
  (catch #t (lambda () (mls-json-parse text) #f) (lambda args #t)))

(mls-test-main
 (lambda ()
   (lazy-plugin-force)
   (let ((v (mls-json-parse "{\"a\": [1, 2.5, -3e2], \"b\": {\"c\": null, \"d\": true, \"e\": false}}")))
     (mls-check "nested object and array parse" (and (pair? v) (vector? (mls-json-get v "a"))))
     (mls-check-equal "array length" 3 (vector-length (mls-json-get v "a")))
     (mls-check "negative exponent number" (= -300 (vector-ref (mls-json-get v "a") 2)))
     (mls-check "null is the null symbol" (eq? 'null (mls-json-get (mls-json-get v "b") "c")))
     (mls-check "true parses to #t" (eq? #t (mls-json-get (mls-json-get v "b") "d")))
     (mls-check "false parses to #f" (eq? #f (cdr (assoc "e" (mls-json-get v "b")))))
     (mls-check "missing key is #f" (not (mls-json-get v "zzz"))))
   (mls-check-equal "escapes decode"
                (string-append "a" (string #\") "b" (string #\\) "c/d" (string #\newline) (string #\tab))
                (vector-ref (mls-json-parse "[\"a\\\"b\\\\c\\/d\\n\\t\"]") 0))
   (mls-check-equal "unicode escape gives UTF-8 bytes"
                (string (integer->char #xC3) (integer->char #xA9)
                        (integer->char #xE2) (integer->char #x88) (integer->char #x80))
                (vector-ref (mls-json-parse "[\"\\u00e9\\u2200\"]") 0))
   (mls-check-equal "surrogate pair gives a four-byte character"
                (string (integer->char #xF0) (integer->char #x9F)
                        (integer->char #x98) (integer->char #x80))
                (vector-ref (mls-json-parse "[\"\\ud83d\\ude00\"]") 0))
   (mls-check "trailing data is an error" (json-error? "{} x"))
   (mls-check "two values are an error" (json-error? "1 2"))
   (mls-check "missing value is an error" (json-error? "{\"a\":}"))
   (mls-check "unterminated array is an error" (json-error? "[1,2"))
   (mls-check "unterminated string is an error" (json-error? "\"abc"))
   (mls-check "bad literal is an error" (json-error? "tru"))
   (mls-check "missing colon is an error" (json-error? "{\"a\" 1}"))
   (mls-check "unknown escape is an error" (json-error? "\"\\q\""))
   (mls-check "raw control character is an error" (json-error? (string #\" (integer->char 1) #\")))
   (mls-check "empty input is an error" (json-error? ""))
   (mls-check "empty object and array" (and (null? (mls-json-parse "{}")) (vector? (mls-json-parse "[]"))))
   (let* ((original (list (cons "k" "a\"b\nc") (cons "n" 'null) (cons "t" #t)
                          (cons "v" (vector 1 "x" #f))))
          (round (mls-json-parse (mls-json-encode original))))
     (mls-check-equal "writer round-trips through the reader" original round))
   (mls-check-equal "sh quoting of spaces and a single quote"
                "'a b'\\''c'" (mls-sh-quote "a b'c"))
   (mls-check-equal "sh quoting keeps dollar and backtick inert"
                "'$HOME `id`'" (mls-sh-quote "$HOME `id`"))
   (mls-check-equal "non-blank lines" (list "a : b" "c")
                (mls-non-blank-lines "a : b\n\n  c \r\n"))
   (mls-check-equal "join" "x, y" (mls-join (list "x" "y") ", "))
   (let ((replacement (string (integer->char #xEF) (integer->char #xBF) (integer->char #xBD))))
     (mls-check-equal "lone high surrogate becomes U+FFFD"
                      replacement (vector-ref (mls-json-parse "[\"\\ud800\"]") 0))
     (mls-check-equal "lone low surrogate becomes U+FFFD"
                      replacement (vector-ref (mls-json-parse "[\"\\udc00\"]") 0))
     (mls-check-equal "high surrogate before a non-low escape: U+FFFD, then the escape"
                      (string-append replacement "A")
                      (vector-ref (mls-json-parse "[\"\\ud800\\u0041\"]") 0))
     (mls-check-equal "high surrogate before another high: U+FFFD, then a pair"
                      (string-append replacement)
                      (substring (vector-ref (mls-json-parse "[\"\\ud800\\ud800\\udc00\"]") 0) 0 3))
     (mls-check-equal "high surrogate before a pair: the pair is decoded"
                      (string (integer->char #xF0) (integer->char #x90) (integer->char #x80) (integer->char #x80))
                      (substring (vector-ref (mls-json-parse "[\"\\ud800\\ud800\\udc00\"]") 0) 3 7))
     (mls-check-equal "non-surrogate escapes are unchanged"
                      "\n\t"
                      (vector-ref (mls-json-parse "[\"\\n\\t\"]") 0)))
   (mls-check-equal "file state: no text is blank" 'blank (mls-file-state ""))
   (mls-check-equal "file state: a body with text has content" 'content
                    (mls-file-state (mls-test-file-text (mls-test-fixture "canonical.tm"))))
   (mls-check-equal "file state: a body with no text is blank" 'blank
                    (mls-file-state "<TeXmacs|2.1.5>\n\n<style|generic>\n\n<\\body>\n</body>\n"))
   (mls-check-equal "file state: text without body markers is unknown" 'unknown
                    (mls-file-state "<TeXmacs|2.1.5>\n"))
   (mls-check-equal "file state: arbitrary text is unknown" 'unknown
                    (mls-file-state "not a tm document\n"))
   (let ((view (mls-json-parse (mls-test-file-text (mls-test-views "replay.json")))))
     (mls-check-equal "canned view protocol" 1 (mls-json-get view "protocol"))
     (mls-check-equal "canned view block count" 4 (vector-length (mls-json-get view "blocks")))
     (mls-check "canned view is a view" (mls-view? view)))))
