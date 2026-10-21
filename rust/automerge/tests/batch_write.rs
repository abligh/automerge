//! `batch_write`: a batch of map puts and increments must leave the document
//! exactly as the same writes made one at a time would, byte for byte in
//! the save, and must roll back cleanly.

use automerge::transaction::{BatchWrite, Transactable};
use automerge::{
    ActorId, AutoCommit, Automerge, AutomergeError, ObjType, Patch, PatchAction, ReadDoc,
    ScalarValue, Value, ROOT,
};

fn actor() -> ActorId {
    ActorId::from(&[1u8; 16][..])
}

fn doc() -> AutoCommit {
    AutoCommit::new().with_actor(actor())
}

/// A document holding a map `m` with three keys, a counter `c` in it and an
/// older value under `old` that sequential and batched writers then edit.
fn seeded() -> (AutoCommit, automerge::ObjId) {
    let mut d = doc();
    let m = d.put_object(ROOT, "m", ObjType::Map).unwrap();
    d.put(&m, "a", 1).unwrap();
    d.put(&m, "b", "two").unwrap();
    d.put(&m, "c", ScalarValue::counter(10)).unwrap();
    d.put(&m, "old", 1).unwrap();
    d.put(&m, "old", 2).unwrap();
    d.commit();
    (d, m)
}

fn writes(m: &automerge::ObjId) -> Vec<(automerge::ObjId, &'static str, BatchWrite)> {
    vec![
        (m.clone(), "a", BatchWrite::Put(ScalarValue::Int(5))),
        (ROOT, "z", BatchWrite::Put(ScalarValue::Str("new".into()))),
        (m.clone(), "c", BatchWrite::Increment(3)),
        (m.clone(), "old", BatchWrite::Put(ScalarValue::Int(3))),
        (m.clone(), "b", BatchWrite::Put(ScalarValue::Null)),
        (m.clone(), "d", BatchWrite::Put(ScalarValue::F64(1.5))),
    ]
}

fn apply_sequentially(d: &mut AutoCommit, m: &automerge::ObjId) {
    for (obj, key, w) in writes(m) {
        match w {
            BatchWrite::Put(v) => d.put(&obj, key, v).unwrap(),
            BatchWrite::Increment(n) => d.increment(&obj, key, n).unwrap(),
        }
    }
}

#[test]
fn batch_equals_sequential_writes() {
    let (mut seq, m) = seeded();
    let (mut bat, m2) = seeded();
    assert_eq!(m, m2);
    apply_sequentially(&mut seq, &m);
    let bat_ids = bat.batch_write(writes(&m)).unwrap();
    // Every write here makes an op, so every entry names one.
    assert_eq!(bat_ids.len(), 6);
    assert!(bat_ids.iter().all(Option::is_some));
    assert_eq!(seq.get(&m, "a").unwrap().unwrap().0, Value::int(5));
    assert_eq!(bat.get(&m, "a").unwrap().unwrap().0, Value::int(5));
    assert_eq!(bat.get(&m, "c").unwrap().unwrap().0, Value::counter(13));
    assert_eq!(bat.get(&m, "old").unwrap().unwrap().0, Value::int(3));
    assert_eq!(bat.get(ROOT, "z").unwrap().unwrap().0, Value::str("new"));
    assert_eq!(seq.save(), bat.save());
    assert_eq!(seq.get_heads(), bat.get_heads());
}

#[test]
fn batch_put_of_object_values_is_refused_without_losing_earlier_writes() {
    // Only scalars are batched; an object value is the caller's mistake and
    // the writes before it stay applied.
    let (mut d, m) = seeded();
    let r = d.batch_write(vec![
        (m.clone(), "a", BatchWrite::Put(ScalarValue::Int(7))),
        (ROOT, "list", BatchWrite::Put(ScalarValue::Int(1))),
    ]);
    assert!(r.is_ok());
    let list = d.put_object(ROOT, "l", ObjType::List).unwrap();
    let r = d.batch_write(vec![
        (m.clone(), "a", BatchWrite::Put(ScalarValue::Int(8))),
        (list.clone(), "0", BatchWrite::Put(ScalarValue::Int(1))),
    ]);
    assert!(matches!(r, Err(AutomergeError::InvalidOp(ObjType::List))));
    assert_eq!(d.get(&m, "a").unwrap().unwrap().0, Value::int(8));
}

#[test]
fn increment_of_a_non_counter_is_missing_counter() {
    let (mut d, m) = seeded();
    let r = d.batch_write(vec![
        (m.clone(), "a", BatchWrite::Put(ScalarValue::Int(9))),
        (m.clone(), "b", BatchWrite::Increment(1)),
    ]);
    assert!(matches!(r, Err(AutomergeError::MissingCounter)));
    assert_eq!(d.get(&m, "a").unwrap().unwrap().0, Value::int(9));
    let (mut seq, _) = seeded();
    seq.put(&m, "a", 9).unwrap();
    assert_eq!(seq.save(), d.save());
}

#[test]
fn same_key_twice_and_no_op_puts() {
    let (mut seq, m) = seeded();
    let (mut bat, _) = seeded();
    let ws = |m: &automerge::ObjId| {
        vec![
            (m.clone(), "a", BatchWrite::Put(ScalarValue::Int(1))), // same value: no-op
            (m.clone(), "n", BatchWrite::Put(ScalarValue::Int(1))),
            (m.clone(), "n", BatchWrite::Put(ScalarValue::Int(2))),
            (m.clone(), "n", BatchWrite::Put(ScalarValue::Int(2))), // no-op
            (m.clone(), "c", BatchWrite::Increment(1)),
            (m.clone(), "c", BatchWrite::Increment(1)),
            (m.clone(), "a", BatchWrite::Put(ScalarValue::Int(2))),
        ]
    };
    for (obj, key, w) in ws(&m) {
        match w {
            BatchWrite::Put(v) => seq.put(&obj, key, v).unwrap(),
            BatchWrite::Increment(n) => seq.increment(&obj, key, n).unwrap(),
        }
    }
    let bat_ids = bat.batch_write(ws(&m)).unwrap();
    // The two no-op puts make no op and name none.
    assert_eq!(
        bat_ids.iter().map(Option::is_some).collect::<Vec<_>>(),
        vec![false, true, true, false, true, true, true]
    );
    assert_eq!(bat.get(&m, "n").unwrap().unwrap().0, Value::int(2));
    assert_eq!(bat.get(&m, "c").unwrap().unwrap().0, Value::counter(12));
    assert_eq!(bat.get(&m, "a").unwrap().unwrap().0, Value::int(2));
    assert_eq!(seq.save(), bat.save());
}

#[test]
fn rollback_restores_the_document() {
    let (mut d, m) = seeded();
    let before = d.save();
    let heads = d.get_heads();
    d.batch_write(writes(&m)).unwrap();
    // A sequential write after the batch rolls back with it.
    d.put(&m, "after", "x").unwrap();
    d.put_object(&m, "obj", ObjType::Map).unwrap();
    d.batch_write(vec![
        (m.clone(), "e", BatchWrite::Put(ScalarValue::Int(1))),
        (m.clone(), "c", BatchWrite::Increment(100)),
    ])
    .unwrap();
    assert_eq!(d.get(&m, "c").unwrap().unwrap().0, Value::counter(113));
    d.rollback();
    assert_eq!(d.get_heads(), heads);
    assert_eq!(d.get(&m, "a").unwrap().unwrap().0, Value::int(1));
    assert_eq!(d.get(&m, "b").unwrap().unwrap().0, Value::str("two"));
    assert_eq!(d.get(&m, "c").unwrap().unwrap().0, Value::counter(10));
    assert_eq!(d.get(&m, "old").unwrap().unwrap().0, Value::int(2));
    assert!(d.get(&m, "d").unwrap().is_none());
    assert!(d.get(&m, "e").unwrap().is_none());
    assert!(d.get(&m, "after").unwrap().is_none());
    assert!(d.get(&m, "obj").unwrap().is_none());
    assert!(d.get(ROOT, "z").unwrap().is_none());
    assert_eq!(d.save(), before);
    // The document is still usable: the same writes land as before.
    let (mut seq, _) = seeded();
    apply_sequentially(&mut seq, &m);
    d.batch_write(writes(&m)).unwrap();
    assert_eq!(d.save(), seq.save());
}

#[test]
fn manual_transaction_rollback() {
    let mut d = Automerge::new().with_actor(actor());
    let m = {
        let mut tx = d.transaction();
        let m = tx.put_object(ROOT, "m", ObjType::Map).unwrap();
        tx.put(&m, "c", ScalarValue::counter(0)).unwrap();
        tx.commit();
        m
    };
    let before = d.save();
    let mut tx = d.transaction();
    tx.batch_write(vec![
        (m.clone(), "x", BatchWrite::Put(ScalarValue::Int(1))),
        (m.clone(), "c", BatchWrite::Increment(2)),
    ])
    .unwrap();
    assert_eq!(tx.get(&m, "c").unwrap().unwrap().0, Value::counter(2));
    tx.rollback();
    assert_eq!(d.save(), before);
    assert!(d.get(&m, "x").unwrap().is_none());
}

#[test]
fn patches_come_in_write_order() {
    fn actions(patches: Vec<Patch>) -> Vec<(String, String)> {
        patches
            .into_iter()
            .map(|p| {
                let key = match &p.action {
                    PatchAction::PutMap { key, .. } => key.clone(),
                    PatchAction::Increment { prop, .. } => prop.to_string(),
                    other => format!("{other:?}"),
                };
                let what = match &p.action {
                    PatchAction::PutMap { value, .. } => format!("{:?}", value.0),
                    PatchAction::Increment { value, .. } => format!("+{value}"),
                    _ => String::new(),
                };
                (key, what)
            })
            .collect()
    }
    let (mut seq, m) = seeded();
    let (mut bat, _) = seeded();
    seq.update_diff_cursor();
    bat.update_diff_cursor();
    apply_sequentially(&mut seq, &m);
    bat.batch_write(writes(&m)).unwrap();
    let s = actions(seq.diff_incremental());
    let b = actions(bat.diff_incremental());
    // The log holds the batch's patches in write order, as it holds the
    // sequential ones; the diff then orders them as it orders any log.
    assert_eq!(s, b);
    assert_eq!(s.len(), 6);
    assert!(s.contains(&("c".to_string(), "+3".to_string())));
}

#[test]
fn empty_batch_is_nothing() {
    let (mut d, _) = seeded();
    let before = d.save();
    let heads = d.get_heads();
    let ids = d
        .batch_write(Vec::<(automerge::ObjId, &str, BatchWrite)>::new())
        .unwrap();
    assert!(ids.is_empty());
    assert_eq!(d.get_heads(), heads);
    assert_eq!(d.save(), before);
}
