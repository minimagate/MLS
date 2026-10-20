//! Deterministic dependency-ordered Lean shadow files and exact source spans.

use mls_core::BlockId;
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
};

/// One generated declaration and its block dependencies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShadowDeclaration {
    /// Stable block identity.
    pub block_id: BlockId,
    /// Exact admitted Lean declaration source.
    pub source: String,
    /// Block IDs that must occur earlier in this section.
    pub dependencies: Vec<BlockId>,
}

/// One document section in a multi-file shadow project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShadowSection {
    /// Lean module name and generated filename stem.
    pub name: String,
    /// Declarations owned by this section.
    pub declarations: Vec<ShadowDeclaration>,
}

/// Exact byte and line range occupied by a declaration (excluding its block comment).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationSpan {
    /// Stable block identity.
    pub block_id: BlockId,
    /// Inclusive UTF-8 byte offset.
    pub start_byte: usize,
    /// Exclusive UTF-8 byte offset.
    pub end_byte: usize,
    /// Inclusive one-based line.
    pub start_line: usize,
    /// Inclusive one-based final line.
    pub end_line: usize,
}

/// A complete generated section file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShadowFile {
    /// UTF-8 Lean source.
    pub source: String,
    /// Declaration ranges used for diagnostic mapping.
    pub spans: Vec<DeclarationSpan>,
}

/// Invalid dependency graph.
#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum ShadowError {
    /// A block ID occurred more than once.
    #[error("duplicate shadow block ID: {0}")]
    Duplicate(BlockId),
    /// A dependency was not included in this section.
    #[error("block {block} depends on missing block {dependency}")]
    MissingDependency {
        /// Referencing block.
        block: BlockId,
        /// Absent block identity.
        dependency: BlockId,
    },
    /// Dependencies contain a cycle.
    #[error("shadow dependencies contain a cycle")]
    Cycle,
    /// A section name occurred more than once.
    #[error("duplicate shadow section name: {0}")]
    DuplicateSection(String),
    /// A section name cannot safely be used as a Lean module identifier.
    #[error("invalid shadow section name: {0}")]
    InvalidSection(String),
}

/// Place a section of one document under `.mls/Shadow/<module>/`, the document's own directory in
/// the shared Shadow library. `module` is the document's Lean module component.
pub fn shadow_path_for_document(
    document: &Path,
    module: &str,
    section: &str,
) -> Result<PathBuf, ShadowError> {
    validate_section(module)?;
    validate_section(section)?;
    Ok(document
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".mls")
        .join("Shadow")
        .join(module)
        .join(format!("{section}.lean")))
}

/// Topologically order declarations and generate a reproducible Lean section.
pub fn generate_shadow(
    section: &str,
    declarations: &[ShadowDeclaration],
) -> Result<ShadowFile, ShadowError> {
    validate_section(section)?;
    let mut index = HashMap::new();
    for (position, declaration) in declarations.iter().enumerate() {
        if index.insert(declaration.block_id, position).is_some() {
            return Err(ShadowError::Duplicate(declaration.block_id));
        }
    }
    let mut indegree = vec![0_usize; declarations.len()];
    let mut downstream = vec![Vec::new(); declarations.len()];
    for (position, declaration) in declarations.iter().enumerate() {
        for dependency in &declaration.dependencies {
            let Some(&dependency_position) = index.get(dependency) else {
                return Err(ShadowError::MissingDependency {
                    block: declaration.block_id,
                    dependency: *dependency,
                });
            };
            indegree[position] += 1;
            downstream[dependency_position].push(position);
        }
    }
    let mut ready: BTreeSet<_> = indegree
        .iter()
        .enumerate()
        .filter_map(|(i, degree)| (*degree == 0).then_some(i))
        .collect();
    let mut order = Vec::with_capacity(declarations.len());
    while let Some(position) = ready.pop_first() {
        order.push(position);
        for &next in &downstream[position] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                ready.insert(next);
            }
        }
    }
    if order.len() != declarations.len() {
        return Err(ShadowError::Cycle);
    }

    let mut source = format!(
        "import Mathlib\nimport MlsExtract\nset_option autoImplicit false\n\n-- section:{section}\n\n"
    );
    let mut spans = Vec::with_capacity(order.len());
    for position in order {
        let declaration = &declarations[position];
        source.push_str("-- block:");
        source.push_str(&declaration.block_id.to_string());
        source.push('\n');
        let start_byte = source.len();
        let start_line = source.bytes().filter(|byte| *byte == b'\n').count() + 1;
        source.push_str(declaration.source.trim_end());
        let end_byte = source.len();
        let end_line = source[..end_byte]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        source.push_str("\n\n");
        spans.push(DeclarationSpan {
            block_id: declaration.block_id,
            start_byte,
            end_byte,
            start_line,
            end_line,
        });
    }
    Ok(ShadowFile { source, spans })
}

/// Generate section modules in dependency order, adding imports for earlier sections.
///
/// The sections are modules `Shadow.<module>.<section>`, where `module` names one document's
/// directory in the shared Shadow library. Dependencies may cross sections. The returned
/// `(name, file)` pairs are ordered so every imported generated module appears earlier.
pub fn generate_shadow_project(
    module: &str,
    sections: &[ShadowSection],
) -> Result<Vec<(String, ShadowFile)>, ShadowError> {
    validate_section(module)?;
    let mut section_names = HashMap::new();
    let mut block_sections = HashMap::new();
    for (section_index, section) in sections.iter().enumerate() {
        validate_section(&section.name)?;
        if section_names
            .insert(section.name.as_str(), section_index)
            .is_some()
        {
            return Err(ShadowError::DuplicateSection(section.name.clone()));
        }
        for declaration in &section.declarations {
            if block_sections
                .insert(declaration.block_id, section_index)
                .is_some()
            {
                return Err(ShadowError::Duplicate(declaration.block_id));
            }
        }
    }
    let mut section_dependencies = vec![BTreeSet::new(); sections.len()];
    for (section_index, section) in sections.iter().enumerate() {
        for declaration in &section.declarations {
            for dependency in &declaration.dependencies {
                let Some(&dependency_section) = block_sections.get(dependency) else {
                    return Err(ShadowError::MissingDependency {
                        block: declaration.block_id,
                        dependency: *dependency,
                    });
                };
                if dependency_section != section_index {
                    section_dependencies[section_index].insert(dependency_section);
                }
            }
        }
    }
    let order = topo_sets(&section_dependencies)?;
    let mut output = Vec::with_capacity(sections.len());
    for section_index in order {
        let section = &sections[section_index];
        // Local generation ignores dependencies satisfied by imported section modules.
        let local_ids: BTreeSet<_> = section.declarations.iter().map(|d| d.block_id).collect();
        let local: Vec<_> = section
            .declarations
            .iter()
            .cloned()
            .map(|mut declaration| {
                declaration
                    .dependencies
                    .retain(|dependency| local_ids.contains(dependency));
                declaration
            })
            .collect();
        let mut generated = generate_shadow(&section.name, &local)?;
        let imports: String = section_dependencies[section_index]
            .iter()
            .map(|dependency| format!("import Shadow.{module}.{}\n", sections[*dependency].name))
            .collect();
        if !imports.is_empty() {
            let insertion = "import Mathlib\n".len();
            generated.source.insert_str(insertion, &imports);
            for span in &mut generated.spans {
                span.start_byte += imports.len();
                span.end_byte += imports.len();
                let lines = imports.bytes().filter(|byte| *byte == b'\n').count();
                span.start_line += lines;
                span.end_line += lines;
            }
        }
        output.push((section.name.clone(), generated));
    }
    Ok(output)
}

fn validate_section(section: &str) -> Result<(), ShadowError> {
    let mut bytes = section.bytes();
    let valid = bytes.next().is_some_and(|byte| byte.is_ascii_uppercase())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(ShadowError::InvalidSection(section.to_owned()))
    }
}

fn topo_sets(dependencies: &[BTreeSet<usize>]) -> Result<Vec<usize>, ShadowError> {
    let mut indegree: Vec<_> = dependencies.iter().map(BTreeSet::len).collect();
    let mut downstream = vec![Vec::new(); dependencies.len()];
    for (node, deps) in dependencies.iter().enumerate() {
        for dependency in deps {
            downstream[*dependency].push(node);
        }
    }
    let mut ready: BTreeSet<_> = indegree
        .iter()
        .enumerate()
        .filter_map(|(i, degree)| (*degree == 0).then_some(i))
        .collect();
    let mut order = Vec::with_capacity(dependencies.len());
    while let Some(node) = ready.pop_first() {
        order.push(node);
        for next in &downstream[node] {
            indegree[*next] -= 1;
            if indegree[*next] == 0 {
                ready.insert(*next);
            }
        }
    }
    if order.len() == dependencies.len() {
        Ok(order)
    } else {
        Err(ShadowError::Cycle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(value: usize) -> BlockId {
        format!("00000000-0000-4000-8000-{value:012}")
            .parse()
            .unwrap()
    }

    #[test]
    fn every_generated_dependency_precedes_its_consumer() {
        // Exercise many deterministic DAGs without coupling the invariant to an implementation.
        for mask in 0_u16..256 {
            let mut declarations = Vec::new();
            for node in 0..4 {
                let dependencies = (0..node)
                    .filter(|dep| mask & (1 << (node * 2 + dep)) != 0)
                    .map(block)
                    .collect();
                declarations.push(ShadowDeclaration {
                    block_id: block(node),
                    source: format!("theorem t{node} : True := by trivial"),
                    dependencies,
                });
            }
            declarations.reverse();
            let generated = generate_shadow("S", &declarations).expect("DAG should generate");
            for declaration in &declarations {
                let own = generated
                    .source
                    .find(&format!("-- block:{}", declaration.block_id))
                    .unwrap();
                for dependency in &declaration.dependencies {
                    assert!(
                        generated
                            .source
                            .find(&format!("-- block:{dependency}"))
                            .unwrap()
                            < own
                    );
                }
            }
        }
    }

    #[test]
    fn cycle_is_rejected() {
        let declarations = vec![
            ShadowDeclaration {
                block_id: block(1),
                source: "def a := 1".into(),
                dependencies: vec![block(2)],
            },
            ShadowDeclaration {
                block_id: block(2),
                source: "def b := 1".into(),
                dependencies: vec![block(1)],
            },
        ];
        assert_eq!(generate_shadow("S", &declarations), Err(ShadowError::Cycle));
    }

    #[test]
    fn sections_are_ordered_and_import_cross_section_dependencies() {
        let sections = vec![
            ShadowSection {
                name: "Later".into(),
                declarations: vec![ShadowDeclaration {
                    block_id: block(2),
                    source: "theorem b : a = a := rfl".into(),
                    dependencies: vec![block(1)],
                }],
            },
            ShadowSection {
                name: "Earlier".into(),
                declarations: vec![ShadowDeclaration {
                    block_id: block(1),
                    source: "def a := 1".into(),
                    dependencies: vec![],
                }],
            },
        ];
        let generated = generate_shadow_project("Doc_notes_00000000", &sections).unwrap();
        assert_eq!(generated[0].0, "Earlier");
        assert_eq!(generated[1].0, "Later");
        assert!(
            generated[1]
                .1
                .source
                .contains("import Shadow.Doc_notes_00000000.Earlier\n")
        );
    }

    #[test]
    fn invalid_module_names_are_refused() {
        let sections = vec![ShadowSection {
            name: "Section1".into(),
            declarations: vec![],
        }];
        for module in ["doc", "Doc.Nested", "Doc-x", "Bad\nimport Evil", ""] {
            assert!(
                matches!(
                    generate_shadow_project(module, &sections),
                    Err(ShadowError::InvalidSection(_))
                ),
                "{module:?}"
            );
        }
    }

    #[test]
    fn invalid_section_names_cannot_reach_paths_comments_or_imports() {
        let declaration = ShadowDeclaration {
            block_id: block(1),
            source: "def x := 1".into(),
            dependencies: vec![],
        };
        assert!(matches!(
            generate_shadow("Bad\nimport Evil", &[declaration]),
            Err(ShadowError::InvalidSection(_))
        ));
        assert!(matches!(
            shadow_path_for_document(Path::new("notes.tm"), "Doc_notes", "a-b"),
            Err(ShadowError::InvalidSection(_))
        ));
        assert!(matches!(
            shadow_path_for_document(Path::new("notes.tm"), "../escape", "Section1"),
            Err(ShadowError::InvalidSection(_))
        ));
        assert_eq!(
            shadow_path_for_document(Path::new("/d/notes.tm"), "Doc_notes", "Section1").unwrap(),
            PathBuf::from("/d/.mls/Shadow/Doc_notes/Section1.lean")
        );
    }
}
