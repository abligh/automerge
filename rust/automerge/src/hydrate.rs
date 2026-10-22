use crate::op_set2::types::KeyRef;
use crate::op_set2::{Op, OpSet, OpType};
use crate::types::{Clock, ObjId, ScalarValue, SequenceType};
use crate::TextEncoding;
use crate::{error::HydrateError, value, ObjType, Patch, PatchAction, Prop};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

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
    pub(crate) fn hydrate_root_in_one_pass(&self, encoding: TextEncoding) -> Value {
        let mut objects: HashMap<ObjId, Vec<Entry>> = HashMap::new();
        let mut current: Option<(ObjId, Vec<Entry>)> = None;
        // A text object is read whole by `hydrate_text`, so its own
        // ops, characters and marks, are passed over. Every object is
        // made before its ops, so its make op is met first.
        let mut texts: HashSet<ObjId> = HashSet::new();
        for top in self.top_ops_of_all() {
            if texts.contains(&top.obj) {
                continue;
            }
            if current.as_ref().map(|(obj, _)| *obj) != Some(top.obj) {
                if let Some((obj, entries)) = current.take() {
                    objects.insert(obj, entries);
                }
                current = Some((top.obj, Vec::new()));
            }
            let key = match top.elemid_or_key() {
                KeyRef::Map(key) => Some(key.into_owned()),
                KeyRef::Seq(_) => None,
            };
            let slot = match top.action() {
                OpType::Make(ObjType::Text) => {
                    texts.insert(top.id.into());
                    Slot::Value(self.hydrate_text(&top.id.into(), None, encoding))
                }
                OpType::Make(typ) => Slot::Object(top.id.into(), typ),
                OpType::Put(scalar) => Slot::Value(Value::Scalar(scalar.into())),
                // Marks belong to text, read with it; nothing else is a
                // top op.
                _ => continue,
            };
            let entries = &mut current.as_mut().expect("set above").1;
            entries.push(Entry {
                key,
                slot,
                conflict: top.conflict,
            });
        }
        if let Some((obj, entries)) = current {
            objects.insert(obj, entries);
        }
        assemble(&mut objects, &ObjId::root(), ObjType::Map)
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

/// One top op of an object, as [`OpSet::hydrate_root_in_one_pass`]
/// gathers it: its map key, if the object is a map, and its value or
/// the object it makes.
struct Entry {
    key: Option<String>,
    slot: Slot,
    conflict: bool,
}

enum Slot {
    Value(Value),
    Object(ObjId, ObjType),
}

/// The value of `obj`, of type `typ`, built from the entries gathered
/// for it and, recursively, for the objects they make; each is taken
/// out of `objects`. An object with no entries is empty.
fn assemble(objects: &mut HashMap<ObjId, Vec<Entry>>, obj: &ObjId, typ: ObjType) -> Value {
    let entries = objects.remove(obj).unwrap_or_default();
    let mut value = |slot| match slot {
        Slot::Value(value) => value,
        Slot::Object(child, typ) => assemble(objects, &child, typ),
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
