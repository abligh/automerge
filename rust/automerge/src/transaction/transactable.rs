use std::borrow::Cow;

use crate::exid::ExId;
use crate::iter::Span;
use crate::marks::{ExpandMark, Mark, UpdateSpansConfig};
use crate::{AutomergeError, ChangeHash, ObjType, Prop, ReadDoc, ScalarValue};

/// One write of a [`Transactable::batch_write`]: to a map key, a scalar
/// put or an increment of a counter.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchWrite {
    /// Set the key to the value, as [`Transactable::put`] does.
    Put(ScalarValue),
    /// Add to the counter at the key, as [`Transactable::increment`] does.
    Increment(i64),
}

/// A way of mutating a document within a single change.
pub trait Transactable: ReadDoc {
    /// Get the number of pending operations in this transaction.
    fn pending_ops(&self) -> usize;

    /// Set the value of property `P` to value `V` in object `obj`.
    ///
    /// # Errors
    ///
    /// This will return an error if
    /// - The object does not exist
    /// - The key is the wrong type for the object
    /// - The key does not exist in the object
    fn put<O: AsRef<ExId>, P: Into<Prop>, V: Into<ScalarValue>>(
        &mut self,
        obj: O,
        prop: P,
        value: V,
    ) -> Result<(), AutomergeError>;

    /// Set the value of property `P` to the new object `V` in object `obj`.
    ///
    /// # Returns
    ///
    /// The id of the object which was created.
    ///
    /// # Errors
    ///
    /// This will return an error if
    /// - The object does not exist
    /// - The key is the wrong type for the object
    /// - The key does not exist in the object
    fn put_object<O: AsRef<ExId>, P: Into<Prop>>(
        &mut self,
        obj: O,
        prop: P,
        object: ObjType,
    ) -> Result<ExId, AutomergeError>;

    /// Insert a value into a list at the given index.
    fn insert<O: AsRef<ExId>, V: Into<ScalarValue>>(
        &mut self,
        obj: O,
        index: usize,
        value: V,
    ) -> Result<(), AutomergeError>;

    /// Insert an object into a list at the given index.
    fn insert_object<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        index: usize,
        object: ObjType,
    ) -> Result<ExId, AutomergeError>;

    /// Increment the counter at the prop in the object by `value`.
    fn increment<O: AsRef<ExId>, P: Into<Prop>>(
        &mut self,
        obj: O,
        prop: P,
        value: i64,
    ) -> Result<(), AutomergeError>;

    /// Delete the value at prop in the object.
    fn delete<O: AsRef<ExId>, P: Into<Prop>>(
        &mut self,
        obj: O,
        prop: P,
    ) -> Result<(), AutomergeError>;

    /// replace a section of a list. If `del` is positive then N values
    /// are deleted after position `pos` and the new values inserted. If
    /// it is negative then N values are deleted before position `pos` instead.
    ///
    /// Values can be scalars or nested objects (maps, lists, text). Scalar
    /// values are inserted directly, while nested objects are created using
    /// batch insertion for efficiency.
    fn splice<O: AsRef<ExId>, V: Into<crate::hydrate::Value>, I: IntoIterator<Item = V>>(
        &mut self,
        obj: O,
        pos: usize,
        del: isize,
        vals: I,
    ) -> Result<(), AutomergeError>;

    /// Like [`Self::splice`] but for text.
    fn splice_text<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        pos: usize,
        del: isize,
        text: &str,
    ) -> Result<(), AutomergeError>;

    /// Mark a sequence
    fn mark<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        mark: Mark,
        expand: ExpandMark,
    ) -> Result<(), AutomergeError>;

    /// Remove a Mark from a sequence
    fn unmark<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        key: &str,
        start: usize,
        end: usize,
        expand: ExpandMark,
    ) -> Result<(), AutomergeError>;

    /// Insert a block marker into the text object `obj` at the given index.
    ///
    /// # Returns
    ///
    /// The ID of the new block marker. The block marker is a plain old map so you can use all the
    /// normal methods of modifying a map to interact with it.
    fn split_block<O>(&mut self, obj: O, index: usize) -> Result<ExId, AutomergeError>
    where
        O: AsRef<ExId>;

    /// Delete a block marker at `index` from the text object `obj`.
    fn join_block<O: AsRef<ExId>>(&mut self, text: O, index: usize) -> Result<(), AutomergeError>;

    /// Replace a block marker at `index` in `obj` with a new marker and return the ID of the new
    /// marker
    fn replace_block<O>(&mut self, text: O, index: usize) -> Result<ExId, AutomergeError>
    where
        O: AsRef<ExId>;

    /// Update the blocks and text in a text object
    ///
    /// This performs a diff against the current state of both the text and the block markers in a
    /// text object and attempts to perform a reasonably minimal set of operations to update the
    /// document to match the new text.
    ///
    /// The `config` argument is an [`UpdateSpansConfig`] object which is used to configure how the
    /// [`ExpandMark`] flag will be set for spans which are created as part of the reconciliation
    fn update_spans<O: AsRef<ExId>, I: IntoIterator<Item = Span>>(
        &mut self,
        text: O,
        config: UpdateSpansConfig,
        new_text: I,
    ) -> Result<(), AutomergeError>;

    /// The heads this transaction will be based on
    fn base_heads(&self) -> Vec<ChangeHash>;

    /// Update the value of a string
    ///
    /// This will calculate a diff between the current value and the new value and
    /// then convert that diff into calls to {@link splice}. This will produce results
    /// which don't merge as well as directly capturing the user input actions, but
    /// sometimes it's not possible to capture user input and this is the best you
    /// can do.
    fn update_text<S: AsRef<str>>(&mut self, obj: &ExId, new_text: S)
        -> Result<(), AutomergeError>;

    fn update_object<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        new_value: &crate::hydrate::Value,
    ) -> Result<(), crate::error::UpdateObjectError>;

    /// Make many scalar puts and counter increments to map keys as one
    /// batch.
    ///
    /// The result is exactly the ops the same calls to [`Self::put`] and
    /// [`Self::increment`], in that order, would make: the same ids, the
    /// same change, the same patches, in write order. What differs is
    /// the cost: the ops land in the op set in one pass per column rather
    /// than one splice each, so a batch of thousands costs a few
    /// microseconds a write instead of tens. Each entry gives the op's id
    /// as `put` would, `None` for a put of the value already there.
    ///
    /// A write to a list or text index, or to an object that is not a
    /// map, is refused with [`AutomergeError::InvalidOp`]; an increment
    /// of a key holding no counter with [`AutomergeError::MissingCounter`].
    /// On an error the writes before it stand, as the same calls one at
    /// a time would have left them.
    fn batch_write<O: AsRef<ExId>, P: Into<Prop>, I: IntoIterator<Item = (O, P, BatchWrite)>>(
        &mut self,
        writes: I,
    ) -> Result<Vec<Option<ExId>>, AutomergeError>;

    /// Put each of `entries`, a key and a map, list or text value, into
    /// the map `obj` as a new object tree, as [`Self::batch_create_object`]
    /// does for one.
    ///
    /// The document is what the same calls to `batch_create_object`, in
    /// that order, would leave, but every key's object is made before
    /// any of their contents, so the ops' ids differ. The cost differs
    /// too: the keys' ops land in the op set in one pass per column, as
    /// [`Self::batch_write`]'s do, and every object's contents in one
    /// append, rather than two splices an entry. A key given twice is
    /// written twice, the later winning. Returns each entry's new
    /// object.
    ///
    /// Refused, writing nothing, with [`AutomergeError::NotAnObject`]
    /// where an entry is a scalar; with [`AutomergeError::InvalidOp`]
    /// where `obj` is not a map. Patches are logged for every key, then
    /// for the objects' contents.
    fn batch_create_objects<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        entries: &[(String, crate::hydrate::Value)],
    ) -> Result<Vec<ExId>, AutomergeError>;

    /// Put or insert a nested `hydrate::Value` as a new object tree using
    /// batch operations.
    ///
    /// This is much faster than decomposing the value into individual put/insert
    /// operations because it only requires two OpSet splices instead of one per
    /// node.
    ///
    /// When `insert` is true and `prop` is a sequence index, the value is
    /// inserted at that index. When false, it overwrites the existing element.
    /// For map keys, `insert` is ignored.
    fn batch_create_object<O: AsRef<ExId>, P: Into<Prop>>(
        &mut self,
        obj: O,
        prop: P,
        value: &crate::hydrate::Value,
        insert: bool,
    ) -> Result<ExId, AutomergeError>;

    /// Overwrite the keys of the root object with the values from `value`
    ///
    /// This is useful to initialize an empty document with a large initial
    /// value. Note that existing keys which are not in `value` are left as is
    fn init_root_from_hydrate(&mut self, value: &crate::hydrate::Map)
        -> Result<(), AutomergeError>;
}

#[derive(Debug, PartialEq, Clone)]
pub enum BlockOrText<'a> {
    Block(crate::hydrate::Map),
    Text(Cow<'a, str>),
}
