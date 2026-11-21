//! `text_marks_at` reads the marks on one character of a text through
//! the text and mark indexes (galatea issue 14404). These tests build
//! texts of random edits, marks, unmarks and merges, in every encoding,
//! and compare it at every index with what `marks` says covers that
//! index, and, where an index counts elements, with `get_marks`.

use std::collections::BTreeMap;

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ReadDoc, ScalarValue, TextEncoding, ROOT};

/// A small deterministic generator, so a failure names its seed.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

const CHARS: [&str; 6] = ["a", "b", "é", "π", "🎨", "\u{10348}"];
const NAMES: [&str; 3] = ["bold", "run", "font"];
const EXPANDS: [ExpandMark; 4] = [
    ExpandMark::Before,
    ExpandMark::After,
    ExpandMark::Both,
    ExpandMark::None,
];

/// One random edit of the text `text` in `doc`: a splice, a deletion,
/// a mark, an unmark or a block.
fn edit(doc: &mut AutoCommit, text: &automerge::ObjId, rng: &mut Rng) {
    let len = doc.length(text);
    match rng.below(10) {
        0..=3 => {
            let s: String = (0..1 + rng.below(4))
                .map(|_| CHARS[rng.below(CHARS.len())])
                .collect();
            let at = rng.below(len + 1);
            doc.splice_text(text, at, 0, &s).unwrap();
        }
        4 if len > 0 => {
            let at = rng.below(len);
            let del = 1 + rng.below((len - at).min(3));
            doc.splice_text(text, at, del as isize, "").unwrap();
        }
        5..=7 if len > 0 => {
            let start = rng.below(len);
            let end = start + 1 + rng.below(len - start);
            let name = NAMES[rng.below(NAMES.len())];
            let value = format!("v{}", rng.below(3));
            let mark = Mark::new(name.to_owned(), value, start, end);
            doc.mark(text, mark, EXPANDS[rng.below(EXPANDS.len())])
                .unwrap();
        }
        8 if len > 0 => {
            let start = rng.below(len);
            let end = start + 1 + rng.below(len - start);
            let name = NAMES[rng.below(NAMES.len())];
            doc.unmark(text, name, start, end, EXPANDS[rng.below(EXPANDS.len())])
                .unwrap();
        }
        _ => {
            let at = rng.below(len + 1);
            doc.split_block(text, at).unwrap();
        }
    }
}

/// The marks on each index of `text`, from the ranges `marks` gives.
fn expected(
    doc: &AutoCommit,
    text: &automerge::ObjId,
    index: usize,
) -> BTreeMap<String, ScalarValue> {
    (doc.marks(text).unwrap().into_iter())
        .filter(|mark| mark.start <= index && index < mark.end)
        .map(|mark| (mark.name().to_owned(), mark.value().clone()))
        .collect()
}

fn read(doc: &AutoCommit, text: &automerge::ObjId, index: usize) -> BTreeMap<String, ScalarValue> {
    (doc.text_marks_at(text, index).unwrap().iter())
        .map(|(name, value)| (name.to_owned(), value.clone()))
        .collect()
}

/// Every index of `text`, and one past its end, read through the
/// indexes, against `marks` and, where an index counts elements (code
/// points, with no block), `get_marks`.
fn check(doc: &AutoCommit, text: &automerge::ObjId, encoding: TextEncoding, seed: u64) {
    let len = doc.length(text);
    let elements = encoding == TextEncoding::UnicodeCodePoint;
    for index in 0..len {
        let got = read(doc, text, index);
        assert_eq!(
            got,
            expected(doc, text, index),
            "seed {seed}, {encoding:?}, index {index}"
        );
        if elements {
            let walked: BTreeMap<_, _> = (doc.get_marks(text, index, None).unwrap().iter())
                .map(|(name, value)| (name.to_owned(), value.clone()))
                .collect();
            assert_eq!(got, walked, "seed {seed}, get_marks at {index}");
        }
    }
    assert!(read(doc, text, len).is_empty(), "seed {seed}: past the end");
}

#[test]
fn text_marks_at_agrees_with_marks_at_every_index() {
    for encoding in [
        TextEncoding::UnicodeCodePoint,
        TextEncoding::Utf16CodeUnit,
        TextEncoding::Utf8CodeUnit,
    ] {
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let mut doc = AutoCommit::new_with_encoding(encoding);
            let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
            for _ in 0..30 {
                edit(&mut doc, &text, &mut rng);
            }
            check(&doc, &text, encoding, seed);
            // Concurrent edits, then a merge.
            let mut other = doc.fork();
            for _ in 0..10 {
                edit(&mut doc, &text, &mut rng);
                edit(&mut other, &text, &mut rng);
            }
            doc.merge(&mut other).unwrap();
            check(&doc, &text, encoding, seed);
        }
    }
}

#[test]
fn text_marks_at_reads_the_heads_a_document_is_isolated_to() {
    let mut rng = Rng(7);
    let mut doc = AutoCommit::new();
    let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
    for _ in 0..30 {
        edit(&mut doc, &text, &mut rng);
    }
    let heads = doc.get_heads();
    let then = doc.fork();
    for _ in 0..20 {
        edit(&mut doc, &text, &mut rng);
    }
    doc.isolate(&heads);
    for index in 0..=then.length(&text) {
        assert_eq!(
            read(&doc, &text, index),
            read(&then, &text, index),
            "index {index}"
        );
    }
}

#[test]
fn text_marks_at_is_empty_for_a_text_with_no_marks_and_for_a_list() {
    let mut doc = AutoCommit::new();
    let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "abc").unwrap();
    assert!(read(&doc, &text, 1).is_empty());
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, "x").unwrap();
    assert!(read(&doc, &list, 0).is_empty());
}
