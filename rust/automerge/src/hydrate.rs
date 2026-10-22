use crate::op_set2::types::{Action, KeyRef};
use crate::op_set2::{Op, OpSet, OpType};
use crate::types::{Clock, ObjId, ScalarValue, SequenceType};
use crate::TextEncoding;
use crate::{error::HydrateError, value, ObjType, Patch, PatchAction, Prop};
use std::borrow::Cow;
use std::collections::HashMap;

mod list;
mod map;
mod text;

#[cfg(test)]
mod tests;

use crate::Automerge;

pub use list::{List, ListValue};
pub use map::{Map, MapValue};
pub use text::Text;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Scalar(ScalarValue),
    Map(Map),
    List(List),
    Text(Text),
}

/// A value as [`Gathered::take`] builds it: what [`Value`] holds for a
/// reader that only reads it, with no hash map per map, no conflict
/// flags and no marks.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// A map's or table's entries. The keys are unique and in
    /// ascending order as `str`'s `Ord` sorts them, which is the order
    /// Automerge keeps map keys in, so a reader may binary search them.
    Map(Vec<(String, Node)>),
    /// A list's elements, in order.
    List(Vec<Node>),
    /// A scalar.
    Scalar(ScalarValue),
    /// A text object's characters.
    Text(String),
}

impl Node {
    /// A map of `entries`, which must be as [`Node::Map`] says: keys
    /// unique and sorted (checked in debug builds).
    fn map(entries: Vec<(String, Node)>) -> Self {
        debug_assert!(
            entries.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "map keys unique and sorted"
        );
        Node::Map(entries)
    }
}

impl From<&Value> for Node {
    /// The node holding what `value` holds: a map's entries sorted by
    /// key, a text's characters, and neither conflict flags nor marks.
    fn from(value: &Value) -> Self {
        match value {
            Value::Scalar(scalar) => Node::Scalar(scalar.clone()),
            Value::Map(map) => {
                let mut entries: Vec<(String, Node)> = map
                    .iter()
                    .map(|(key, entry)| (key.clone(), Node::from(&entry.value)))
                    .collect();
                entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                Node::map(entries)
            }
            Value::List(list) => {
                Node::List(list.iter().map(|entry| Node::from(&entry.value)).collect())
            }
            Value::Text(text) => Node::Text(text.to_string()),
        }
    }
}

impl Value {
    pub fn new<'a, V: Into<crate::Value<'a>>>(value: V, text_encoding: TextEncoding) -> Self {
        match value.into() {
            value::Value::Object(ObjType::Map) => Value::Map(Map::default()),
            value::Value::Object(ObjType::List) => Value::List(List::default()),
            value::Value::Object(ObjType::Text) => Value::Text(Text::new(text_encoding, "")),
            value::Value::Object(ObjType::Table) => Value::Map(Map::default()),
            value::Value::Scalar(s) => Value::Scalar(s.into_owned()),
        }
    }

    pub fn map() -> Self {
        Value::Map(Map::default())
    }

    pub fn list() -> Self {
        Value::List(List::default())
    }

    pub fn text(text_encoding: TextEncoding, s: &str) -> Self {
        Value::Text(Text::new(text_encoding, s))
    }

    pub fn scalar<V: Into<ScalarValue>>(val: V) -> Self {
        Value::Scalar(val.into())
    }

    pub fn is_scalar(&self) -> bool {
        matches!(self, Value::Scalar(_))
    }

    pub(crate) fn width(&self, seq_type: SequenceType, encoding: TextEncoding) -> usize {
        match seq_type {
            SequenceType::List => 1,
            SequenceType::Text => encoding.width(self.as_str()),
        }
    }

    pub fn is_object(&self) -> bool {
        !self.is_scalar()
    }

    pub(crate) fn as_str(&self) -> &str {
        match &self {
            Self::Scalar(ScalarValue::Str(s)) => s,
            _ => "\u{fffc}",
        }
    }

    pub fn apply_patches<P: IntoIterator<Item = Patch>>(
        &mut self,
        text_encoding: TextEncoding,
        patches: P,
    ) -> Result<(), HydrateError> {
        for p in patches {
            self.apply(p.path.iter().map(|(_, prop)| prop), text_encoding, p.action)?;
        }
        Ok(())
    }

    pub(crate) fn apply<'a, P: Iterator<Item = &'a Prop>>(
        &mut self,
        mut path: P,
        text_encoding: TextEncoding,
        patch: PatchAction,
    ) -> Result<(), HydrateError> {
        match (path.next(), self) {
            (Some(Prop::Seq(n)), Value::List(list)) => list
                .get_mut(*n)
                .ok_or_else(|| HydrateError::ApplyInvalidProp(patch.clone()))?
                .apply(path, text_encoding, patch),
            (Some(Prop::Map(s)), Value::Map(map)) => map
                .get_mut(s)
                .ok_or_else(|| HydrateError::ApplyInvalidProp(patch.clone()))?
                .apply(path, text_encoding, patch),
            // Hydrated text represents embedded block objects only by their
            // replacement characters, so patches to a block's contents do
            // not affect the hydrated text value.
            (Some(Prop::Seq(_)), Value::Text(_)) => Ok(()),
            (None, Value::Map(map)) => map.apply(text_encoding, patch),
            (None, Value::List(list)) => list.apply(text_encoding, patch),
            (None, Value::Text(text)) => text.apply(text_encoding, patch),
            _ => Err(HydrateError::Fail),
        }
    }

    pub fn as_map(&mut self) -> Option<&mut Map> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_list(&mut self) -> Option<&mut List> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> i64 {
        match self {
            Value::Scalar(s) => s.as_i64_or_default(),
            _ => 0,
        }
    }
}

impl From<Value> for value::Value<'_> {
    fn from(value: Value) -> Self {
        match value {
            Value::Map(_) => value::Value::Object(ObjType::Map),
            Value::List(_) => value::Value::Object(ObjType::List),
            Value::Text(_) => value::Value::Object(ObjType::Text),
            Value::Scalar(s) => value::Value::Scalar(Cow::Owned(s)),
        }
    }
}

impl From<Map> for Value {
    fn from(value: Map) -> Self {
        Value::Map(value)
    }
}

impl From<List> for Value {
    fn from(value: List) -> Self {
        Value::List(value)
    }
}

impl From<Text> for Value {
    fn from(value: Text) -> Self {
        Value::Text(value)
    }
}

impl From<&Value> for value::Value<'_> {
    fn from(value: &Value) -> Self {
        match value {
            Value::Map(_) => value::Value::Object(ObjType::Map),
            Value::List(_) => value::Value::Object(ObjType::List),
            Value::Text(_) => value::Value::Object(ObjType::Text),
            Value::Scalar(s) => value::Value::Scalar(Cow::Owned(s.clone())),
        }
    }
}

impl From<HashMap<&str, Value>> for Value {
    fn from(value: HashMap<&str, Value>) -> Self {
        Value::Map(value.into())
    }
}

impl<'a> From<&'a str> for Value {
    fn from(value: &'a str) -> Self {
        Value::Scalar(ScalarValue::Str(value.into()))
    }
}

impl From<u32> for Value {
    fn from(value: u32) -> Self {
        Value::Scalar(ScalarValue::Uint(value as u64))
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Value::Scalar(ScalarValue::Uint(value))
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Value::Scalar(ScalarValue::Int(value as i64))
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Value::Scalar(ScalarValue::Int(value))
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Scalar(ScalarValue::F64(value))
    }
}

impl From<f32> for Value {
    fn from(value: f32) -> Self {
        Value::Scalar(ScalarValue::F64(value as f64))
    }
}

impl From<ScalarValue> for Value {
    fn from(value: ScalarValue) -> Self {
        Value::Scalar(value)
    }
}

impl Automerge {
    pub(crate) fn hydrate_map(&self, obj: &ObjId, clock: Option<&Clock>) -> Value {
        self.ops().hydrate_map(obj, clock, self.text_encoding())
    }
    pub(crate) fn hydrate_list(&self, obj: &ObjId, clock: Option<&Clock>) -> Value {
        self.ops().hydrate_list(obj, clock, self.text_encoding())
    }
    pub(crate) fn hydrate_text(&self, obj: &ObjId, clock: Option<&Clock>) -> Value {
        self.ops().hydrate_text(obj, clock, self.text_encoding())
    }
}

impl OpSet {
    pub(crate) fn hydrate_map(
        &self,
        obj: &ObjId,
        clock: Option<&Clock>,
        encoding: TextEncoding,
    ) -> Value {
        let mut map = Map::new();
        for top in self.top_ops(obj, clock.cloned()) {
            let key = self.to_string(top.elemid_or_key());
            let id = self.id_to_exid(top.id);
            let conflict = top.conflict;
            let value = self.hydrate_op(top, clock, encoding);
            map.insert(key, MapValue::new(value, id, conflict));
        }
        Value::Map(map)
    }

    pub(crate) fn hydrate_list(
        &self,
        obj: &ObjId,
        clock: Option<&Clock>,
        encoding: TextEncoding,
    ) -> Value {
        let mut list = List::new();
        for top in self.top_ops(obj, clock.cloned()) {
            //let id = top.exid();
            let id = self.id_to_exid(top.id);
            let conflict = top.conflict;
            let value = self.hydrate_op(top, clock, encoding);
            list.push(value, id, conflict);
        }
        Value::List(list)
    }

    pub(crate) fn hydrate_text(
        &self,
        obj: &ObjId,
        clock: Option<&Clock>,
        encoding: TextEncoding,
    ) -> Value {
        let text = self.text(obj, clock.cloned());
        Value::Text(Text::new(encoding, text))
    }

    /// The root map, as [`OpSet::hydrate_map`] gives it for the root
    /// with no clock, but read in one ordered pass over the op set:
    /// each object's top ops are gathered as the pass reaches them, and
    /// the tree is assembled from the root afterwards. Hydrating one
    /// object at a time seeks every column to each object's ops, which
    /// for a document of many small maps costs more than reading them.
    /// Every object's top ops, gathered in one ordered pass, for
    /// [`Gathered::hydrate`] to build objects from.
    pub(crate) fn gather_in_one_pass(&self) -> Gathered {
        let mut objects: HashMap<ObjId, Vec<Entry>> = HashMap::new();
        let mut in_text: HashMap<ObjId, Vec<ObjId>> = HashMap::new();
        // Each object met inside a text, mapped to that text.
        let mut text_of: HashMap<ObjId, ObjId> = HashMap::new();
        // The object whose top ops are being read, and its entries, or
        // `None` for a text: a text object is read whole by
        // `hydrate_text` when it is built, so its own ops, characters
        // and marks, are passed over. An object inside a text, such as a
        // block map, is gathered and recorded in `in_text`, so that
        // building the text, which reads it whole, takes it out. An
        // object comes after its parent in op set order, so its parent
        // has been met.
        let mut current: Option<(ObjId, Option<Vec<Entry>>)> = None;
        for top in self.top_rows_of_all() {
            if current.as_ref().map(|(obj, _)| *obj) != Some(top.obj) {
                if let Some((obj, Some(entries))) = current.take() {
                    objects.insert(obj, entries);
                }
                let is_text = self.object_type(&top.obj) == Some(ObjType::Text);
                if is_text {
                    text_of.insert(top.obj, top.obj);
                } else if let Some(text) = self
                    .object_parent(&top.obj)
                    .and_then(|parent| text_of.get(&parent).copied())
                {
                    text_of.insert(top.obj, text);
                    in_text.entry(text).or_default().push(top.obj);
                }
                current = Some((top.obj, (!is_text).then(Vec::new)));
            }
            let Some((_, Some(entries))) = current.as_mut() else {
                continue;
            };
            // A sequence's element has a `Seq` key column, whether it
            // was inserted or overwrites an element.
            let key = match top.key {
                KeyRef::Map(key) => Some(key.into_owned()),
                KeyRef::Seq(_) => None,
            };
            let slot = match top.action {
                Action::MakeMap => Slot::Object(top.id.into(), ObjType::Map),
                Action::MakeTable => Slot::Object(top.id.into(), ObjType::Table),
                Action::MakeList => Slot::Object(top.id.into(), ObjType::List),
                Action::MakeText => Slot::Object(top.id.into(), ObjType::Text),
                Action::Set => Slot::Scalar(top.value.into()),
                // Marks belong to text, read with it; nothing else is a
                // top op.
                _ => continue,
            };
            entries.push(Entry {
                key,
                slot,
                conflict: top.conflict,
            });
        }
        if let Some((obj, Some(entries))) = current {
            objects.insert(obj, entries);
        }
        objects.shrink_to_fit();
        Gathered { objects, in_text }
    }

    pub(crate) fn hydrate_op(
        &self,
        op: Op<'_>,
        clock: Option<&Clock>,
        encoding: TextEncoding,
    ) -> Value {
        match op.action() {
            OpType::Make(ObjType::Map) => self.hydrate_map(&op.id.into(), clock, encoding),
            OpType::Make(ObjType::Table) => self.hydrate_map(&op.id.into(), clock, encoding),
            OpType::Make(ObjType::List) => self.hydrate_list(&op.id.into(), clock, encoding),
            OpType::Make(ObjType::Text) => self.hydrate_text(&op.id.into(), clock, encoding),
            OpType::Put(scalar) => Value::Scalar(scalar.into()),
            _ => panic!("invalid op to hydrate"),
        }
    }
}

/// Every object's top ops at the current heads, gathered by
/// [`Automerge::gather_in_one_pass`] in one ordered pass over the ops,
/// so that [`Automerge::hydrate_gathered`] can build each object
/// without seeking to it. Building an object takes its ops, and those
/// of the objects inside it, out of the gathering, so each object can
/// be built once; the gathering shrinks as objects are built.
#[derive(Debug, Default)]
pub struct Gathered {
    objects: HashMap<ObjId, Vec<Entry>>,
    /// For each text object, the objects inside it (block maps and the
    /// objects in those), which building the text, read whole from the
    /// document, takes out of the gathering.
    in_text: HashMap<ObjId, Vec<ObjId>>,
}

/// One top op of an object, as [`Gathered`] holds it: its map key, if
/// the object is a map, and its value or the object it makes.
#[derive(Debug)]
struct Entry {
    key: Option<String>,
    slot: Slot,
    conflict: bool,
}

#[derive(Debug)]
enum Slot {
    Scalar(ScalarValue),
    Object(ObjId, ObjType),
}

impl Gathered {
    /// How many objects are gathered and not yet built.
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Whether every gathered object has been built.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Takes the objects inside the text `text` out of the gathering:
    /// the text is read whole from the document.
    fn take_text_objects(&mut self, text: &ObjId) {
        for obj in self.in_text.remove(text).unwrap_or_default() {
            self.objects.remove(&obj);
        }
    }

    /// The value of `obj`, of type `typ`, as [`Gathered::hydrate`]
    /// builds it, but as a [`Node`]: its gathered entries and,
    /// recursively, those of the objects they make, each taken out of
    /// the gathering. Text is read from `ops`. An object with no
    /// entries, or one already taken, is empty.
    pub(crate) fn take(&mut self, ops: &OpSet, obj: &ObjId, typ: ObjType) -> Node {
        if typ == ObjType::Text {
            self.take_text_objects(obj);
            return Node::Text(ops.text(obj, None));
        }
        let entries = self.objects.remove(obj).unwrap_or_default();
        let mut node = |slot| match slot {
            Slot::Scalar(scalar) => Node::Scalar(scalar),
            Slot::Object(child, typ) => self.take(ops, &child, typ),
        };
        match typ {
            ObjType::Map | ObjType::Table => Node::map(
                entries
                    .into_iter()
                    .map(|Entry { key, slot, .. }| (key.unwrap_or_default(), node(slot)))
                    .collect(),
            ),
            ObjType::List | ObjType::Text => Node::List(
                entries
                    .into_iter()
                    .map(|Entry { slot, .. }| node(slot))
                    .collect(),
            ),
        }
    }

    /// The value of `obj`, of type `typ`, built from its gathered
    /// entries and, recursively, those of the objects they make, each
    /// taken out of the gathering. Text is read from `ops`. An object
    /// with no entries, or one already built, is empty.
    pub(crate) fn hydrate(
        &mut self,
        ops: &OpSet,
        obj: &ObjId,
        typ: ObjType,
        encoding: TextEncoding,
    ) -> Value {
        if typ == ObjType::Text {
            self.take_text_objects(obj);
            return ops.hydrate_text(obj, None, encoding);
        }
        let entries = self.objects.remove(obj).unwrap_or_default();
        let mut value = |slot| match slot {
            Slot::Scalar(scalar) => Value::Scalar(scalar),
            Slot::Object(child, typ) => self.hydrate(ops, &child, typ, encoding),
        };
        match typ {
            ObjType::Map | ObjType::Table => {
                let mut map = Map::new();
                for Entry {
                    key,
                    slot,
                    conflict,
                } in entries
                {
                    let value = value(slot);
                    map.insert(key.unwrap_or_default(), MapValue { value, conflict });
                }
                Value::Map(map)
            }
            ObjType::List | ObjType::Text => {
                let mut list = List::new();
                for Entry { slot, conflict, .. } in entries {
                    list.push_value(value(slot), conflict);
                }
                Value::List(list)
            }
        }
    }
}

#[macro_export]
macro_rules! hydrate_map {
    {} => {
        $crate::hydrate::Map::default()
    };
    {$($k: expr => $v: expr),* $(,)?} => {
        $crate::hydrate::Map::from(std::collections::HashMap::from([$(($k, $crate::hydrate::Value::from($v)),)*]))
    };
}

#[macro_export]
macro_rules! hydrate_list {
    {$($v: expr),* $(,)?} => {
        $crate::hydrate::List::from(Vec::<$crate::hydrate::Value>::from([$($crate::hydrate::Value::from($v),)*]))
    };
}

#[macro_export]
macro_rules! hydrate_text {
    {$t: expr} => {
        $crate::hydrate::Text::new($crate::TextEncoding::platform_default(), $t)
    };
}

#[cfg(feature = "wasm")]
impl From<Value> for wasm_bindgen::JsValue {
    fn from(value: Value) -> Self {
        (&value).into()
    }
}

#[cfg(feature = "wasm")]
impl From<&Value> for wasm_bindgen::JsValue {
    fn from(value: &Value) -> Self {
        use js_sys::{Array, Date, Object, Reflect, Uint8Array};
        use wasm_bindgen::JsValue;
        match value {
            Value::Scalar(s) => match s {
                ScalarValue::Bytes(v) => Uint8Array::from(v.as_slice()).into(),
                ScalarValue::Str(v) => v.to_string().into(),
                ScalarValue::Int(v) => (*v as f64).into(),
                ScalarValue::Uint(v) => (*v as f64).into(),
                ScalarValue::F64(v) => (*v).into(),
                ScalarValue::Counter(v) => (f64::from(v)).into(),
                ScalarValue::Timestamp(v) => Date::new(&(*v as f64).into()).into(),
                ScalarValue::Boolean(v) => (*v).into(),
                ScalarValue::Null => JsValue::null(),
                ScalarValue::Unknown {
                    bytes,
                    type_code: _,
                } => Uint8Array::from(bytes.as_slice()).into(),
            },
            Value::Map(m) => {
                let result = Object::new();
                for (key, val) in m.iter() {
                    Reflect::set(&result, &key.into(), &JsValue::from(&val.value)).unwrap();
                }
                result.into()
            }
            Value::List(l) => l
                .iter()
                .map(|v| JsValue::from(&v.value))
                .collect::<Array>()
                .into(),
            Value::Text(t) => String::from(t).into(),
        }
    }
}
