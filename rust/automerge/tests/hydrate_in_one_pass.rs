//! `hydrate_in_one_pass`, and `hydrate_gathered` object by object,
//! read what `hydrate` reads with no heads, for every kind of object
//! and value, after deletions, overwrites, increments, merges with
//! conflicts, and a save and load.
use automerge::hydrate::Node;
use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjType, ReadDoc, ScalarValue, ROOT};

fn same(doc: &AutoCommit) {
    assert_eq!(doc.hydrate_in_one_pass(), doc.hydrate(&ROOT, None).unwrap());
    same_one_at_a_time(doc);
    same_as_nodes(doc);
}

/// `take_gathered` builds the root, and each object under `nodes` from
/// a gathering of its own, as `hydrate` gives it converted to a node.
fn same_as_nodes(doc: &AutoCommit) {
    let mut gathered = doc.gather_in_one_pass();
    let root = doc.take_gathered(&mut gathered, &ROOT).unwrap();
    assert_eq!(root, Node::from(&doc.hydrate(&ROOT, None).unwrap()));
    let Some((_, nodes)) = doc.get(&ROOT, "nodes").unwrap() else {
        return;
    };
    let mut gathered = doc.gather_in_one_pass();
    for item in doc.map_range(&nodes, ..) {
        let id = item.id();
        if !matches!(item.value, automerge::ValueRef::Object(_)) {
            continue;
        }
        let taken = doc.take_gathered(&mut gathered, &id).unwrap();
        assert_eq!(
            taken,
            Node::from(&doc.hydrate(&id, None).unwrap()),
            "{}",
            item.key
        );
    }
}

/// Each object under `nodes`, built from one gathering in turn, is
/// what `hydrate` gives for it alone, and building takes it out.
fn same_one_at_a_time(doc: &AutoCommit) {
    let Some((_, nodes)) = doc.get(&ROOT, "nodes").unwrap() else {
        return;
    };
    let mut gathered = doc.gather_in_one_pass();
    let before = gathered.len();
    for item in doc.map_range(&nodes, ..) {
        let id = item.id();
        if !matches!(item.value, automerge::ValueRef::Object(_)) {
            continue;
        }
        let built = doc.hydrate_gathered(&mut gathered, &id).unwrap();
        assert_eq!(built, doc.hydrate(&id, None).unwrap(), "{}", item.key);
        // Built once; a second build finds nothing left.
        let again = doc.hydrate_gathered(&mut gathered, &id).unwrap();
        assert_ne!(again, built, "{} built twice", item.key);
    }
    assert!(gathered.len() < before);
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

/// Two actors setting the same map keys at once, to scalars and to
/// objects, and keys deleted and set again: the tree picks the winner
/// `hydrate` picks.
#[test]
fn concurrent_and_reset_keys_read_alike() {
    let mut left = everything("aaaa");
    let mut right = left.fork().with_actor(ActorId::from(b"bbbb".as_slice()));
    let nodes = left.get(&ROOT, "nodes").unwrap().unwrap().1;
    for i in 0..10 {
        let key = format!("n{i:02}");
        left.put(&nodes, &key, format!("left {i}")).unwrap();
        if i % 2 == 0 {
            let map = right.put_object(&nodes, &key, ObjType::Map).unwrap();
            right.put(&map, "from", "right").unwrap();
        } else {
            right.put(&nodes, &key, i as i64).unwrap();
        }
    }
    // Deleted on one side, set on the other.
    left.delete(&nodes, "n20").unwrap();
    right.put(&nodes, "n20", "set by right").unwrap();
    // Deleted and set again on one side.
    left.delete(&nodes, "n21").unwrap();
    left.put(&nodes, "n21", "set again").unwrap();
    let n22 = left.get(&nodes, "n22").unwrap().unwrap().1;
    left.delete(&n22, "at").unwrap();
    left.put(&n22, "at", 99).unwrap();
    left.merge(&mut right).unwrap();
    same(&left);
    let mut loaded = AutoCommit::load(&left.save()).unwrap();
    same(&loaded);
    loaded.put(&ROOT, "pending", 1).unwrap();
    same(&loaded);
}

/// A map's keys come out sorted as `str` sorts them, whatever their
/// case, length or script, and `Node::from` sorts a hydrated map's.
#[test]
fn map_keys_come_out_sorted() {
    let mut doc = AutoCommit::new();
    let map = doc.put_object(&ROOT, "keys", ObjType::Map).unwrap();
    let keys = [
        "b",
        "B",
        "a",
        "aa",
        "é",
        "Z",
        "",
        "a b",
        "\u{1F600}",
        "10",
        "9",
    ];
    for (i, key) in keys.iter().enumerate() {
        doc.put(&map, *key, i as i64).unwrap();
    }
    let mut sorted: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
    sorted.sort();
    for node in [
        doc.take_gathered(&mut doc.gather_in_one_pass(), &map)
            .unwrap(),
        Node::from(&doc.hydrate(&map, None).unwrap()),
    ] {
        let Node::Map(entries) = node else {
            panic!("a map");
        };
        let got: Vec<String> = entries.into_iter().map(|(key, _)| key).collect();
        assert_eq!(got, sorted);
    }
    same(&doc);
}
