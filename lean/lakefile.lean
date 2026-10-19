import Lake
open Lake DSL

package mlsExtract

require mathlib from git
  "https://github.com/leanprover-community/mathlib4.git" @
  "c44e0c8ee63ca166450922a373c7409c5d26b00b"

lean_lib MlsExtract where
  roots := #[`MlsExtract]

lean_lib Shadow where
  globs := #[.submodules `Shadow]
