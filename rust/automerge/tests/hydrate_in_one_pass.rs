//! `hydrate_in_one_pass` reads what `hydrate` reads with no heads, for
//! every kind of object and value, after deletions, overwrites,
//! increments, merges with conflicts, and a save and load.
use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjType, ReadDoc, ScalarValue, ROOT};

fn same(doc: &AutoCommit) {
    assert_eq!(doc.hydrate_in_one_pass(), doc.hydrate(&ROOT, None).unwrap());
}

/// A document holding one of everything, some of it changed after.
fn everything(actor: &str) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(actor.as_bytes()));
    doc.put(&ROOT, "version", 2).unwrap();
    doc.put(&ROOT, "name", "drawing").unwrap();
    doc.put(&ROOT, "bytes", ScalarValue::Bytes(vec![1, 2, 3]))
        .unwrap();
    doc.put(&ROOT, "count", ScalarValue::counter(5)).unwrap();
    doc.increment(&ROOT, "count", 3).unwrap();
    let nodes = doc.put_object(&ROOT, "nodes", ObjType::Map).unwrap();
    for i in 0..40 {
        let node = doc
            .put_object(&nodes, format!("n{i:02}"), ObjType::Map)
            .unwrap();
        doc.put(&node, "type", "path").unwrap();
        doc.put(&node, "at", i as i64).unwrap();
        let attrs = doc.put_object(&node, "attrs", ObjType::Map).unwrap();
        if i % 3 == 0 {
            doc.put(&attrs, "lineWidth", 500).unwrap();
        }
        let list = doc.put_object(&node, "items", ObjType::List).unwrap();
        for j in 0..(i % 4) {
            doc.insert(&list, j, j as i64).unwrap();
        }
        if i % 5 == 0 {
            let inner = doc.insert_object(&list, 0, ObjType::Map).unwrap();
            doc.put(&inner, "deep", true).unwrap();
        }
        if i % 7 == 0 {
            let text = doc.put_object(&node, "lines", ObjType::Text).unwrap();
            doc.splice_text(&text, 0, 0, "hello").unwrap();
            // Marks are ops of the text, read with it.
            doc.mark(
                &text,
                Mark::new("bold".to_owned(), true, 0, 3),
                ExpandMark::After,
            )
            .unwrap();
            doc.split_block(&text, 2).unwrap();
        }
    }
    doc.put_object(&ROOT, "empty map", ObjType::Map).unwrap();
    doc.put_object(&ROOT, "empty list", ObjType::List).unwrap();
    doc.put_object(&ROOT, "table", ObjType::Table).unwrap();
    // Deletions and overwrites leave ops that are no longer on top.
    doc.delete(&nodes, "n03").unwrap();
    doc.put(&nodes, "n04", "replaced by a scalar").unwrap();
    let n05 = doc.get(&nodes, "n05").unwrap().unwrap().1;
    let items = doc.get(&n05, "items").unwrap().unwrap().1;
    doc.delete(&items, 0).unwrap();
    doc.put(&ROOT, "name", "renamed").unwrap();
    doc.commit();
    doc
}

#[test]
fn one_of_everything_reads_alike() {
    let mut doc = everything("aaaa");
    same(&doc);
    // Uncommitted ops are read too.
    doc.put(&ROOT, "pending", 1).unwrap();
    same(&doc);
}

#[test]
fn a_merge_with_conflicts_reads_alike() {
    let mut left = everything("aaaa");
    let mut right = left.fork().with_actor(ActorId::from(b"bbbb".as_slice()));
    left.put(&ROOT, "name", "left").unwrap();
    right.put(&ROOT, "name", "right").unwrap();
    let nodes = left.get(&ROOT, "nodes").unwrap().unwrap().1;
    left.put(&nodes, "n10", "left wins?").unwrap();
    right.put_object(&nodes, "n10", ObjType::List).unwrap();
    right.increment(&ROOT, "count", 10).unwrap();
    left.merge(&mut right).unwrap();
    let hydrated = left.hydrate_in_one_pass();
    assert_eq!(hydrated, left.hydrate(&ROOT, None).unwrap());
    let automerge::hydrate::Value::Map(root) = hydrated else {
        panic!("the root is a map");
    };
    assert!(root.iter().any(|(_, value)| value.conflict));
}

#[test]
fn a_loaded_document_reads_alike() {
    let mut doc = everything("aaaa");
    let loaded = AutoCommit::load(&doc.save()).unwrap();
    same(&loaded);
    assert_eq!(loaded.hydrate_in_one_pass(), doc.hydrate_in_one_pass());
}

#[test]
fn an_empty_document_reads_alike() {
    same(&AutoCommit::new());
}
