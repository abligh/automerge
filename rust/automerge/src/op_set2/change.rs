use super::meta::ValueMeta;
use super::op::{AsChangeOp, OpBuilder};
use super::types::{Action, ActorIdx};
use crate::change_graph::ChangeGraph;
use crate::storage::change::{ChangeOpsColumns as ChangeOpsColumns2, Verified};
use crate::storage::{Change, ChunkType, Header};
use crate::types::{ActorId, ChangeHash};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::io::Write;
use std::marker::PhantomData;
use std::num::NonZero;
use std::ops::Range;

pub(crate) mod batch;
pub(crate) mod collector;

pub(crate) use collector::{BuildChangeMetadata, ChangeCollector, CollectedChanges, OutOfMemory};

pub(crate) trait GetHash {
    fn get_hash(&self, index: usize) -> Option<ChangeHash>;
}

impl GetHash for Vec<crate::Change> {
    fn get_hash(&self, index: usize) -> Option<ChangeHash> {
        Some(self.get(index)?.hash())
    }
}

impl GetHash for ChangeGraph {
    fn get_hash(&self, index: usize) -> Option<ChangeHash> {
        self.index_to_hash(index).copied()
    }
}

pub(crate) fn build_change<T, G>(
    ops: &[T],
    meta: &BuildChangeMetadata<'_>,
    graph: &G,
    actors: &[ActorId],
) -> Change<'static, Verified>
where
    T: AsChangeOp,
    G: GetHash,
{
    let mut mapper = ActorMapper::new(actors);
    build_change_inner(ops, meta, graph, &mut mapper)
}

pub(crate) fn build_change_inner<T, G>(
    ops: &[T],
    meta: &BuildChangeMetadata<'_>,
    graph: &G,
    mapper: &mut ActorMapper<'_>,
) -> Change<'static, Verified>
where
    T: AsChangeOp,
    G: GetHash,
{
    build_change_with(ops, meta, graph, mapper, None)
}

/// [`build_change`] for `ops` whose first `encoded.ops` are already
/// encoded in `encoded`: only the ops after those are encoded, and the
/// change's bytes are the ones [`build_change`] gives.
pub(crate) fn build_change_encoded<T, G>(
    ops: &[T],
    meta: &BuildChangeMetadata<'_>,
    graph: &G,
    actors: &[ActorId],
    encoded: EncodedOps,
) -> Change<'static, Verified>
where
    T: AsChangeOp,
    G: GetHash,
{
    let mut mapper = ActorMapper::new(actors);
    build_change_with(ops, meta, graph, &mut mapper, Some(encoded))
}

fn build_change_with<T, G>(
    ops: &[T],
    meta: &BuildChangeMetadata<'_>,
    graph: &G,
    mapper: &mut ActorMapper<'_>,
    encoded: Option<EncodedOps>,
) -> Change<'static, Verified>
where
    T: AsChangeOp,
    G: GetHash,
{
    let num_ops = ops.len();

    let actor = mapper.actors[meta.actor].clone();

    let start_op = ops.first().map(T::op_id_ctr).unwrap_or(meta.max_op + 1);

    let (col_data, ops_meta) = match encoded.and_then(|e| e.extend(ops, meta.actor, mapper)) {
        Some((data, cols)) => (data, cols.into()),
        None => {
            let mut col_data = Vec::new();
            let ops_meta = write_change_ops(ops, meta.actor, &mut col_data, mapper);
            (col_data, ops_meta)
        }
    };

    let other_actors: Vec<_> = mapper.iter().collect();

    let mut data = Vec::with_capacity(col_data.len());
    leb128::write::unsigned(&mut data, meta.deps.len() as u64).unwrap();

    // FIXME missing value here is changes out of order error
    let deps: Vec<_> = meta
        .deps
        .iter()
        .map(|i| graph.get_hash(*i as usize).unwrap())
        .collect();

    for hash in &deps {
        data.write_all(hash.as_bytes()).unwrap();
    }

    length_prefixed_bytes(&actor, &mut data);

    leb128::write::unsigned(&mut data, meta.seq).unwrap();
    leb128::write::unsigned(&mut data, start_op).unwrap();
    leb128::write::signed(&mut data, meta.timestamp).unwrap();

    length_prefixed_bytes(meta.message_str(), &mut data);

    leb128::write::unsigned(&mut data, other_actors.len() as u64).unwrap();

    for actor in other_actors.iter() {
        length_prefixed_bytes(actor, &mut data);
    }

    ops_meta.raw_columns().write(&mut data);

    let ops_data_start = data.len();
    let ops_data = ops_data_start..(ops_data_start + col_data.len());

    data.extend(col_data);
    let extra_bytes = data.len()..(data.len() + meta.extra.len());
    if !meta.extra.is_empty() {
        data.extend(meta.extra.as_ref());
    }

    let header = Header::new(ChunkType::Change, &data);

    let mut bytes = Vec::with_capacity(header.len() + data.len());
    header.write(&mut bytes);
    bytes.extend(data);

    let ops_data = shift_range(ops_data, header.len());
    let extra_bytes = shift_range(extra_bytes, header.len());

    Change {
        bytes: Cow::Owned(bytes),
        header,
        dependencies: deps,
        actor,
        other_actors,
        seq: meta.seq,
        start_op: NonZero::new(start_op).unwrap(),
        timestamp: meta.timestamp,
        message: meta.message.as_ref().map(|s| s.to_string()),
        ops_meta,
        ops_data,
        extra_bytes,
        num_ops,
        _phantom: PhantomData,
    }
}

impl PartialOrd for OpBuilder<'_> {
    fn partial_cmp(&self, other: &OpBuilder<'_>) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OpBuilder<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.id.cmp(&other.id)
    }
}

#[derive(Default, Debug, Clone)]
pub(crate) struct ChangeOpsColumns {
    pub(crate) obj_actor: Range<usize>,
    pub(crate) obj_ctr: Range<usize>,
    pub(crate) key_actor: Range<usize>,
    pub(crate) key_ctr: Range<usize>,
    pub(crate) key_str: Range<usize>,
    pub(crate) insert: Range<usize>,
    pub(crate) action: Range<usize>,
    pub(crate) value_meta: Range<usize>,
    pub(crate) value: Range<usize>,
    pub(crate) pred_count: Range<usize>,
    pub(crate) pred_actor: Range<usize>,
    pub(crate) pred_ctr: Range<usize>,
    pub(crate) expand: Range<usize>,
    pub(crate) mark_name: Range<usize>,
}

pub(crate) fn shift_range(range: Range<usize>, by: usize) -> Range<usize> {
    range.start + by..range.end + by
}

pub(crate) fn length_prefixed_bytes<B: AsRef<[u8]>>(b: B, out: &mut Vec<u8>) -> usize {
    let prefix_len = leb128::write::unsigned(out, b.as_ref().len() as u64).unwrap();
    out.write_all(b.as_ref()).unwrap();
    prefix_len + b.as_ref().len()
}

impl<'a> PartialEq for OpBuilder<'a> {
    fn eq(&self, other: &OpBuilder<'a>) -> bool {
        self.id == other.id
    }
}

impl Eq for OpBuilder<'_> {}

fn write_change_ops<T>(
    ops: &[T],
    change_actor: usize,
    data: &mut Vec<u8>,
    mapper: &mut ActorMapper<'_>,
) -> ChangeOpsColumns2
where
    T: AsChangeOp,
{
    if ops.is_empty() {
        return ChangeOpsColumns::default().into();
    }

    mapper.remap_actors(ops, change_actor);
    encode_change_ops(ops, data, &mapper.mapping).into()
}

/// `ops`' change columns, appended to `data`, their actors numbered by
/// `mapping` (an [`ActorMapper`]'s after `remap_actors` over a set of
/// ops holding these).
pub(crate) fn encode_change_ops<T>(
    ops: &[T],
    data: &mut Vec<u8>,
    mapping: &[Option<ActorIdx>],
) -> ChangeOpsColumns
where
    T: AsChangeOp,
{
    use hexane::EncoderApi;

    let remap_opt_actor = |actor: Option<ActorIdx>| actor.map(|a| mapping[usize::from(a)].unwrap());
    let remap_actor = |a: ActorIdx| mapping[usize::from(a)].unwrap();

    let obj_actor = hexane::Encoder::<Option<ActorIdx>>::encode_to_unless(
        data,
        ops.iter().map(T::obj_actor).map(&remap_opt_actor),
        None,
    );
    let obj_ctr =
        hexane::Encoder::<Option<u64>>::encode_to_unless(data, ops.iter().map(T::obj_ctr), None);
    let key_actor = hexane::Encoder::<Option<ActorIdx>>::encode_to_unless(
        data,
        ops.iter().map(T::key_actor).map(&remap_opt_actor),
        None,
    );
    let key_ctr = hexane::DeltaEncoder::<Option<i64>>::encode_to_unless(
        data,
        ops.iter().map(T::key_ctr).map(|c| c.map(|c| *c)),
        None,
    );
    let key_str =
        hexane::Encoder::<Option<String>>::encode_to_unless(data, ops.iter().map(T::key_str), None);
    let insert = hexane::Encoder::<bool>::encode_to(data, ops.iter().map(T::insert));
    let action = hexane::Encoder::<Action>::encode_to(data, ops.iter().map(T::action));
    let value_meta = hexane::Encoder::<ValueMeta>::encode_to(data, ops.iter().map(T::value_meta));
    let value_start = data.len();
    for bytes in ops.iter().filter_map(T::value) {
        data.extend_from_slice(&bytes);
    }
    let value = value_start..data.len();
    let pred_count = hexane::Encoder::<u32>::encode_to(data, ops.iter().map(T::pred_count));
    let pred_iter = ops.iter().map(T::pred).flat_map(|id| id.iter());
    let pred_actor = hexane::Encoder::<ActorIdx>::encode_to(
        data,
        pred_iter.clone().map(T::id_actor).map(&remap_actor),
    );
    let pred_ctr = hexane::DeltaEncoder::<i64>::encode_to(data, pred_iter.map(|id| id.icounter()));
    let expand = hexane::Encoder::<bool>::encode_to_unless(data, ops.iter().map(T::expand), false);
    let mark_name = hexane::Encoder::<Option<String>>::encode_to_unless(
        data,
        ops.iter().map(T::mark_name),
        None,
    );

    ChangeOpsColumns {
        obj_actor,
        obj_ctr,
        key_actor,
        key_ctr,
        key_str,
        insert,
        action,
        value_meta,
        value,
        pred_count,
        pred_actor,
        pred_ctr,
        expand,
        mark_name,
    }
}

// The many small mallocs in the remap_actors
// was causing some memory thrashing with dmalloc/wasm
// this structure allows for the vectors to be allocated
// once and reused (via trucate()) when creating a large number
// of changes (like on load)
#[derive(Debug, PartialEq)]
pub(crate) struct ActorMapper<'a> {
    seen_actors: Vec<bool>,
    pub(crate) mapping: Vec<Option<ActorIdx>>,
    actors: &'a [ActorId],
    other_actors: Vec<usize>,
}

impl<'a> ActorMapper<'a> {
    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = ActorId> + '_ {
        self.other_actors.iter().map(|i| self.actors[*i].clone())
    }

    pub(crate) fn new(actors: &'a [ActorId]) -> ActorMapper<'a> {
        let len = actors.len();
        ActorMapper {
            seen_actors: vec![false; len],
            mapping: vec![None; len],
            actors,
            other_actors: vec![],
        }
    }

    fn reset(&mut self) {
        let len = self.actors.len();
        self.seen_actors.truncate(0);
        self.mapping.truncate(0);
        self.other_actors.truncate(0);

        self.seen_actors.resize(len, false);
        self.mapping.resize(len, None);
    }

    pub(crate) fn process_actor(&mut self, actor: usize) {
        self.seen_actors[actor] = true;
    }

    pub(crate) fn process_op<C>(&mut self, op: &C)
    where
        C: AsChangeOp,
    {
        if let Some(actor) = C::obj_actor(op) {
            self.process_actor(usize::from(actor))
        }
        if let Some(actor) = C::key_actor(op) {
            self.process_actor(usize::from(actor));
        }
        for id in C::pred(op) {
            self.process_actor(id.actor());
        }
    }

    pub(crate) fn build_mapping(&mut self, default_actor: Option<usize>) {
        let mut seen_index = 0;

        if let Some(actor) = default_actor {
            self.seen_actors[actor] = false;
            self.mapping[actor] = Some(ActorIdx(0));
            seen_index = 1;
        }

        for (index, seen) in self.seen_actors.iter().enumerate() {
            if *seen {
                self.other_actors.push(index);
                self.mapping[index] = Some(ActorIdx(seen_index));
                seen_index += 1;
            }
        }
    }

    fn remap_actors<C>(&mut self, ops: &[C], change_actor: usize)
    where
        C: AsChangeOp,
    {
        self.reset();

        for op in ops {
            self.process_op(op);
        }

        self.build_mapping(Some(change_actor));
    }
}

/// The change columns of a transaction's first `ops` ops, encoded once
/// so that committing the transaction need not encode them again.
///
/// The ops' change fields never change once they are pending (a
/// `batch_write` moves only their positions and undo links), so these
/// bytes stay theirs until the commit.
#[derive(Clone)]
pub(crate) struct EncodedOps {
    /// How many of the transaction's ops are encoded.
    pub(crate) ops: usize,
    /// How many predecessors those ops name, the length of the
    /// `pred_actor` and `pred_ctr` columns.
    preds: usize,
    /// The actor numbering the columns were written with.
    mapping: Vec<Option<ActorIdx>>,
    pub(crate) data: Vec<u8>,
    pub(crate) cols: ChangeOpsColumns,
}

impl std::fmt::Debug for EncodedOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncodedOps")
            .field("ops", &self.ops)
            .field("bytes", &self.data.len())
            .finish()
    }
}

impl EncodedOps {
    /// `ops` encoded as a change by `change_actor` would encode them.
    pub(crate) fn new<T: AsChangeOp>(
        ops: &[T],
        change_actor: usize,
        actors: &[ActorId],
    ) -> EncodedOps {
        let mut mapper = ActorMapper::new(actors);
        mapper.remap_actors(ops, change_actor);
        let mut data = Vec::new();
        let cols = encode_change_ops(ops, &mut data, &mapper.mapping);
        EncodedOps {
            ops: ops.len(),
            preds: ops.iter().map(|op| T::pred(op).len()).sum(),
            mapping: mapper.mapping,
            data,
            cols,
        }
    }

    /// The columns of all of `ops`, of which these are the first: the
    /// rest encoded and joined on, byte for byte as one encoding of
    /// `ops` writes them. `None` where these cannot be reused: `ops`
    /// is shorter, or its actors number differently, as when a later op
    /// names a new one.
    fn extend<T: AsChangeOp>(
        self,
        ops: &[T],
        change_actor: usize,
        mapper: &mut ActorMapper<'_>,
    ) -> Option<(Vec<u8>, ChangeOpsColumns)> {
        if ops.len() < self.ops || ops.is_empty() {
            return None;
        }
        mapper.remap_actors(ops, change_actor);
        if mapper.mapping != self.mapping {
            return None;
        }
        let tail = &ops[self.ops..];
        if tail.is_empty() {
            return Some((self.data, self.cols));
        }
        let mut tail_data = Vec::new();
        let tail_cols = encode_change_ops(tail, &mut tail_data, &mapper.mapping);
        let tail_preds = tail.iter().map(|op| T::pred(op).len()).sum();
        Some(self.join(&tail_data, &tail_cols, tail.len(), tail_preds))
    }

    /// These columns followed by `tail`'s, over `m` ops naming
    /// `m_preds` predecessors, written as one encoding writes them:
    /// each column is loaded, the tail's values spliced on, and saved,
    /// so runs meeting at the join merge.
    ///
    /// # Panics
    ///
    /// Where a column does not load: both sides are bytes just encoded.
    fn join(
        self,
        tail_data: &[u8],
        tail: &ChangeOpsColumns,
        m: usize,
        m_preds: usize,
    ) -> (Vec<u8>, ChangeOpsColumns) {
        use hexane::LoadOpts;
        const LOADS: &str = "a column just encoded loads";
        let (pre, pre_data, n, n_preds) = (&self.cols, &self.data, self.ops, self.preds);
        let mut out = Vec::with_capacity(pre_data.len() + tail_data.len());
        // A column left out where every value is `$u`.
        macro_rules! join_unless {
            ($ty:ty, $f:ident, $u:expr) => {{
                let opts = |len| LoadOpts::new().with_length(len).with_fill($u);
                let mut c = <$ty>::load_with(&pre_data[pre.$f.clone()], opts(n)).expect(LOADS);
                let t = <$ty>::load_with(&tail_data[tail.$f.clone()], opts(m)).expect(LOADS);
                c.splice(n, 0, t.iter());
                c.save_to_unless(&mut out, $u)
            }};
        }
        // A column always written, `$a` values long before the join and
        // `$b` after.
        macro_rules! join {
            ($ty:ty, $f:ident, $a:expr, $b:expr) => {{
                let opts = |len| LoadOpts::new().with_length(len);
                let mut c = <$ty>::load_with(&pre_data[pre.$f.clone()], opts($a)).expect(LOADS);
                let t = <$ty>::load_with(&tail_data[tail.$f.clone()], opts($b)).expect(LOADS);
                c.splice($a, 0, t.iter());
                c.save_to(&mut out)
            }};
        }
        let obj_actor = join_unless!(hexane::Column<Option<ActorIdx>>, obj_actor, None);
        let obj_ctr = join_unless!(hexane::Column<Option<u64>>, obj_ctr, None);
        let key_actor = join_unless!(hexane::Column<Option<ActorIdx>>, key_actor, None);
        let key_ctr = join_unless!(hexane::DeltaColumn<Option<i64>>, key_ctr, None);
        let key_str = join_unless!(hexane::Column<Option<String>>, key_str, None);
        let insert = join!(hexane::Column<bool>, insert, n, m);
        let action = join!(hexane::Column<Action>, action, n, m);
        let value_meta = join!(hexane::Column<ValueMeta>, value_meta, n, m);
        let start = out.len();
        out.extend_from_slice(&pre_data[pre.value.clone()]);
        out.extend_from_slice(&tail_data[tail.value.clone()]);
        let value = start..out.len();
        let pred_count = join!(hexane::Column<u32>, pred_count, n, m);
        let pred_actor = join!(hexane::Column<ActorIdx>, pred_actor, n_preds, m_preds);
        let pred_ctr = join!(hexane::DeltaColumn<i64>, pred_ctr, n_preds, m_preds);
        let expand = join_unless!(hexane::Column<bool>, expand, false);
        let mark_name = join_unless!(hexane::Column<Option<String>>, mark_name, None);
        let cols = ChangeOpsColumns {
            obj_actor,
            obj_ctr,
            key_actor,
            key_ctr,
            key_str,
            insert,
            action,
            value_meta,
            value,
            pred_count,
            pred_actor,
            pred_ctr,
            expand,
            mark_name,
        };
        (out, cols)
    }
}
