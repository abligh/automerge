use text_value::ConcreteTextValue;

use crate::exid::ExId;
use crate::transaction::Transactable;
use crate::*;

#[test]
fn simple_hydrate() -> Result<(), AutomergeError> {
    let mut doc = AutoCommit::default();
    let list = doc.put_object(&ObjId::Root, "list", ObjType::List)?;
    doc.insert(&list, 0, 5)?;
    doc.insert(&list, 1, 6)?;
    doc.insert(&list, 2, 7)?;
    doc.insert(&list, 3, "hello")?;
    doc.insert(&list, 4, ScalarValue::counter(100))?;
    doc.insert_object(&list, 5, ObjType::Map)?;
    doc.insert_object(&list, 6, ObjType::List)?;
    let text = doc.put_object(&ObjId::Root, "text", ObjType::Text)?;
    doc.splice_text(&text, 0, 0, "hello world")?;
    let mut hydrated = doc.hydrate(ExId::Root, None)?;
    assert_eq!(
        hydrated,
        hydrate_map!(
            "list" => hydrate_list!(5,6,7,"hello", ScalarValue::counter(100), hydrate_map!(), hydrate_list![]),
            "text" => ConcreteTextValue::new("hello world", TextEncoding::platform_default()),
        ).into()
    );
    doc.splice_text(&text, 6, 0, "big bad ")?;
    assert_eq!(doc.text(&text)?, "hello big bad world".to_owned());
    let heads = doc.get_heads();
    let cursor = doc.diff_cursor().to_vec();
    let patches = doc.diff(&cursor, &heads);
    doc.update_diff_cursor();
    hydrated.apply_patches(TextEncoding::platform_default(), patches)?;
    let hydrate::Value::Text(val) = &hydrated.as_map().unwrap().get("text").unwrap() else {
        panic!("expected text");
    };
    assert_eq!(String::from(val), "hello big bad world".to_string(),);
    Ok(())
}

/// `Node::map` sorts entries that come to it out of order, in release
/// builds as in debug ones, so a reader may binary search a map's keys.
#[test]
fn node_map_sorts_unsorted_keys() {
    let entries = ["b", "", "a", "B", "\u{e9}", "aa"]
        .iter()
        .enumerate()
        .map(|(i, key)| {
            (
                key.to_string(),
                hydrate::Node::Scalar(ScalarValue::Int(i as i64)),
            )
        })
        .collect::<Vec<_>>();
    let mut sorted = entries.clone();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        hydrate::Node::map(entries),
        hydrate::Node::Map(sorted.clone())
    );
    // Already sorted entries are kept as they are.
    assert_eq!(
        hydrate::Node::map(sorted.clone()),
        hydrate::Node::Map(sorted)
    );
}

/// A hydrated map converts to the node its value converts to, without
/// being wrapped in a `Value`.
#[test]
fn node_from_map_is_node_from_its_value() {
    let mut doc = AutoCommit::new();
    let map = doc.put_object(&ObjId::Root, "map", ObjType::Map).unwrap();
    for key in ["z", "a", "m", "B", "\u{1F600}", ""] {
        doc.put(&map, key, key.len() as i64).unwrap();
    }
    let inner = doc.put_object(&map, "inner", ObjType::Map).unwrap();
    doc.put(&inner, "y", 1).unwrap();
    doc.put(&inner, "x", 2).unwrap();
    let list = doc.put_object(&map, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, "one").unwrap();
    let hydrate::Value::Map(hydrated) = doc.hydrate(&map, None).unwrap() else {
        panic!("a map");
    };
    let node = hydrate::Node::from(&hydrated);
    assert_eq!(
        node,
        hydrate::Node::from(&hydrate::Value::Map(hydrated.clone()))
    );
    let hydrate::Node::Map(entries) = node else {
        panic!("a map node");
    };
    assert!(entries.windows(2).all(|pair| pair[0].0 < pair[1].0));
}
