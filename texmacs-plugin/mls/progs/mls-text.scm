;;; mls-text.scm -- string helpers for the MLS plugin (Guile 1.8 subset, no SRFI).
;;; Strings are byte strings in TeXmacs; every helper here is byte-safe.

(define (mls-join strings sep)
  "Concatenate the list STRINGS with SEP between neighbours."
  (cond ((null? strings) "")
        ((null? (cdr strings)) (car strings))
        (else (string-append (car strings) sep (mls-join (cdr strings) sep)))))

(define (mls-search needle hay start)
  "Index of the first occurrence of NEEDLE in HAY at or after START, or #f."
  (let ((n (string-length needle))
        (h (string-length hay)))
    (let loop ((i start))
      (cond ((> (+ i n) h) #f)
            ((string=? (substring hay i (+ i n)) needle) i)
            (else (loop (+ i 1)))))))

(define (mls-blank? s)
  "True when S holds only whitespace."
  (let loop ((i 0))
    (cond ((= i (string-length s)) #t)
          ((memv (string-ref s i) '(#\space #\tab #\newline #\return)) (loop (+ i 1)))
          (else #f))))

(define (mls-trim s)
  (string-trim-both s))

(define (mls-split-lines s)
  "Split S at newlines; carriage returns are dropped."
  (let loop ((i 0) (start 0) (acc '()))
    (cond ((= i (string-length s))
           (reverse (cons (mls-strip-cr (substring s start i)) acc)))
          ((char=? (string-ref s i) #\newline)
           (loop (+ i 1) (+ i 1) (cons (mls-strip-cr (substring s start i)) acc)))
          (else (loop (+ i 1) start acc)))))

(define (mls-strip-cr s)
  (let ((n (string-length s)))
    (if (and (> n 0) (char=? (string-ref s (- n 1)) #\return))
        (substring s 0 (- n 1))
        s)))

(define (mls-non-blank-lines s)
  "Trimmed, non-blank lines of S, in order."
  (let loop ((ls (mls-split-lines s)) (acc '()))
    (cond ((null? ls) (reverse acc))
          ((mls-blank? (car ls)) (loop (cdr ls) acc))
          (else (loop (cdr ls) (cons (mls-trim (car ls)) acc))))))

(define (mls-sh-quote s)
  "Quote S as one POSIX shell word: single quotes, with ' written as '\\''."
  (apply string-append
         "'"
         (append (map (lambda (c) (if (char=? c #\') "'\\''" (string c)))
                      (string->list s))
                 (list "'"))))
