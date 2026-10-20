//! Property checks of the layout-independent block text. No TeXmacs binary is needed.
//!
//! A random tree of words, spaces, paragraph breaks and tags is rendered in random layouts:
//! spaces become any run with at most one line feed, long and short forms alternate, and long
//! arguments get leading and trailing whitespace. The canonical text must always equal the text
//! of the tree itself. Changing a word, a tag name, the argument count or a paragraph split must
//! change it.

use mls_tm::analyze;
use mls_tm::parse;
use proptest::collection::vec;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Item {
    Word(String),
    Space,
    Break,
    Tag(String, Vec<Vec<Item>>),
}

/// Remove leading and trailing spaces and merge runs of spaces: a tree never starts or ends an
/// argument with a space, because TeXmacs keeps or trims it depending on the form.
fn normalize(items: Vec<Item>) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    for item in items {
        if matches!(item, Item::Space) && !matches!(out.last(), Some(Item::Word(_) | Item::Tag(..)))
        {
            continue;
        }
        if matches!(item, Item::Space) && matches!(out.last(), Some(Item::Space)) {
            continue;
        }
        out.push(item);
    }
    while matches!(out.last(), Some(Item::Space)) {
        out.pop();
    }
    out
}

fn items(depth: u32) -> BoxedStrategy<Vec<Item>> {
    let word = "[a-z]{1,5}".prop_map(Item::Word);
    let leaf = prop_oneof![4 => word, 2 => Just(Item::Space)];
    if depth == 0 {
        return vec(leaf, 0..6).prop_map(normalize).boxed();
    }
    let tag =
        ("[a-z]{1,4}", vec(items(depth - 1), 0..3)).prop_map(|(name, args)| Item::Tag(name, args));
    vec(prop_oneof![4 => leaf, 1 => tag], 0..7)
        .prop_map(normalize)
        .boxed()
}

/// The canonical text a tree must have, in its single-line form.
fn ideal(items: &[Item]) -> String {
    items
        .iter()
        .map(|item| match item {
            Item::Word(word) => word.clone(),
            Item::Space => " ".into(),
            Item::Break => "\n\n".into(),
            Item::Tag(name, args) => {
                let args: String = args.iter().map(|arg| format!("|{}", ideal(arg))).collect();
                format!("<{name}{args}>")
            }
        })
        .collect()
}

/// Deterministic layout choices drawn from a byte stream.
struct Layout {
    choices: Vec<u8>,
    position: usize,
}

impl Layout {
    fn pick(&mut self, count: usize) -> usize {
        if self.choices.is_empty() {
            return 0;
        }
        let byte = usize::from(self.choices[self.position % self.choices.len()]);
        self.position += 1;
        byte % count
    }

    fn render(&mut self, items: &[Item], out: &mut String) {
        for item in items {
            match item {
                Item::Word(word) => out.push_str(word),
                Item::Space => out.push_str(SPACES[self.pick(SPACES.len())]),
                Item::Break => out.push_str(BREAKS[self.pick(BREAKS.len())]),
                Item::Tag(name, args) => self.tag(name, args, out),
            }
        }
    }

    fn tag(&mut self, name: &str, args: &[Vec<Item>], out: &mut String) {
        if self.pick(2) == 0 || args.is_empty() {
            out.push('<');
            out.push_str(name);
            for arg in args {
                out.push('|');
                self.render(arg, out);
            }
            out.push('>');
            return;
        }
        out.push_str(&format!("<\\{name}>"));
        for (index, arg) in args.iter().enumerate() {
            if index > 0 {
                out.push_str(&format!("<|{name}>"));
            }
            out.push_str(LEADING[self.pick(LEADING.len())]);
            self.render(arg, out);
            out.push_str(LEADING[self.pick(LEADING.len())]);
        }
        out.push_str(&format!("</{name}>"));
    }
}

/// Runs of spaces and tabs, with at most one line feed: TeXmacs reads each as one space.
const SPACES: [&str; 7] = ["  ", " ", "\t", "\n  ", "\n", " \r\n\t", "\n\t "];
/// Runs with at least two line feeds: TeXmacs reads each as a paragraph break.
const BREAKS: [&str; 3] = ["\n\n", " \n \n", "\n\n\t"];
/// Whitespace at the edge of a long-form argument, which TeXmacs trims.
const LEADING: [&str; 5] = ["", "\n  ", "\n\n", "  \n    ", "\t"];

fn document(items: &[Item], choices: &[u8]) -> Vec<u8> {
    let mut layout = Layout {
        choices: choices.to_vec(),
        position: 0,
    };
    let mut body = String::new();
    layout.tag("theorem", &[items.to_vec()], &mut body);
    format!("<TeXmacs|2.1.5>\n\n<\\body>\n{body}\n</body>\n").into_bytes()
}

/// Canonical text of the first block, which is the outer theorem.
fn first_block_text(source: &[u8]) -> String {
    let document = parse(source).unwrap();
    let analysis = analyze(&document);
    let block = analysis.blocks.first().expect("the theorem is a block");
    String::from_utf8(analysis.canonical_text(block.id).unwrap().to_vec()).unwrap()
}

/// One tree edit. Each changes the canonical text of the tree.
#[derive(Clone, Copy, Debug)]
enum Edit {
    ChangeWord,
    RenameTag,
    AddArgument,
    DropArgument,
    SplitParagraph,
}

fn apply(items: &mut [Item], edit: Edit, target: &mut usize) -> bool {
    for item in items {
        let hit = match (edit, &*item) {
            (Edit::ChangeWord, Item::Word(_))
            | (Edit::RenameTag, Item::Tag(..))
            | (Edit::AddArgument, Item::Tag(..))
            | (Edit::SplitParagraph, Item::Space) => true,
            (Edit::DropArgument, Item::Tag(_, args)) => args.len() > 1,
            _ => false,
        };
        if hit {
            if *target == 0 {
                match item {
                    Item::Word(word) => {
                        let first = if word.starts_with('a') { "b" } else { "a" };
                        word.replace_range(0..1, first);
                    }
                    Item::Tag(name, args) => match edit {
                        Edit::RenameTag => name.push('q'),
                        Edit::AddArgument => args.push(Vec::new()),
                        _ => {
                            args.pop();
                        }
                    },
                    Item::Space => *item = Item::Break,
                    Item::Break => {}
                }
                return true;
            }
            *target -= 1;
        }
        if let Item::Tag(_, args) = item {
            for arg in args {
                if apply(arg, edit, target) {
                    return true;
                }
            }
        }
    }
    false
}

fn count_sites(items: &[Item], edit: Edit) -> usize {
    items
        .iter()
        .map(|item| {
            let own = match (edit, item) {
                (Edit::ChangeWord, Item::Word(_))
                | (Edit::RenameTag, Item::Tag(..))
                | (Edit::AddArgument, Item::Tag(..))
                | (Edit::SplitParagraph, Item::Space) => 1,
                (Edit::DropArgument, Item::Tag(_, args)) if args.len() > 1 => 1,
                _ => 0,
            };
            let nested = match item {
                Item::Tag(_, args) => args.iter().map(|arg| count_sites(arg, edit)).sum(),
                _ => 0,
            };
            own + nested
        })
        .sum()
}

fn edit_from(choice: u8) -> Edit {
    [
        Edit::ChangeWord,
        Edit::RenameTag,
        Edit::AddArgument,
        Edit::DropArgument,
        Edit::SplitParagraph,
    ][usize::from(choice) % 5]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn any_layout_reads_as_the_tree_it_came_from(
        tree in items(2),
        choices in vec(any::<u8>(), 0..256),
    ) {
        let source = document(&tree, &choices);
        prop_assert_eq!(
            first_block_text(&source),
            format!("<theorem|{}>", ideal(&tree))
        );
    }

    #[test]
    fn a_different_tree_gets_different_text_in_any_layouts(
        tree in items(2),
        edit_choice in any::<u8>(),
        site in any::<u8>(),
        choices in vec(any::<u8>(), 0..256),
        other_choices in vec(any::<u8>(), 0..256),
    ) {
        let edit = edit_from(edit_choice);
        let sites = count_sites(&tree, edit);
        prop_assume!(sites > 0);
        let mut edited = tree.clone();
        let mut target = usize::from(site) % sites;
        prop_assert!(apply(&mut edited, edit, &mut target));
        prop_assume!(ideal(&edited) != ideal(&tree));
        let original = first_block_text(&document(&tree, &choices));
        let changed = first_block_text(&document(&edited, &other_choices));
        prop_assert_eq!(&changed, &format!("<theorem|{}>", ideal(&edited)));
        prop_assert_ne!(original, changed, "edit {:?} did not change the text", edit);
    }
}

/// Wrap a body fragment in a minimal TeXmacs document.
fn in_body(fragment: &str) -> Vec<u8> {
    format!("<TeXmacs|2.1.5>\n\n<\\body>\n{fragment}\n</body>\n").into_bytes()
}

#[test]
fn reflowing_a_wrapped_long_form_argument_keeps_its_text() {
    let compact = "<\\theorem>Truth holds for every <markup|finite> set.<label|thm:x></theorem>";
    let wrapped = "<\\theorem>\n    Truth holds for\n    every <markup|finite>\n      set.<label|thm:x>\n</theorem>";
    let short = "<theorem|Truth holds for every <markup|finite> set.<label|thm:x>>";
    assert_eq!(
        first_block_text(&in_body(compact)),
        first_block_text(&in_body(short))
    );
    assert_eq!(
        first_block_text(&in_body(wrapped)),
        first_block_text(&in_body(short))
    );
}

#[test]
fn a_single_line_feed_is_a_space_but_a_blank_line_is_a_paragraph() {
    let spaced = in_body("<theorem|A B>");
    let wrapped = in_body("<theorem|A\nB>");
    let split = in_body("<theorem|A\n\nB>");
    assert_eq!(first_block_text(&spaced), first_block_text(&wrapped));
    assert_ne!(first_block_text(&spaced), first_block_text(&split));
}
