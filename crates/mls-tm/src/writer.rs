//! Atomic persistence of generated IDs without reserializing source bytes.

use crate::IdInsertion;
use std::{
    error::Error,
    fmt, fs, io,
    io::Write,
    path::{Path, PathBuf},
};

/// Failure while validating or atomically persisting ID wrappers.
#[derive(Debug)]
pub enum WriteError {
    /// A filesystem operation failed.
    Io(io::Error),
    /// An insertion span was empty, out of bounds, duplicated, or crossed another span.
    InvalidInsertion,
    /// Applying the insertions would produce invalid TeXmacs native syntax.
    InvalidGeneratedSyntax,
    /// The file no longer matches the bytes from which insertions were computed.
    StaleSource,
    /// The destination is a symbolic link, which the writer does not replace.
    SymlinkTarget,
}
impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::InvalidInsertion => {
                f.write_str("invalid, duplicate, or crossing ID insertion spans")
            }
            Self::InvalidGeneratedSyntax => {
                f.write_str("ID insertions would produce invalid TeXmacs syntax")
            }
            Self::StaleSource => f.write_str("source file changed since it was parsed"),
            Self::SymlinkTarget => f.write_str("refusing to replace a symbolic link"),
        }
    }
}
impl Error for WriteError {}
impl From<io::Error> for WriteError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Persist wrappers through an exclusively-created same-directory file and rename.
///
/// Symbolic-link targets are rejected: replacing a link would unexpectedly change
/// the directory entry while preserving (and no longer updating) the linked file.
pub fn write_ids_atomic(
    path: &Path,
    source: &[u8],
    insertions: &[IdInsertion],
) -> Result<(), WriteError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(WriteError::SymlinkTarget);
    }
    if fs::read(path)? != source {
        return Err(WriteError::StaleSource);
    }
    let output = apply_insertions(source, insertions)?;
    if insertions.is_empty() {
        return Ok(());
    }
    crate::parse(&output).map_err(|_| WriteError::InvalidGeneratedSyntax)?;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document.tm");
    let (mut temporary_file, mut temporary) = create_temporary(parent, file_name)?;
    (|| {
        temporary_file.write_all(&output)?;
        fs::set_permissions(temporary.path(), metadata.permissions())?;
        temporary_file.sync_all()?;
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(WriteError::SymlinkTarget);
        }
        if fs::read(path)? != source {
            return Err(WriteError::StaleSource);
        }
        fs::rename(temporary.path(), path)?;
        temporary.disarm();
        Ok(())
    })()
}

fn apply_insertions(source: &[u8], insertions: &[IdInsertion]) -> Result<Vec<u8>, WriteError> {
    let mut ordered = insertions.to_vec();
    ordered.sort_by(|left, right| {
        left.span
            .start
            .cmp(&right.span.start)
            .then_with(|| right.span.end.cmp(&left.span.end))
    });
    let mut open = Vec::new();
    let mut seen_spans = std::collections::HashSet::new();
    for insertion in &ordered {
        if insertion.span.end > source.len() || insertion.span.start >= insertion.span.end {
            return Err(WriteError::InvalidInsertion);
        }
        while open
            .last()
            .is_some_and(|end: &usize| insertion.span.start >= *end)
        {
            open.pop();
        }
        if open.last().is_some_and(|end| insertion.span.end > *end) {
            return Err(WriteError::InvalidInsertion);
        }
        if !seen_spans.insert((insertion.span.start, insertion.span.end)) {
            return Err(WriteError::InvalidInsertion);
        }
        open.push(insertion.span.end);
    }

    let added_capacity: usize = ordered
        .iter()
        .map(|insertion| insertion.id.to_string().len() + 22)
        .sum();
    let mut output = Vec::with_capacity(source.len() + added_capacity);
    let mut edits = Vec::with_capacity(ordered.len() * 2);
    #[derive(Clone, Copy)]
    enum Edit<'a> {
        Open(&'a IdInsertion),
        Close(&'a IdInsertion),
    }
    for insertion in &ordered {
        edits.push((insertion.span.start, Edit::Open(insertion)));
        edits.push((insertion.span.end, Edit::Close(insertion)));
    }
    edits.sort_by(|(left_position, left), (right_position, right)| {
        left_position
            .cmp(right_position)
            .then_with(|| match (left, right) {
                (Edit::Close(left), Edit::Close(right)) => right.span.start.cmp(&left.span.start),
                (Edit::Close(_), Edit::Open(_)) => std::cmp::Ordering::Less,
                (Edit::Open(_), Edit::Close(_)) => std::cmp::Ordering::Greater,
                (Edit::Open(left), Edit::Open(right)) => right.span.end.cmp(&left.span.end),
            })
    });
    let mut position = 0;
    for (edit_position, edit) in edits {
        output.extend_from_slice(&source[position..edit_position]);
        match edit {
            Edit::Open(insertion) => {
                write!(output, "<\\with|mls-id|{}>", insertion.id)
                    .expect("writing to a byte vector cannot fail");
            }
            Edit::Close(_) => output.extend_from_slice(b"</with>"),
        }
        position = edit_position;
    }
    output.extend_from_slice(&source[position..]);
    Ok(output)
}

fn create_temporary(parent: &Path, file_name: &str) -> io::Result<(fs::File, TemporaryPath)> {
    loop {
        let path = parent.join(format!(".{file_name}.mls-{}.tmp", mls_core::BlockId::new()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((file, TemporaryPath(Some(path)))),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

struct TemporaryPath(Option<PathBuf>);

impl TemporaryPath {
    fn path(&self) -> &Path {
        self.0.as_deref().expect("temporary path is armed")
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze, parse};
    use proptest::prelude::*;

    #[test]
    fn insertion_preserves_every_original_byte() {
        let source = b"<\\body>before \xff <\\theorem>x</theorem> after</body>";
        let document = parse(source).unwrap();
        let analysis = analyze(&document);
        let path = std::env::temp_dir().join(format!("mls-writer-{}.tm", analysis.blocks[0].id));
        fs::write(&path, source).unwrap();
        write_ids_atomic(&path, source, &analysis.insertions).unwrap();
        let written = fs::read(&path).unwrap();
        let mut expected =
            format!("<\\body><\\with|mls-id|{}>before ", analysis.blocks[0].id).into_bytes();
        expected.push(0xff);
        expected.extend_from_slice(format!("</with> <\\with|mls-id|{}><\\theorem>x</theorem></with> <\\with|mls-id|{}>after</with></body>", analysis.blocks[1].id, analysis.blocks[2].id).as_bytes());
        assert_eq!(written, expected);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn nested_and_adjacent_insertions_have_stable_wrapper_order() {
        let source = b"abcdef";
        let outer = mls_core::BlockId::new();
        let left = mls_core::BlockId::new();
        let right = mls_core::BlockId::new();
        let insertions = [
            IdInsertion {
                id: right,
                span: crate::Span { start: 3, end: 5 },
            },
            IdInsertion {
                id: outer,
                span: crate::Span { start: 0, end: 6 },
            },
            IdInsertion {
                id: left,
                span: crate::Span { start: 1, end: 3 },
            },
        ];

        let actual = apply_insertions(source, &insertions).unwrap();
        let expected = format!(
            "<\\with|mls-id|{outer}>a<\\with|mls-id|{left}>bc</with><\\with|mls-id|{right}>de</with>f</with>"
        );
        assert_eq!(actual, expected.as_bytes());
    }

    #[test]
    fn crossing_and_duplicate_spans_are_rejected_without_touching_source() {
        let source = b"abcdef";
        let path = std::env::temp_dir().join(format!(
            "mls-invalid-insertion-{}.tm",
            mls_core::BlockId::new()
        ));
        fs::write(&path, source).unwrap();
        let id = mls_core::BlockId::new();
        let crossing = [
            IdInsertion {
                id,
                span: crate::Span { start: 0, end: 4 },
            },
            IdInsertion {
                id: mls_core::BlockId::new(),
                span: crate::Span { start: 2, end: 6 },
            },
        ];
        assert!(matches!(
            write_ids_atomic(&path, source, &crossing),
            Err(WriteError::InvalidInsertion)
        ));
        let duplicate = [crossing[0].clone(), crossing[0].clone()];
        assert!(matches!(
            write_ids_atomic(&path, source, &duplicate),
            Err(WriteError::InvalidInsertion)
        ));
        assert_eq!(fs::read(&path).unwrap(), source);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn generated_syntax_is_validated_before_replacing_source() {
        let source = b"<\\body>foo\\ bar</body>";
        parse(source).unwrap();
        let path = std::env::temp_dir().join(format!(
            "mls-invalid-output-{}.tm",
            mls_core::BlockId::new()
        ));
        fs::write(&path, source).unwrap();
        let splits_escaped_space = IdInsertion {
            id: mls_core::BlockId::new(),
            span: crate::Span { start: 7, end: 11 },
        };

        assert!(matches!(
            write_ids_atomic(&path, source, &[splits_escaped_space]),
            Err(WriteError::InvalidGeneratedSyntax)
        ));
        assert_eq!(fs::read(&path).unwrap(), source);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn no_op_does_not_replace_the_file() {
        let source = b"unchanged";
        let path = std::env::temp_dir().join(format!("mls-no-op-{}.tm", mls_core::BlockId::new()));
        fs::write(&path, source).unwrap();
        #[cfg(unix)]
        let inode_before = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(&path).unwrap().ino()
        };
        write_ids_atomic(&path, source, &[]).unwrap();
        assert_eq!(fs::read(&path).unwrap(), source);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(&path).unwrap().ino(), inode_before);
        }
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn armed_temporary_path_cleans_up_automatically() {
        let suffix = mls_core::BlockId::new().to_string();
        let (file, temporary) = create_temporary(&std::env::temp_dir(), &suffix).unwrap();
        let path = temporary.path().to_owned();
        assert!(path.exists());
        drop(file);
        drop(temporary);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn replacement_preserves_file_permissions() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let path =
            std::env::temp_dir().join(format!("mls-permissions-{}.tm", mls_core::BlockId::new()));
        fs::write(&path, b"abcdef").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let insertion = IdInsertion {
            id: mls_core::BlockId::new(),
            span: crate::Span { start: 0, end: 6 },
        };
        write_ids_atomic(&path, b"abcdef", &[insertion]).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_target_is_left_untouched() {
        use std::os::unix::fs::symlink;

        let suffix = mls_core::BlockId::new();
        let target = std::env::temp_dir().join(format!("mls-link-target-{suffix}.tm"));
        let link = std::env::temp_dir().join(format!("mls-link-{suffix}.tm"));
        fs::write(&target, b"abcdef").unwrap();
        symlink(&target, &link).unwrap();
        let insertion = IdInsertion {
            id: mls_core::BlockId::new(),
            span: crate::Span { start: 0, end: 6 },
        };
        assert!(matches!(
            write_ids_atomic(&link, b"abcdef", &[insertion]),
            Err(WriteError::SymlinkTarget)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"abcdef");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_file(link).unwrap();
        fs::remove_file(target).unwrap();
    }

    #[test]
    fn refuses_to_replace_a_stale_source() {
        let source = b"<\\body><\\theorem>x</theorem></body>";
        let analysis = analyze(&parse(source).unwrap());
        let path = std::env::temp_dir().join(format!("mls-stale-{}.tm", analysis.blocks[0].id));
        fs::write(&path, b"changed").unwrap();
        assert!(matches!(
            write_ids_atomic(&path, source, &analysis.insertions),
            Err(WriteError::StaleSource)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"changed");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn nested_blocks_get_distinct_ids_without_changing_parent_hash() {
        let source = b"<\\body><\\theorem>Claim.<\\equation>x=x</equation></theorem></body>";
        let document = parse(source).unwrap();
        let before = analyze(&document);
        assert_ne!(before.blocks[0].id, before.blocks[1].id);
        let path = std::env::temp_dir().join(format!("mls-nested-{}.tm", before.blocks[0].id));
        fs::write(&path, source).unwrap();
        write_ids_atomic(&path, source, &before.insertions).unwrap();
        let persisted = fs::read(&path).unwrap();
        let after = analyze(&parse(&persisted).unwrap());
        assert_eq!(after.blocks[0].id, before.blocks[0].id);
        assert_eq!(after.blocks[1].id, before.blocks[1].id);
        assert_eq!(after.blocks[0].text_hash, before.blocks[0].text_hash);
        fs::remove_file(path).unwrap();
    }

    proptest! {
        #[test]
        fn identity_survives_edit_move_and_reopen(left in "[a-z]{1,20}", right in "[a-z]{1,20}") {
            let first = format!("<\\body><\\theorem>{left}</theorem><\\definition>{right}</definition></body>");
            let path = std::env::temp_dir().join(format!("mls-property-{}.tm", mls_core::BlockId::new()));
            fs::write(&path, first.as_bytes()).unwrap();
            let first_doc = parse(first.as_bytes()).unwrap();
            let first_analysis = analyze(&first_doc);
            write_ids_atomic(&path, first.as_bytes(), &first_analysis.insertions).unwrap();
            let persisted = fs::read(&path).unwrap();
            let reopened = analyze(&parse(&persisted).unwrap());
            prop_assert_eq!(reopened.blocks[0].id, first_analysis.blocks[0].id);
            prop_assert_eq!(reopened.blocks[1].id, first_analysis.blocks[1].id);

            let theorem = format!("<\\with|mls-id|{}><\\theorem>{left} edited</theorem></with>", reopened.blocks[0].id);
            let definition = format!("<\\with|mls-id|{}><\\definition>{right}</definition></with>", reopened.blocks[1].id);
            let moved = format!("<\\body>{definition}{theorem}</body>");
            let moved_analysis = analyze(&parse(moved.as_bytes()).unwrap());
            prop_assert_eq!(moved_analysis.blocks[0].id, reopened.blocks[1].id);
            prop_assert_eq!(moved_analysis.blocks[1].id, reopened.blocks[0].id);
            fs::remove_file(path).unwrap();
        }
    }
}
