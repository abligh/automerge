//! A load with `LoadOptions::deferred_hashes` reads, edits and saves as a
//! full load does, refuses what needs a hash it has not learned, and
//! takes its hashes from a full load of what it saves, which proves the
//! stored head: a document whose stored head is not what its ops
//! rebuild into opens, but its save fails a full load, and hashes from
//! elsewhere are refused.
use automerge::sync::{self, SyncDoc};
use automerge::transaction::Transactable;
use automerge::{
    ActorId, AutoCommit, DeferredMismatch, LoadOptions, ObjType, ReadDoc, VerificationMode, ROOT,
};
use sha2::{Digest, Sha256};

/// A document of a few changes and every kind of object.
fn drawing() -> AutoCommit {
    drawing_by(b"aaaa")
}

/// [`drawing`], its first actor `actor`.
fn drawing_by(actor: &[u8]) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(actor));
    let nodes = doc.put_object(&ROOT, "nodes", ObjType::Map).unwrap();
    for i in 0..30 {
        let node = doc
            .put_object(&nodes, format!("n{i:02}"), ObjType::Map)
            .unwrap();
        doc.put(&node, "type", "path").unwrap();
        let list = doc.put_object(&node, "items", ObjType::List).unwrap();
        doc.insert(&list, 0, i as i64).unwrap();
        if i % 7 == 0 {
            let text = doc.put_object(&node, "lines", ObjType::Text).unwrap();
            doc.splice_text(&text, 0, 0, "hello").unwrap();
        }
    }
    doc.commit();
    // A second actor's change on top, so there is history to defer.
    let mut other = doc.fork().with_actor(ActorId::from(b"bbbb".as_slice()));
    other.put(&nodes, "n03", "replaced").unwrap();
    other.delete(&nodes, "n04").unwrap();
    other.commit();
    doc.merge(&mut other).unwrap();
    doc.put(&ROOT, "version", 2).unwrap();
    doc.commit();
    doc
}

fn deferred(bytes: &[u8]) -> AutoCommit {
    AutoCommit::load_with_options(
        bytes,
        LoadOptions::new()
            .verification_mode(VerificationMode::DontCheck)
            .deferred_hashes(true),
    )
    .unwrap()
}

#[test]
fn a_deferred_load_reads_and_saves_as_a_full_load() {
    let mut doc = drawing();
    let bytes = doc.save();
    let mut full = AutoCommit::load(&bytes).unwrap();
    let mut late = deferred(&bytes);
    assert!(late.hashes_deferred());
    assert!(!full.hashes_deferred());
    assert_eq!(late.get_heads(), full.get_heads());
    assert_eq!(
        late.hydrate(&ROOT, None).unwrap(),
        full.hydrate(&ROOT, None).unwrap()
    );
    assert_eq!(late.save(), bytes);
}

#[test]
fn a_deferred_load_takes_local_edits() {
    let mut doc = drawing();
    let bytes = doc.save();
    let mut late = deferred(&bytes);
    let mut full = AutoCommit::load(&bytes).unwrap();
    for d in [&mut late, &mut full] {
        d.set_actor(ActorId::from(b"cccc".as_slice()));
        d.put(&ROOT, "edited", true).unwrap();
        d.commit();
    }
    assert_eq!(late.get_heads(), full.get_heads());
    assert_eq!(late.save(), full.save());
    // A diff from the loaded heads, as Galatea's diff cursor takes one.
    let before = AutoCommit::load(&bytes).unwrap().get_heads();
    let (late_heads, full_heads) = (late.get_heads(), full.get_heads());
    assert_eq!(
        late.diff(&before, &late_heads),
        full.diff(&before, &full_heads)
    );
}

#[test]
fn filled_hashes_make_a_deferred_load_whole() {
    let mut doc = drawing();
    let bytes = doc.save();
    let mut late = deferred(&bytes);
    late.set_actor(ActorId::from(b"cccc".as_slice()));
    late.put(&ROOT, "edited", true).unwrap();
    late.commit();
    // The proof: a full load of what the deferred copy saves.
    let mut prover = AutoCommit::load(&late.save()).unwrap();
    late.fill_deferred_hashes(&prover.hashes_and_op_counts())
        .unwrap();
    assert!(!late.hashes_deferred());
    let mut full = AutoCommit::load(&late.save()).unwrap();
    assert_eq!(late.get_changes(&[]), full.get_changes(&[]));
    assert_eq!(late.hashes_and_op_counts(), full.hashes_and_op_counts());
    // Sync works from here.
    let mut peer = AutoCommit::new();
    let (mut ours, mut theirs) = (sync::State::new(), sync::State::new());
    for _ in 0..10 {
        if let Some(m) = late.sync().generate_sync_message(&mut ours) {
            peer.sync().receive_sync_message(&mut theirs, m).unwrap();
        }
        if let Some(m) = peer.sync().generate_sync_message(&mut theirs) {
            late.sync().receive_sync_message(&mut ours, m).unwrap();
        }
    }
    assert_eq!(peer.get_heads(), late.get_heads());
    // Filling again does nothing.
    late.fill_deferred_hashes(&[]).unwrap();
}

#[test]
#[should_panic(expected = "deferred load")]
fn history_waits_for_the_hashes() {
    let mut late = deferred(&drawing().save());
    late.get_changes(&[]);
}

#[test]
#[should_panic(expected = "deferred load")]
fn sync_waits_for_the_hashes() {
    let mut late = deferred(&drawing().save());
    let mut state = sync::State::new();
    late.sync().generate_sync_message(&mut state);
}

#[test]
fn hashes_from_another_document_are_refused() {
    let mut doc = drawing();
    let mut late = deferred(&doc.save());
    // As many changes, none of them the same.
    let given = drawing_by(b"zzzz").hashes_and_op_counts();
    assert!(matches!(
        late.fill_deferred_hashes(&given),
        Err(DeferredMismatch::Hash { .. })
    ));
    assert!(matches!(
        late.fill_deferred_hashes(&given[..1]),
        Err(DeferredMismatch::Length { .. })
    ));
    assert!(late.hashes_deferred());
}

/// `bytes`, an uncompressed document chunk, with one bit of its stored
/// head flipped and its checksum made good again: ops that do not
/// rebuild into the stored head.
fn with_a_wrong_head(bytes: &[u8]) -> Vec<u8> {
    // Magic, checksum, chunk type, then the data's length.
    let mut at = 9;
    let mut len = 0u64;
    let mut shift = 0;
    loop {
        let b = bytes[at];
        at += 1;
        len |= u64::from(b & 0x7f) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            break;
        }
    }
    let data_start = at;
    assert_eq!(bytes.len() - data_start, len as usize, "one chunk");
    let leb = |at: &mut usize| {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let b = bytes[*at];
            *at += 1;
            v |= u64::from(b & 0x7f) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                return v;
            }
        }
    };
    let mut p = data_start;
    for _ in 0..leb(&mut p) {
        let actor = leb(&mut p) as usize;
        p += actor;
    }
    assert_eq!(leb(&mut p), 1, "one head");
    let mut out = bytes.to_vec();
    out[p] ^= 1;
    let digest = Sha256::new()
        .chain_update(&out[8..9])
        .chain_update(&out[9..data_start])
        .chain_update(&out[data_start..])
        .finalize();
    out[4..8].copy_from_slice(&digest[..4]);
    out
}

#[test]
fn a_wrong_head_opens_deferred_and_fails_its_proof() {
    let mut doc = drawing();
    let crafted = with_a_wrong_head(&doc.save_nocompress());
    // A full load refuses it at once.
    assert!(AutoCommit::load(&crafted).is_err());
    // A deferred load opens it and reads it.
    let mut late = deferred(&crafted);
    assert!(late.hashes_deferred());
    assert_eq!(
        late.hydrate(&ROOT, None).unwrap(),
        doc.hydrate(&ROOT, None).unwrap()
    );
    // Its save keeps the wrong head, so the proof fails.
    assert!(AutoCommit::load(&late.save()).is_err());
    // Hashes from the honest document don't match the wrong head.
    assert!(matches!(
        late.fill_deferred_hashes(&doc.hashes_and_op_counts()),
        Err(DeferredMismatch::Hash { .. })
    ));
    assert!(late.hashes_deferred());
}

#[test]
fn a_document_of_two_heads_loads_in_full() {
    let mut doc = drawing();
    let mut left = doc.fork().with_actor(ActorId::from(b"llll".as_slice()));
    let mut right = doc.fork().with_actor(ActorId::from(b"rrrr".as_slice()));
    left.put(&ROOT, "side", "left").unwrap();
    right.put(&ROOT, "side", "right").unwrap();
    left.merge(&mut right).unwrap();
    assert_eq!(left.get_heads().len(), 2);
    let late = deferred(&left.save());
    assert!(!late.hashes_deferred());
}

#[test]
fn an_empty_document_loads_in_full() {
    let late = deferred(&AutoCommit::new().save());
    assert!(!late.hashes_deferred());
}

#[test]
fn a_change_made_since_is_read_back_before_the_hashes_are_filled() {
    let mut late = deferred(&drawing().save());
    late.set_actor(ActorId::from(b"cccc".as_slice()));
    late.put(&ROOT, "edited", true).unwrap();
    late.commit();
    let change = late.get_last_local_change().unwrap();
    assert_eq!(change.hash(), late.get_heads()[0]);
    assert_eq!(change.len(), 1);
}

#[test]
#[should_panic(expected = "deferred load")]
fn a_loaded_change_waits_for_the_hashes() {
    let mut doc = drawing();
    let head = doc.get_heads()[0];
    let late = deferred(&doc.save());
    // The stored head's hash is known, but its parents' are not.
    late.get_change_by_hash(&head);
}
