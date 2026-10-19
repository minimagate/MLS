# TeXmacs corpus provenance

The three `texmacs-*.tm` fixtures are unmodified documentation files from GNU
TeXmacs 2.1.4/2.1.5, installed under
`/Applications/TeXmacs.app/Contents/Resources/share/TeXmacs/doc`:

- `main/text/man-environments.en.tm`
- `devel/format/basics/tm-tm.en.tm`
- `devel/format/regular/prim-table.en.tm`

They retain their source comments and are redistributed under the GNU Free
Documentation License, version 1.1 or later, as those comments specify. A copy
of the license shipped with TeXmacs is included as `GFDL-1.1.txt`.

The adjacent `*.expected.json` files are MLS-generated normalized block trees.
They omit random UUID values while retaining kinds, labels, dependency indices,
source spans, and diagnostic locations.
