//! Shared ECMAScript object, array, function and lexical-environment semantics.
//!
//! This backend-neutral layer is consumed by interpreter and native backends.
//! M1 implements ordinary data properties, string atoms, prototypes, sparse
//! arrays, function closures and lexical binding cells. Accessors, Symbols,
//! Proxy, classes and primitive boxing are intentionally not claimed.

use std::collections::BinaryHeap;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::value::{Value, ValueRepr, ValueType};

use super::{CellKind, GcRef, Heap, HeapError, release_vec_capacity, reserve_vec};

/// Stable function-table identity. It is metadata, never a code pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FunctionId(
    /// Numeric identity in the backend-owned function table.
    pub u32,
);

/// Stable lexical binding identity within a function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BindingId(
    /// Numeric identity in the function's lexical environment.
    pub u32,
);

/// Opaque interned string property key owned by one Runtime.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PropertyKey {
    heap_id: u64,
    index: u32,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for PropertyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PropertyKey(<atom>)")
    }
}

/// Partial data descriptor. Accessor descriptors are outside this M1 model.
#[derive(Clone, Copy, Debug, Default)]
pub struct PropertyDescriptor {
    /// Optional data value.
    pub value: Option<Value>,
    /// Optional writable flag.
    pub writable: Option<bool>,
    /// Optional enumerable flag.
    pub enumerable: Option<bool>,
    /// Optional configurable flag.
    pub configurable: Option<bool>,
}

impl PropertyDescriptor {
    /// Constructs a complete data descriptor.
    #[must_use]
    pub const fn data(value: Value, writable: bool, enumerable: bool, configurable: bool) -> Self {
        Self {
            value: Some(value),
            writable: Some(writable),
            enumerable: Some(enumerable),
            configurable: Some(configurable),
        }
    }

    /// Constructs a value-only descriptor update.
    #[must_use]
    pub const fn value(value: Value) -> Self {
        Self {
            value: Some(value),
            writable: None,
            enumerable: None,
            configurable: None,
        }
    }
}

/// Fully materialized own data descriptor.
#[derive(Clone, Copy, Debug)]
pub struct CompletePropertyDescriptor {
    /// Property value.
    pub value: Value,
    /// Whether assignment may change the value.
    pub writable: bool,
    /// Whether the key is enumerable.
    pub enumerable: bool,
    /// Whether the property may be reconfigured or deleted.
    pub configurable: bool,
}

/// Opaque GC-managed lexical environment. It is not a JavaScript Value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct LexicalEnvironment {
    pub(super) reference: GcRef,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for LexicalEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LexicalEnvironment(<opaque>)")
    }
}

/// Binding state when a lexical environment is created.
#[derive(Clone, Copy, Debug)]
pub struct BindingDefinition {
    /// Stable binding identity.
    pub id: BindingId,
    /// Whether the binding is mutable.
    pub mutable: bool,
    /// Initial value. None means uninitialized (TDZ).
    pub initial_value: Option<Value>,
}

impl BindingDefinition {
    /// Creates an initialized mutable binding.
    #[must_use]
    pub const fn mutable(id: BindingId, value: Value) -> Self {
        Self {
            id,
            mutable: true,
            initial_value: Some(value),
        }
    }

    /// Creates a binding in TDZ.
    #[must_use]
    pub const fn uninitialized(id: BindingId, mutable: bool) -> Self {
        Self {
            id,
            mutable,
            initial_value: None,
        }
    }

    /// Creates an initialized immutable binding.
    #[must_use]
    pub const fn immutable(id: BindingId, value: Value) -> Self {
        Self {
            id,
            mutable: false,
            initial_value: Some(value),
        }
    }
}

/// Closure capture storage contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    /// Share an enclosing mutable binding cell.
    Cell,
    /// Copy an initialized immutable value.
    Value,
}

/// One captured lexical binding, mapped to a closure-local alias.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureDefinition {
    /// Binding in the enclosing environment.
    pub source: BindingId,
    /// Binding alias in the closure environment.
    pub alias: BindingId,
    /// Shared cell or copied value.
    pub mode: CaptureMode,
}

/// Static function metadata required to create a closure and prepare calls.
#[derive(Clone, Debug)]
pub struct FunctionDefinition {
    /// Stable function-table identity.
    pub id: FunctionId,
    /// Mutable formal parameters in source order.
    pub parameters: Vec<BindingDefinition>,
    /// Local bindings created for every activation.
    pub locals: Vec<BindingDefinition>,
    /// Explicit outer bindings imported into the closure.
    pub captures: Vec<CaptureDefinition>,
    /// Ordinary or lexical-receiver function.
    pub kind: FunctionKind,
    /// Ordinary strict functions preserve the supplied receiver.
    pub strict: bool,
}

/// Function receiver behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionKind {
    /// Ordinary function with call-site receiver.
    Ordinary,
    /// Arrow function with creation-time lexical receiver.
    Arrow,
}

/// Rooted function activation returned to a backend dispatcher.
///
/// The dispatcher must call leave_function_call on both normal and abrupt exit.
#[derive(Clone, Copy, Debug)]
pub struct CallActivation {
    frame: super::FrameToken,
    environment: LexicalEnvironment,
    function_id: FunctionId,
    this_value: Value,
}

impl CallActivation {
    /// Function-table identity to dispatch.
    #[must_use]
    pub const fn function_id(self) -> FunctionId {
        self.function_id
    }

    /// Activation lexical environment.
    #[must_use]
    pub const fn environment(self) -> LexicalEnvironment {
        self.environment
    }

    /// Receiver visible to the function body.
    #[must_use]
    pub const fn this_value(self) -> Value {
        self.this_value
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OwnProperty {
    pub(super) key: u32,
    pub(super) descriptor: PropertyDescriptorData,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PropertyDescriptorData {
    pub(super) value: Value,
    pub(super) writable: bool,
    pub(super) enumerable: bool,
    pub(super) configurable: bool,
}

#[derive(Debug)]
pub(super) struct ObjectData {
    pub(super) prototype: Option<GcRef>,
    pub(super) properties: Vec<OwnProperty>,
    pub(super) extensible: bool,
}

impl ObjectData {
    pub(super) fn new() -> Self {
        Self {
            prototype: None,
            properties: Vec::new(),
            extensible: true,
        }
    }
}

#[derive(Debug)]
pub(super) struct ArrayData {
    pub(super) object: ObjectData,
    pub(super) length: u32,
    pub(super) length_writable: bool,
}

impl ArrayData {
    pub(super) fn new() -> Self {
        Self {
            object: ObjectData::new(),
            length: 0,
            length_writable: true,
        }
    }
}

#[derive(Debug)]
pub(super) struct EnvironmentData {
    pub(super) parent: Option<GcRef>,
    pub(super) bindings: Vec<EnvironmentBinding>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct EnvironmentBinding {
    pub(super) id: BindingId,
    pub(super) storage: BindingStorage,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum BindingStorage {
    Cell(GcRef),
    Value {
        value: Value,
        initialized: bool,
    },
    /// Read-only live alias to another lexical binding (for ESM imports).
    Alias {
        environment: GcRef,
        binding: BindingId,
    },
}

#[derive(Clone, Copy, Debug)]
pub(super) struct BindingCellData {
    pub(super) value: Value,
    pub(super) mutable: bool,
    pub(super) initialized: bool,
}

#[derive(Debug)]
pub(super) struct FunctionData {
    pub(super) object: ObjectData,
    pub(super) id: Option<FunctionId>,
    pub(super) closure_environment: Option<GcRef>,
    pub(super) kind: FunctionKind,
    pub(super) strict: bool,
    pub(super) lexical_this: Value,
}

impl FunctionData {
    pub(super) fn unbound() -> Self {
        Self {
            object: ObjectData::new(),
            id: None,
            closure_environment: None,
            kind: FunctionKind::Ordinary,
            strict: true,
            lexical_this: Value::undefined(),
        }
    }

    pub(super) fn closure(
        id: FunctionId,
        environment: GcRef,
        kind: FunctionKind,
        strict: bool,
        lexical_this: Value,
    ) -> Self {
        Self {
            object: ObjectData::new(),
            id: Some(id),
            closure_environment: Some(environment),
            kind,
            strict,
            lexical_this,
        }
    }
}

impl Heap {
    /// Interns UTF-8 text as an ECMAScript property-key atom.
    pub fn intern_property_key(&mut self, text: &str) -> Result<PropertyKey, HeapError> {
        if let Some(index) = self
            .atoms
            .iter()
            .position(|entry| entry.iter().copied().eq(text.encode_utf16()))
        {
            return Ok(self.property_key_for_index(
                u32::try_from(index).map_err(|_| HeapError::HeapCapacityExceeded)?,
            ));
        }
        if self.atoms.len() >= u32::MAX as usize {
            return Err(HeapError::HeapCapacityExceeded);
        }
        let unit_count = text.encode_utf16().count();
        let mut atom = Vec::new();
        reserve_vec(&mut self.budget, &mut atom, unit_count)?;
        atom.extend(text.encode_utf16());
        if let Err(error) = reserve_vec(&mut self.budget, &mut self.atoms, 1) {
            release_vec_capacity(&mut self.budget, &atom)?;
            return Err(error);
        }
        let index = u32::try_from(self.atoms.len()).map_err(|_| HeapError::HeapCapacityExceeded)?;
        self.atoms.push(atom);
        Ok(self.property_key_for_index(index))
    }

    /// Interns UTF-16 key units without replacing lone surrogates.
    pub fn intern_property_key_utf16(&mut self, units: &[u16]) -> Result<PropertyKey, HeapError> {
        if let Some(index) = self
            .atoms
            .iter()
            .position(|entry| entry.as_slice() == units)
        {
            return Ok(self.property_key_for_index(
                u32::try_from(index).map_err(|_| HeapError::HeapCapacityExceeded)?,
            ));
        }
        if self.atoms.len() >= u32::MAX as usize {
            return Err(HeapError::HeapCapacityExceeded);
        }
        let mut atom = Vec::new();
        reserve_vec(&mut self.budget, &mut atom, units.len())?;
        atom.extend_from_slice(units);
        if let Err(error) = reserve_vec(&mut self.budget, &mut self.atoms, 1) {
            release_vec_capacity(&mut self.budget, &atom)?;
            return Err(error);
        }
        let index = u32::try_from(self.atoms.len()).map_err(|_| HeapError::HeapCapacityExceeded)?;
        self.atoms.push(atom);
        Ok(self.property_key_for_index(index))
    }

    /// Returns the UTF-16 units for a key owned by this heap.
    pub fn property_key_utf16(&self, key: PropertyKey) -> Result<&[u16], HeapError> {
        let index = self.validate_property_key(key)?;
        Ok(&self.atoms[index])
    }

    /// Performs primitive ToPropertyKey conversion for M1 values.
    ///
    /// Object-to-primitive conversion is deliberately rejected until the
    /// observable coercion hooks exist. Symbols are not in the M1 Value model.
    pub fn to_property_key(&mut self, value: Value) -> Result<PropertyKey, HeapError> {
        self.validate_value(value)?;
        match value.repr() {
            ValueRepr::String(reference) => self.intern_string_property_key(reference),
            ValueRepr::Undefined => self.intern_property_key("undefined"),
            ValueRepr::Null => self.intern_property_key("null"),
            ValueRepr::Boolean(false) => self.intern_property_key("false"),
            ValueRepr::Boolean(true) => self.intern_property_key("true"),
            ValueRepr::Number(number) => {
                self.intern_property_key(&number_to_property_string(number))
            }
            ValueRepr::Object(_) | ValueRepr::Function(_) => Err(HeapError::UnsupportedCoercion),
        }
    }

    /// Allocates an array with length zero, writable length and sparse elements.
    pub fn allocate_array(&mut self) -> Result<Value, HeapError> {
        let reference =
            self.allocate_cell_with_retry(|| CellKind::Array(Box::new(ArrayData::new())))?;
        Ok(Value::from_object(reference))
    }

    /// Returns an array's current length.
    pub fn array_length(&self, array: Value) -> Result<u32, HeapError> {
        let index = self.array_index(array)?;
        match &self.slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Array(data) => Ok(data.length),
            _ => Err(HeapError::InvalidObjectKind),
        }
    }

    /// Returns an own data descriptor, including an array's special length.
    pub fn get_own_property_descriptor(
        &self,
        object: Value,
        key: PropertyKey,
    ) -> Result<Option<CompletePropertyDescriptor>, HeapError> {
        let key_index = self.validate_property_key(key)?;
        let object_index = self.object_index(object)?;
        if self.is_array_length_key(key_index)
            && let CellKind::Array(array) = &self.slots[object_index]
                .cell
                .as_ref()
                .ok_or(HeapError::StaleReference)?
                .kind
        {
            return Ok(Some(CompletePropertyDescriptor {
                value: Value::number(f64::from(array.length)),
                writable: array.length_writable,
                enumerable: false,
                configurable: false,
            }));
        }
        let Some(position) = self.find_own_property(object_index, key_index)? else {
            return Ok(None);
        };
        let descriptor = self.object_data(object_index)?.properties[position].descriptor;
        Ok(Some(CompletePropertyDescriptor {
            value: descriptor.value,
            writable: descriptor.writable,
            enumerable: descriptor.enumerable,
            configurable: descriptor.configurable,
        }))
    }

    /// Performs generic own/inherited data-property lookup.
    pub fn get_property(&self, object: Value, key: PropertyKey) -> Result<Value, HeapError> {
        let key_index = self.validate_property_key(key)?;
        let mut current = self.object_reference(object)?;
        for _ in 0..=self.slots.len() {
            let index = self.validate_ref(current)?;
            if self.is_array_length_key(key_index)
                && let CellKind::Array(array) = &self.slots[index]
                    .cell
                    .as_ref()
                    .ok_or(HeapError::StaleReference)?
                    .kind
            {
                return Ok(Value::number(f64::from(array.length)));
            }
            if let Some(position) = self.find_own_property(index, key_index)? {
                return Ok(self.object_data(index)?.properties[position]
                    .descriptor
                    .value);
            }
            let Some(prototype) = self.object_data(index)?.prototype else {
                return Ok(Value::undefined());
            };
            current = prototype;
        }
        Err(HeapError::PrototypeCycle)
    }

    /// Checks own-property presence, including array length.
    pub fn has_own_property(&self, object: Value, key: PropertyKey) -> Result<bool, HeapError> {
        let key_index = self.validate_property_key(key)?;
        let index = self.object_index(object)?;
        if self.is_array_length_key(key_index)
            && matches!(
                &self.slots[index]
                    .cell
                    .as_ref()
                    .ok_or(HeapError::StaleReference)?
                    .kind,
                CellKind::Array(_)
            )
        {
            return Ok(true);
        }
        Ok(self.find_own_property(index, key_index)?.is_some())
    }

    /// Checks own and inherited property presence.
    pub fn has_property(&self, object: Value, key: PropertyKey) -> Result<bool, HeapError> {
        let key_index = self.validate_property_key(key)?;
        let mut current = self.object_reference(object)?;
        for _ in 0..=self.slots.len() {
            let index = self.validate_ref(current)?;
            if self.is_array_length_key(key_index)
                && matches!(
                    &self.slots[index]
                        .cell
                        .as_ref()
                        .ok_or(HeapError::StaleReference)?
                        .kind,
                    CellKind::Array(_)
                )
            {
                return Ok(true);
            }
            if self.find_own_property(index, key_index)?.is_some() {
                return Ok(true);
            }
            let Some(prototype) = self.object_data(index)?.prototype else {
                return Ok(false);
            };
            current = prototype;
        }
        Err(HeapError::PrototypeCycle)
    }

    /// Defines an own data property, enforcing non-configurable descriptor rules.
    ///
    /// Returns false rather than throwing when a descriptor is incompatible or
    /// the object is non-extensible. Callers apply strict assignment policy.
    pub fn define_own_property(
        &mut self,
        object: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<bool, HeapError> {
        let key_index = self.validate_property_key(key)?;
        let object_index = self.object_index(object)?;
        if let Some(value) = descriptor.value {
            self.validate_value(value)?;
        }
        if self.is_array_length_key(key_index)
            && matches!(
                &self.slots[object_index]
                    .cell
                    .as_ref()
                    .ok_or(HeapError::StaleReference)?
                    .kind,
                CellKind::Array(_)
            )
        {
            return self.define_array_length(object_index, descriptor);
        }

        if let Some(position) = self.find_own_property(object_index, key_index)? {
            let current = self.object_data(object_index)?.properties[position].descriptor;
            if !current.configurable {
                if descriptor.configurable == Some(true)
                    || descriptor
                        .enumerable
                        .is_some_and(|v| v != current.enumerable)
                {
                    return Ok(false);
                }
                if !current.writable {
                    if descriptor.writable == Some(true) {
                        return Ok(false);
                    }
                    if let Some(value) = descriptor.value
                        && !self.same_value(current.value, value)?
                    {
                        return Ok(false);
                    }
                }
            }
            let next = PropertyDescriptorData {
                value: descriptor.value.unwrap_or(current.value),
                writable: descriptor.writable.unwrap_or(current.writable),
                enumerable: descriptor.enumerable.unwrap_or(current.enumerable),
                configurable: descriptor.configurable.unwrap_or(current.configurable),
            };
            self.replace_property_value(object_index, current.value, next.value)?;
            self.object_data_mut(object_index)?.properties[position].descriptor = next;
            return Ok(true);
        }

        if !self.object_data(object_index)?.extensible {
            return Ok(false);
        }
        if let Some(array_index) = self.array_index_from_atom(key_index)
            && let CellKind::Array(array) = &self.slots[object_index]
                .cell
                .as_ref()
                .ok_or(HeapError::StaleReference)?
                .kind
            && array_index >= array.length
            && !array.length_writable
        {
            return Ok(false);
        }
        let next = PropertyDescriptorData {
            value: descriptor.value.unwrap_or_else(Value::undefined),
            writable: descriptor.writable.unwrap_or(false),
            enumerable: descriptor.enumerable.unwrap_or(false),
            configurable: descriptor.configurable.unwrap_or(false),
        };
        self.reserve_object_properties(object_index, 1)?;
        self.add_value_edge(object_index, next.value)?;
        self.object_data_mut(object_index)?
            .properties
            .push(OwnProperty {
                key: key_index as u32,
                descriptor: next,
            });
        if let Some(array_index) = self.array_index_from_atom(key_index)
            && let CellKind::Array(array) = &mut self.slots[object_index]
                .cell
                .as_mut()
                .ok_or(HeapError::StaleReference)?
                .kind
            && array_index >= array.length
        {
            array.length = array_index + 1;
        }
        Ok(true)
    }

    /// OrdinarySet for data properties with an explicit receiver.
    pub fn set_property_with_receiver(
        &mut self,
        target: Value,
        key: PropertyKey,
        value: Value,
        receiver: Value,
    ) -> Result<bool, HeapError> {
        self.validate_value(value)?;
        let key_index = self.validate_property_key(key)?;
        let target_ref = self.object_reference(target)?;
        let receiver_index = self.object_index(receiver)?;
        let mut current = Some(target_ref);
        for _ in 0..=self.slots.len() {
            let Some(reference) = current else {
                return self.define_assignment_property(receiver_index, key_index, value);
            };
            let index = self.validate_ref(reference)?;
            if self.is_array_length_key(key_index)
                && let CellKind::Array(array) = &self.slots[index]
                    .cell
                    .as_ref()
                    .ok_or(HeapError::StaleReference)?
                    .kind
            {
                if !array.length_writable {
                    return Ok(false);
                }
                if receiver_index != index {
                    return self.define_assignment_property(receiver_index, key_index, value);
                }
                return self.define_array_length(index, PropertyDescriptor::value(value));
            }
            if let Some(position) = self.find_own_property(index, key_index)? {
                if !self.object_data(index)?.properties[position]
                    .descriptor
                    .writable
                {
                    return Ok(false);
                }
                return self.define_assignment_property(receiver_index, key_index, value);
            }
            current = self.object_data(index)?.prototype;
        }
        Err(HeapError::PrototypeCycle)
    }

    /// Sets a property with the target object as receiver.
    pub fn set_property(
        &mut self,
        object: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<bool, HeapError> {
        self.set_property_with_receiver(object, key, value, object)
    }

    /// Strict assignment form: a rejected write becomes a TypeError-equivalent error.
    pub fn set_property_or_throw(
        &mut self,
        object: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), HeapError> {
        if self.set_property(object, key, value)? {
            Ok(())
        } else {
            Err(HeapError::ReadOnlyProperty)
        }
    }

    /// Deletes an own property. Deleting an array index leaves a hole.
    pub fn delete_property(&mut self, object: Value, key: PropertyKey) -> Result<bool, HeapError> {
        let key_index = self.validate_property_key(key)?;
        let object_index = self.object_index(object)?;
        if self.is_array_length_key(key_index)
            && matches!(
                &self.slots[object_index]
                    .cell
                    .as_ref()
                    .ok_or(HeapError::StaleReference)?
                    .kind,
                CellKind::Array(_)
            )
        {
            return Ok(false);
        }
        let Some(position) = self.find_own_property(object_index, key_index)? else {
            return Ok(true);
        };
        let property = self.object_data(object_index)?.properties[position];
        if !property.descriptor.configurable {
            return Ok(false);
        }
        self.object_data_mut(object_index)?
            .properties
            .remove(position);
        self.remove_value_edge(object_index, property.descriptor.value)?;
        Ok(true)
    }

    /// Prevents creation of new own properties.
    pub fn prevent_extensions(&mut self, object: Value) -> Result<(), HeapError> {
        let index = self.object_index(object)?;
        self.object_data_mut(index)?.extensible = false;
        Ok(())
    }

    /// Returns the current extensibility flag.
    pub fn is_extensible(&self, object: Value) -> Result<bool, HeapError> {
        Ok(self.object_data(self.object_index(object)?)?.extensible)
    }

    /// Returns the prototype or None for null.
    pub fn get_prototype_of(&self, object: Value) -> Result<Option<Value>, HeapError> {
        let index = self.object_index(object)?;
        self.object_data(index)?
            .prototype
            .map(|r| self.value_for_object_ref(r))
            .transpose()
    }

    /// Sets the prototype, returning false for cycles or a non-extensible target.
    pub fn set_prototype_of(
        &mut self,
        object: Value,
        prototype: Option<Value>,
    ) -> Result<bool, HeapError> {
        let object_ref = self.object_reference(object)?;
        let object_index = self.validate_ref(object_ref)?;
        let prototype_ref = prototype.map(|v| self.object_reference(v)).transpose()?;
        if prototype_ref == self.object_data(object_index)?.prototype {
            return Ok(true);
        }
        if !self.object_data(object_index)?.extensible {
            return Ok(false);
        }
        if let Some(mut current) = prototype_ref {
            for _ in 0..=self.slots.len() {
                if current == object_ref {
                    return Ok(false);
                }
                let index = self.validate_ref(current)?;
                let Some(next) = self.object_data(index)?.prototype else {
                    break;
                };
                current = next;
            }
        }
        if let Some(reference) = prototype_ref {
            self.add_edge_ref(object_ref, reference)?;
        }
        let previous = self.object_data(object_index)?.prototype;
        self.object_data_mut(object_index)?.prototype = prototype_ref;
        if let Some(reference) = previous {
            self.remove_edge_ref(object_ref, reference)?;
        }
        Ok(true)
    }

    /// Returns own keys: integer indices sorted numerically, then strings in insertion order.
    pub fn own_property_keys(&mut self, object: Value) -> Result<Vec<PropertyKey>, HeapError> {
        let index = self.object_index(object)?;
        let length_key = if matches!(
            &self.slots[index]
                .cell
                .as_ref()
                .ok_or(HeapError::StaleReference)?
                .kind,
            CellKind::Array(_)
        ) {
            Some(self.intern_property_key("length")?)
        } else {
            None
        };
        let properties = &self.object_data(index)?.properties;
        let mut indices = Vec::new();
        let mut strings = Vec::new();
        indices
            .try_reserve_exact(properties.len())
            .map_err(|_| HeapError::AllocationFailed)?;
        strings
            .try_reserve_exact(properties.len())
            .map_err(|_| HeapError::AllocationFailed)?;
        for property in properties {
            if let Some(array_index) = self.array_index_from_atom(property.key as usize) {
                indices.push((array_index, property.key));
            } else {
                strings.push(property.key);
            }
        }
        indices.sort_unstable_by_key(|entry| entry.0);
        let key_count = indices
            .len()
            .checked_add(strings.len())
            .and_then(|count| count.checked_add(usize::from(length_key.is_some())))
            .ok_or(HeapError::SizeOverflow)?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(key_count)
            .map_err(|_| HeapError::AllocationFailed)?;
        for (_, key) in indices {
            result.push(self.property_key_for_index(key));
        }
        if let Some(key) = length_key {
            result.push(key);
        }
        for key in strings {
            result.push(self.property_key_for_index(key));
        }
        Ok(result)
    }

    /// Returns at most `limit` own keys in ECMAScript property order.
    ///
    /// Integer-index keys are selected in ascending order, followed by an
    /// array's special `length` key and then string keys in insertion order.
    /// The method keeps temporary storage bounded by `limit`, even when the
    /// object itself has many more properties. The boolean is true when keys
    /// were omitted from the returned prefix.
    pub fn own_property_keys_bounded(
        &mut self,
        object: Value,
        limit: usize,
    ) -> Result<(Vec<PropertyKey>, bool), HeapError> {
        let index = self.object_index(object)?;
        let is_array = matches!(
            &self.slots[index]
                .cell
                .as_ref()
                .ok_or(HeapError::StaleReference)?
                .kind,
            CellKind::Array(_)
        );
        let properties = &self.object_data(index)?.properties;
        let total_count = properties
            .len()
            .checked_add(usize::from(is_array))
            .ok_or(HeapError::SizeOverflow)?;
        let capacity = limit.min(total_count);
        if capacity == 0 {
            return Ok((Vec::new(), total_count != 0));
        }

        let mut indices = BinaryHeap::<(u32, u32)>::new();
        indices
            .try_reserve(capacity.min(properties.len()))
            .map_err(|_| HeapError::AllocationFailed)?;
        let mut strings = Vec::new();
        strings
            .try_reserve_exact(capacity.min(properties.len()))
            .map_err(|_| HeapError::AllocationFailed)?;

        for property in properties {
            if let Some(array_index) = self.array_index_from_atom(property.key as usize) {
                if indices.len() < capacity {
                    indices.push((array_index, property.key));
                } else if indices
                    .peek()
                    .is_some_and(|(largest, _)| array_index < *largest)
                {
                    indices.pop();
                    indices.push((array_index, property.key));
                }
            } else if strings.len() < capacity {
                strings.push(property.key);
            }
        }

        let length_key = if is_array && capacity > indices.len() {
            Some(self.intern_property_key("length")?)
        } else {
            None
        };
        let mut result = Vec::new();
        result
            .try_reserve_exact(capacity)
            .map_err(|_| HeapError::AllocationFailed)?;
        for (_, key) in indices.into_sorted_vec() {
            result.push(self.property_key_for_index(key));
        }
        if result.len() < capacity
            && let Some(key) = length_key
        {
            result.push(key);
        }
        for key in strings.into_iter().take(capacity - result.len()) {
            result.push(self.property_key_for_index(key));
        }
        let truncated = total_count > result.len();
        Ok((result, truncated))
    }

    /// Creates an environment with mutable bindings backed by shared GC cells.
    pub fn create_environment(
        &mut self,
        parent: Option<LexicalEnvironment>,
        bindings: &[BindingDefinition],
    ) -> Result<LexicalEnvironment, HeapError> {
        for (index, binding) in bindings.iter().enumerate() {
            if bindings[..index]
                .iter()
                .any(|previous| previous.id == binding.id)
            {
                return Err(HeapError::InvalidFunctionDefinition);
            }
        }
        let scope = self.open_scope()?;
        let result = (|| {
            if let Some(parent) = parent {
                let index = self.validate_ref(parent.reference)?;
                if !matches!(
                    &self.slots[index]
                        .cell
                        .as_ref()
                        .ok_or(HeapError::StaleReference)?
                        .kind,
                    CellKind::Environment(_)
                ) {
                    return Err(HeapError::InvalidObjectKind);
                }
                self.scope_root_reference(scope, parent.reference)?;
            }
            for binding in bindings {
                if let Some(value) = binding.initial_value {
                    self.scope_root(scope, value)?;
                }
            }
            let reference = self.allocate_cell_with_retry(|| {
                CellKind::Environment(Box::new(EnvironmentData {
                    parent: parent.map(|environment| environment.reference),
                    bindings: Vec::new(),
                }))
            })?;
            self.scope_root_reference(scope, reference)?;
            let index = self.validate_ref(reference)?;
            self.reserve_environment_bindings(index, bindings.len())?;
            if let Some(parent) = parent {
                self.add_edge_ref(reference, parent.reference)?;
            }
            for binding in bindings {
                let storage = if binding.mutable {
                    let value = binding.initial_value.unwrap_or_else(Value::undefined);
                    let cell = self.allocate_cell_with_retry(|| {
                        CellKind::BindingCell(Box::new(BindingCellData {
                            value,
                            mutable: true,
                            initialized: binding.initial_value.is_some(),
                        }))
                    })?;
                    self.scope_root_reference(scope, cell)?;
                    let cell_index = self.validate_ref(cell)?;
                    self.add_value_edge(cell_index, value)?;
                    self.add_edge_ref(reference, cell)?;
                    BindingStorage::Cell(cell)
                } else {
                    let value = binding.initial_value.unwrap_or_else(Value::undefined);
                    self.add_value_edge(index, value)?;
                    BindingStorage::Value {
                        value,
                        initialized: binding.initial_value.is_some(),
                    }
                };
                self.environment_data_mut(index)?
                    .bindings
                    .push(EnvironmentBinding {
                        id: binding.id,
                        storage,
                    });
            }
            Ok(LexicalEnvironment {
                reference,
                _thread_affinity: PhantomData,
            })
        })();
        self.close_scope(scope)?;
        if result.is_err() {
            self.collect()?;
        }
        result
    }

    /// Loads a binding through parent environments, throwing on missing/TDZ state.
    pub fn load_binding(
        &self,
        environment: LexicalEnvironment,
        binding: BindingId,
    ) -> Result<Value, HeapError> {
        self.load_binding_recursive(environment.reference, binding, self.slots.len())
    }

    fn load_binding_recursive(
        &self,
        environment: GcRef,
        binding: BindingId,
        remaining_aliases: usize,
    ) -> Result<Value, HeapError> {
        match self.resolve_binding_storage(environment, binding)? {
            BindingStorage::Value { value, initialized } => {
                if initialized {
                    Ok(value)
                } else {
                    Err(HeapError::UninitializedBinding)
                }
            }
            BindingStorage::Cell(reference) => {
                let index = self.validate_ref(reference)?;
                match &self.slots[index]
                    .cell
                    .as_ref()
                    .ok_or(HeapError::StaleReference)?
                    .kind
                {
                    CellKind::BindingCell(cell) if cell.initialized => Ok(cell.value),
                    CellKind::BindingCell(_) => Err(HeapError::UninitializedBinding),
                    _ => Err(HeapError::AccountingInvariant),
                }
            }
            BindingStorage::Alias {
                environment,
                binding,
            } => {
                if remaining_aliases == 0 {
                    return Err(HeapError::BindingAliasCycle);
                }
                self.load_binding_recursive(environment, binding, remaining_aliases - 1)
            }
        }
    }

    /// Links an uninitialized immutable binding to an exporter binding.
    ///
    /// The alias is read-only even when the target is mutable. Reads resolve
    /// the target on every access, preserving ECMAScript live import semantics.
    /// The importing environment owns a traced edge to the exporter environment.
    pub fn link_binding_alias(
        &mut self,
        environment: LexicalEnvironment,
        binding: BindingId,
        target_environment: LexicalEnvironment,
        target_binding: BindingId,
    ) -> Result<(), HeapError> {
        let (owner, position) = self.find_environment_binding(environment.reference, binding)?;
        let owner_reference = self.environment_reference_at(owner)?;
        let source_storage = match &self.slots[owner]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Environment(data) => data.bindings[position].storage,
            _ => return Err(HeapError::InvalidObjectKind),
        };
        match source_storage {
            BindingStorage::Value {
                initialized: false, ..
            } => {}
            BindingStorage::Alias { .. } => return Err(HeapError::AlreadyInitialized),
            _ => return Err(HeapError::InvalidFunctionDefinition),
        }

        // Resolve the complete alias chain before publishing the new edge.
        // This catches both direct self-links and longer cycles.
        let mut current_environment = target_environment.reference;
        let mut current_binding = target_binding;
        let mut resolved_target = false;
        for _ in 0..=self.slots.len() {
            if current_environment == owner_reference && current_binding == binding {
                return Err(HeapError::BindingAliasCycle);
            }
            match self.resolve_binding_storage(current_environment, current_binding)? {
                BindingStorage::Alias {
                    environment,
                    binding,
                } => {
                    current_environment = environment;
                    current_binding = binding;
                }
                BindingStorage::Cell(_) | BindingStorage::Value { .. } => {
                    resolved_target = true;
                    break;
                }
            }
        }
        if !resolved_target {
            return Err(HeapError::BindingAliasCycle);
        }

        self.add_edge_ref(owner_reference, target_environment.reference)?;
        self.environment_data_mut(owner)?.bindings[position].storage = BindingStorage::Alias {
            environment: target_environment.reference,
            binding: target_binding,
        };
        Ok(())
    }

    fn environment_reference_at(&self, index: usize) -> Result<GcRef, HeapError> {
        let slot = self.slots.get(index).ok_or(HeapError::StaleReference)?;
        if !matches!(
            slot.cell.as_ref().ok_or(HeapError::StaleReference)?.kind,
            CellKind::Environment(_)
        ) {
            return Err(HeapError::InvalidObjectKind);
        }
        Ok(GcRef {
            heap_id: self.heap_id,
            index: u32::try_from(index).map_err(|_| HeapError::HeapCapacityExceeded)?,
            generation: slot.generation,
        })
    }

    /// Stores into an already initialized mutable binding; use initialize_binding for TDZ slots.
    pub fn store_binding(
        &mut self,
        environment: LexicalEnvironment,
        binding: BindingId,
        value: Value,
    ) -> Result<Value, HeapError> {
        self.validate_value(value)?;
        let BindingStorage::Cell(reference) =
            self.resolve_binding_storage(environment.reference, binding)?
        else {
            return Err(HeapError::ImmutableBinding);
        };
        let index = self.validate_ref(reference)?;
        let (old, mutable) = match &self.slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::BindingCell(cell) => (cell.value, cell.mutable),
            _ => return Err(HeapError::AccountingInvariant),
        };
        if !mutable {
            return Err(HeapError::ImmutableBinding);
        }
        let initialized = match &self.slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::BindingCell(cell) => cell.initialized,
            _ => return Err(HeapError::AccountingInvariant),
        };
        if !initialized {
            return Err(HeapError::UninitializedBinding);
        }
        self.replace_cell_value(reference, old, value)?;
        if let CellKind::BindingCell(cell) = &mut self.slots[index]
            .cell
            .as_mut()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            cell.value = value;
            cell.initialized = true;
        }
        Ok(value)
    }

    /// Initializes a lexical binding once; repeated initialization is rejected.
    pub fn initialize_binding(
        &mut self,
        environment: LexicalEnvironment,
        binding: BindingId,
        value: Value,
    ) -> Result<Value, HeapError> {
        self.validate_value(value)?;
        match self.resolve_binding_storage(environment.reference, binding)? {
            BindingStorage::Cell(reference) => {
                let index = self.validate_ref(reference)?;
                let (old, mutable, initialized) = match &self.slots[index]
                    .cell
                    .as_ref()
                    .ok_or(HeapError::StaleReference)?
                    .kind
                {
                    CellKind::BindingCell(cell) => (cell.value, cell.mutable, cell.initialized),
                    _ => return Err(HeapError::AccountingInvariant),
                };
                if initialized {
                    return Err(HeapError::AlreadyInitialized);
                }
                if !mutable {
                    return Err(HeapError::ImmutableBinding);
                }
                self.replace_cell_value(reference, old, value)?;
                if let CellKind::BindingCell(cell) = &mut self.slots[index]
                    .cell
                    .as_mut()
                    .ok_or(HeapError::StaleReference)?
                    .kind
                {
                    cell.value = value;
                    cell.initialized = true;
                }
            }
            BindingStorage::Value {
                initialized: true, ..
            } => return Err(HeapError::AlreadyInitialized),
            BindingStorage::Value {
                initialized: false, ..
            } => {
                let (index, position) =
                    self.find_environment_binding(environment.reference, binding)?;
                self.add_value_edge(index, value)?;
                if let BindingStorage::Value {
                    value: stored,
                    initialized,
                } = &mut self.environment_data_mut(index)?.bindings[position].storage
                {
                    *stored = value;
                    *initialized = true;
                }
            }
            BindingStorage::Alias { .. } => return Err(HeapError::ImmutableBinding),
        }
        Ok(value)
    }

    /// Creates a module Program closure bound directly to its persistent module environment.
    ///
    /// Unlike ordinary closures, a module entry must retain the complete
    /// instantiated environment (including top-level lexical slots and live
    /// import aliases), not a copied environment containing only captures.
    pub fn create_module_closure(
        &mut self,
        definition: &FunctionDefinition,
        environment: LexicalEnvironment,
    ) -> Result<Value, HeapError> {
        if !definition.parameters.is_empty() || !definition.captures.is_empty() {
            return Err(HeapError::InvalidFunctionDefinition);
        }
        let environment_index = self.validate_ref(environment.reference)?;
        if !matches!(
            &self.slots[environment_index]
                .cell
                .as_ref()
                .ok_or(HeapError::StaleReference)?
                .kind,
            CellKind::Environment(_)
        ) {
            return Err(HeapError::InvalidObjectKind);
        }
        let scope = self.open_scope()?;
        let result = (|| {
            self.scope_root_environment(scope, environment)?;
            let function = self.allocate_cell_with_retry(|| {
                CellKind::Function(Box::new(FunctionData::closure(
                    definition.id,
                    environment.reference,
                    definition.kind,
                    definition.strict,
                    Value::undefined(),
                )))
            })?;
            self.scope_root_reference(scope, function)?;
            self.add_edge_ref(function, environment.reference)?;
            Ok(Value::from_function(function))
        })();
        self.close_scope(scope)?;
        if result.is_err() {
            self.collect()?;
        }
        result
    }

    /// Creates a traced function object with explicit cell/value captures.
    ///
    /// `lexical_this` is captured only by arrow functions. Ordinary functions
    /// ignore this argument and derive their receiver from each call.
    pub fn create_closure(
        &mut self,
        definition: &FunctionDefinition,
        outer: LexicalEnvironment,
        lexical_this: Value,
    ) -> Result<Value, HeapError> {
        let stored_lexical_this = match definition.kind {
            FunctionKind::Arrow => {
                self.validate_value(lexical_this)?;
                lexical_this
            }
            FunctionKind::Ordinary => Value::undefined(),
        };
        let outer_index = self.validate_ref(outer.reference)?;
        if !matches!(
            &self.slots[outer_index]
                .cell
                .as_ref()
                .ok_or(HeapError::StaleReference)?
                .kind,
            CellKind::Environment(_)
        ) {
            return Err(HeapError::InvalidObjectKind);
        }
        let scope = self.open_scope()?;
        let result = (|| {
            self.scope_root_reference(scope, outer.reference)?;
            self.scope_root(scope, stored_lexical_this)?;
            let closure_ref = self.allocate_cell_with_retry(|| {
                CellKind::Environment(Box::new(EnvironmentData {
                    parent: None,
                    bindings: Vec::new(),
                }))
            })?;
            self.scope_root_reference(scope, closure_ref)?;
            let closure_index = self.validate_ref(closure_ref)?;
            self.reserve_environment_bindings(closure_index, definition.captures.len())?;

            for (position, capture) in definition.captures.iter().enumerate() {
                if definition.captures[..position].iter().any(|previous| {
                    previous.alias == capture.alias || previous.source == capture.source
                }) || definition
                    .parameters
                    .iter()
                    .any(|binding| binding.id == capture.alias)
                    || definition
                        .locals
                        .iter()
                        .any(|binding| binding.id == capture.alias)
                {
                    return Err(HeapError::InvalidFunctionDefinition);
                }
                let source = self.resolve_binding_storage(outer.reference, capture.source)?;
                let imported = match (capture.mode, source) {
                    (CaptureMode::Cell, BindingStorage::Cell(reference)) => {
                        let index = self.validate_ref(reference)?;
                        if !matches!(
                            &self.slots[index]
                                .cell
                                .as_ref()
                                .ok_or(HeapError::StaleReference)?
                                .kind,
                            CellKind::BindingCell(cell) if cell.mutable
                        ) {
                            return Err(HeapError::InvalidFunctionDefinition);
                        }
                        self.scope_root_reference(scope, reference)?;
                        self.add_edge_ref(closure_ref, reference)?;
                        BindingStorage::Cell(reference)
                    }
                    (
                        CaptureMode::Cell,
                        BindingStorage::Alias {
                            environment,
                            binding,
                        },
                    ) => {
                        self.scope_root_reference(scope, environment)?;
                        self.add_edge_ref(closure_ref, environment)?;
                        BindingStorage::Alias {
                            environment,
                            binding,
                        }
                    }
                    (
                        CaptureMode::Value,
                        BindingStorage::Value {
                            value,
                            initialized: true,
                        },
                    ) => {
                        self.scope_root(scope, value)?;
                        self.add_value_edge(closure_index, value)?;
                        BindingStorage::Value {
                            value,
                            initialized: true,
                        }
                    }
                    _ => return Err(HeapError::InvalidFunctionDefinition),
                };
                self.environment_data_mut(closure_index)?
                    .bindings
                    .push(EnvironmentBinding {
                        id: capture.alias,
                        storage: imported,
                    });
            }

            let function = self.allocate_cell_with_retry(|| {
                CellKind::Function(Box::new(FunctionData::closure(
                    definition.id,
                    closure_ref,
                    definition.kind,
                    definition.strict,
                    stored_lexical_this,
                )))
            })?;
            self.scope_root_reference(scope, function)?;
            self.add_edge_ref(function, closure_ref)?;
            if let Some(reference) = stored_lexical_this.gc_ref() {
                self.add_edge_ref(function, reference)?;
            }
            Ok(Value::from_function(function))
        })();
        self.close_scope(scope)?;
        if result.is_err() {
            self.collect()?;
        }
        result
    }

    /// Returns the backend-owned function-table identity stored in a closure.
    ///
    /// The interpreter uses this identity to select canonical function metadata
    /// before asking the Runtime to prepare an activation.
    pub fn function_id(&self, function: Value) -> Result<FunctionId, HeapError> {
        let index = self.function_index(function)?;
        match &self.slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Function(data) => data.id.ok_or(HeapError::NotCallable),
            _ => Err(HeapError::NotCallable),
        }
    }

    /// Returns whether a closure is bound to the supplied lexical environment.
    pub fn closure_matches_environment(
        &self,
        callee: Value,
        expected_function: FunctionId,
        environment: LexicalEnvironment,
    ) -> Result<bool, HeapError> {
        let index = self.function_index(callee)?;
        match &self.slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Function(function) => Ok(function.id == Some(expected_function)
                && function.closure_environment == Some(environment.reference)),
            _ => Err(HeapError::NotCallable),
        }
    }

    /// Prepares a call, binds arguments and roots the complete activation.
    ///
    /// Execution itself belongs to the interpreter/native dispatcher. For
    /// sloppy ordinary calls, nullish this is replaced with global_this;
    /// primitive boxing fails closed until wrappers are part of the profile.
    pub fn enter_function_call(
        &mut self,
        callee: Value,
        definition: &FunctionDefinition,
        this_argument: Value,
        arguments: &[Value],
        global_this: Value,
    ) -> Result<CallActivation, HeapError> {
        let function_index = self.function_index(callee)?;
        let function = match &self.slots[function_index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Function(function) => function,
            _ => return Err(HeapError::NotCallable),
        };
        let function_id = function.id.ok_or(HeapError::NotCallable)?;
        if function_id != definition.id {
            return Err(HeapError::InvalidFunctionDefinition);
        }
        let closure_environment = function
            .closure_environment
            .ok_or(HeapError::InvalidFunctionDefinition)?;
        let (kind, strict, lexical_this) = (function.kind, function.strict, function.lexical_this);
        let this_value = match kind {
            FunctionKind::Arrow => lexical_this,
            FunctionKind::Ordinary if strict => this_argument,
            FunctionKind::Ordinary if this_argument.is_null() || this_argument.is_undefined() => {
                global_this
            }
            FunctionKind::Ordinary
                if matches!(
                    this_argument.value_type(),
                    ValueType::Object | ValueType::Function
                ) =>
            {
                this_argument
            }
            FunctionKind::Ordinary => return Err(HeapError::UnsupportedCoercion),
        };
        self.validate_value(this_value)?;

        let frame = self.enter_frame()?;
        let result = (|| {
            self.frame_root(frame, callee)?;
            // Only the effective receiver is observable inside this activation.
            // A call-site receiver ignored by an arrow (or globalThis ignored
            // by a strict/non-nullish call) must not become an artificial root.
            self.frame_root(frame, this_value)?;
            for argument in arguments {
                self.frame_root(frame, *argument)?;
            }

            if definition
                .parameters
                .iter()
                .any(|parameter| !parameter.mutable)
            {
                return Err(HeapError::InvalidFunctionDefinition);
            }
            let binding_count = definition
                .parameters
                .len()
                .checked_add(definition.locals.len())
                .ok_or(HeapError::SizeOverflow)?;
            let mut activation_bindings = Vec::new();
            reserve_vec(&mut self.budget, &mut activation_bindings, binding_count)?;
            for (index, parameter) in definition.parameters.iter().enumerate() {
                activation_bindings.push(BindingDefinition {
                    id: parameter.id,
                    mutable: true,
                    initial_value: Some(
                        arguments
                            .get(index)
                            .copied()
                            .unwrap_or_else(Value::undefined),
                    ),
                });
            }
            activation_bindings.extend(definition.locals.iter().copied());
            let environment_result = self.create_environment(
                Some(LexicalEnvironment {
                    reference: closure_environment,
                    _thread_affinity: PhantomData,
                }),
                &activation_bindings,
            );
            release_vec_capacity(&mut self.budget, &activation_bindings)?;
            let environment = environment_result?;
            self.frame_root_reference(frame, environment.reference)?;
            Ok(CallActivation {
                frame,
                environment,
                function_id,
                this_value,
            })
        })();
        if result.is_err() {
            self.leave_frame(frame)?;
        }
        result
    }

    /// Enters a module Program using its pre-instantiated module lexical environment.
    ///
    /// Unlike an ordinary function call, module evaluation must not create a
    /// fresh child environment: imported bindings and top-level lexical slots
    /// are instantiated and linked before dependency evaluation begins.
    pub fn enter_function_call_in_environment(
        &mut self,
        callee: Value,
        definition: &FunctionDefinition,
        environment: LexicalEnvironment,
        this_argument: Value,
        global_this: Value,
    ) -> Result<CallActivation, HeapError> {
        if !definition.parameters.is_empty() {
            return Err(HeapError::InvalidFunctionDefinition);
        }
        let function_index = self.function_index(callee)?;
        let function = match &self.slots[function_index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Function(function) => function,
            _ => return Err(HeapError::NotCallable),
        };
        if function.id != Some(definition.id)
            || function.closure_environment != Some(environment.reference)
        {
            return Err(HeapError::InvalidFunctionDefinition);
        }
        let this_value = match function.kind {
            FunctionKind::Arrow => function.lexical_this,
            FunctionKind::Ordinary if function.strict => this_argument,
            FunctionKind::Ordinary if this_argument.is_null() || this_argument.is_undefined() => {
                global_this
            }
            FunctionKind::Ordinary
                if matches!(
                    this_argument.value_type(),
                    ValueType::Object | ValueType::Function
                ) =>
            {
                this_argument
            }
            FunctionKind::Ordinary => return Err(HeapError::UnsupportedCoercion),
        };
        self.validate_value(this_value)?;
        let frame = self.enter_frame()?;
        let result = (|| {
            self.frame_root(frame, callee)?;
            self.frame_root(frame, this_value)?;
            self.frame_root_environment(frame, environment)?;
            Ok(CallActivation {
                frame,
                environment,
                function_id: definition.id,
                this_value,
            })
        })();
        if result.is_err() {
            self.leave_frame(frame)?;
        }
        result
    }

    /// Ends a normal or abrupt function activation, releasing its roots.
    pub fn leave_function_call(&mut self, activation: CallActivation) -> Result<(), HeapError> {
        self.leave_frame(activation.frame)
    }

    /// Adds an environment to a temporary root scope.
    pub fn scope_root_environment(
        &mut self,
        scope: super::ScopeToken,
        environment: LexicalEnvironment,
    ) -> Result<(), HeapError> {
        self.scope_root_reference(scope, environment.reference)
    }

    /// Adds an environment to an interpreter frame.
    pub fn frame_root_environment(
        &mut self,
        frame: super::FrameToken,
        environment: LexicalEnvironment,
    ) -> Result<(), HeapError> {
        self.frame_root_reference(frame, environment.reference)
    }

    fn intern_string_property_key(&mut self, reference: GcRef) -> Result<PropertyKey, HeapError> {
        let index = self.validate_ref(reference)?;
        let Self {
            budget,
            slots,
            atoms,
            heap_id,
            ..
        } = self;
        let units = match &slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::String(units) => units,
            _ => return Err(HeapError::InvalidObjectKind),
        };
        if let Some(index) = atoms.iter().position(|entry| entry.as_slice() == units) {
            return Ok(PropertyKey {
                heap_id: *heap_id,
                index: u32::try_from(index).map_err(|_| HeapError::HeapCapacityExceeded)?,
                _thread_affinity: PhantomData,
            });
        }
        if atoms.len() >= u32::MAX as usize {
            return Err(HeapError::HeapCapacityExceeded);
        }
        let mut atom = Vec::new();
        reserve_vec(budget, &mut atom, units.len())?;
        atom.extend_from_slice(units);
        if let Err(error) = reserve_vec(budget, atoms, 1) {
            release_vec_capacity(budget, &atom)?;
            return Err(error);
        }
        let atom_index = u32::try_from(atoms.len()).map_err(|_| HeapError::HeapCapacityExceeded)?;
        atoms.push(atom);
        Ok(PropertyKey {
            heap_id: *heap_id,
            index: atom_index,
            _thread_affinity: PhantomData,
        })
    }

    fn validate_property_key(&self, key: PropertyKey) -> Result<usize, HeapError> {
        if key.heap_id != self.heap_id {
            return Err(HeapError::WrongHeap);
        }
        let index = key.index as usize;
        if index >= self.atoms.len() {
            return Err(HeapError::InvalidPropertyKey);
        }
        Ok(index)
    }

    fn property_key_for_index(&self, index: u32) -> PropertyKey {
        PropertyKey {
            heap_id: self.heap_id,
            index,
            _thread_affinity: PhantomData,
        }
    }

    fn is_array_length_key(&self, index: usize) -> bool {
        self.atoms
            .get(index)
            .is_some_and(|units| units.as_slice() == [108, 101, 110, 103, 116, 104])
    }

    fn array_index_from_atom(&self, index: usize) -> Option<u32> {
        let units = self.atoms.get(index)?;
        if units.is_empty() || units.len() > 10 || (units.len() > 1 && units[0] == 48) {
            return None;
        }
        let mut value = 0_u64;
        for unit in units {
            if !(48..=57).contains(unit) {
                return None;
            }
            value = value.checked_mul(10)?.checked_add(u64::from(*unit - 48))?;
            if value > u64::from(u32::MAX - 1) {
                return None;
            }
        }
        Some(value as u32)
    }

    fn object_reference(&self, value: Value) -> Result<GcRef, HeapError> {
        let reference = match value.repr() {
            ValueRepr::Object(reference) | ValueRepr::Function(reference) => reference,
            _ => return Err(HeapError::NotObject),
        };
        self.object_index(value)?;
        Ok(reference)
    }

    fn object_index(&self, value: Value) -> Result<usize, HeapError> {
        let reference = match value.repr() {
            ValueRepr::Object(reference) | ValueRepr::Function(reference) => reference,
            _ => return Err(HeapError::NotObject),
        };
        let index = self.validate_ref(reference)?;
        match &self.slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Object(_) | CellKind::Array(_) | CellKind::Function(_) => Ok(index),
            _ => Err(HeapError::NotObject),
        }
    }

    fn array_index(&self, value: Value) -> Result<usize, HeapError> {
        let index = self.object_index(value)?;
        if matches!(
            &self.slots[index]
                .cell
                .as_ref()
                .ok_or(HeapError::StaleReference)?
                .kind,
            CellKind::Array(_)
        ) {
            Ok(index)
        } else {
            Err(HeapError::InvalidObjectKind)
        }
    }

    fn function_index(&self, value: Value) -> Result<usize, HeapError> {
        let reference = match value.repr() {
            ValueRepr::Function(reference) => reference,
            _ => return Err(HeapError::NotCallable),
        };
        let index = self.validate_ref(reference)?;
        if matches!(
            &self.slots[index]
                .cell
                .as_ref()
                .ok_or(HeapError::StaleReference)?
                .kind,
            CellKind::Function(_)
        ) {
            Ok(index)
        } else {
            Err(HeapError::NotCallable)
        }
    }

    fn object_data(&self, index: usize) -> Result<&ObjectData, HeapError> {
        match &self.slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Object(data) => Ok(data),
            CellKind::Array(data) => Ok(&data.object),
            CellKind::Function(data) => Ok(&data.object),
            _ => Err(HeapError::NotObject),
        }
    }

    fn object_data_mut(&mut self, index: usize) -> Result<&mut ObjectData, HeapError> {
        match &mut self.slots[index]
            .cell
            .as_mut()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Object(data) => Ok(data),
            CellKind::Array(data) => Ok(&mut data.object),
            CellKind::Function(data) => Ok(&mut data.object),
            _ => Err(HeapError::NotObject),
        }
    }

    fn reserve_object_properties(
        &mut self,
        index: usize,
        additional: usize,
    ) -> Result<(), HeapError> {
        let Self { budget, slots, .. } = self;
        let cell = slots
            .get_mut(index)
            .and_then(|slot| slot.cell.as_mut())
            .ok_or(HeapError::StaleReference)?;
        let object = match &mut cell.kind {
            CellKind::Object(data) => data,
            CellKind::Array(data) => &mut data.object,
            CellKind::Function(data) => &mut data.object,
            _ => return Err(HeapError::NotObject),
        };
        reserve_vec(budget, &mut object.properties, additional)
    }

    fn reserve_environment_bindings(
        &mut self,
        index: usize,
        additional: usize,
    ) -> Result<(), HeapError> {
        let Self { budget, slots, .. } = self;
        let cell = slots
            .get_mut(index)
            .and_then(|slot| slot.cell.as_mut())
            .ok_or(HeapError::StaleReference)?;
        let environment = match &mut cell.kind {
            CellKind::Environment(data) => data,
            _ => return Err(HeapError::InvalidObjectKind),
        };
        reserve_vec(budget, &mut environment.bindings, additional)
    }

    fn environment_data_mut(&mut self, index: usize) -> Result<&mut EnvironmentData, HeapError> {
        match &mut self.slots[index]
            .cell
            .as_mut()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Environment(data) => Ok(data),
            _ => Err(HeapError::InvalidObjectKind),
        }
    }

    fn find_own_property(&self, index: usize, key: usize) -> Result<Option<usize>, HeapError> {
        Ok(self
            .object_data(index)?
            .properties
            .iter()
            .position(|property| property.key as usize == key))
    }

    fn allocate_promise_record(&mut self, state: super::PromiseState) -> Result<Value, HeapError> {
        self.promises
            .try_reserve(1)
            .map_err(|_| HeapError::AllocationFailed)?;
        let value = self.allocate_object()?;
        let reference = value.gc_ref().ok_or(HeapError::AccountingInvariant)?;
        let accounting = self.charge_external_memory(std::mem::size_of::<super::PromiseRecord>())?;
        if self
            .promises
            .insert(
                reference,
                super::PromiseRecord {
                    state,
                    reactions: Vec::new(),
                    accounting,
                },
            )
            .is_some()
        {
            let _ = self.release_external_memory(accounting);
            return Err(HeapError::AccountingInvariant);
        }
        Ok(value)
    }

    /// Allocates an already fulfilled Promise for an ordinary JavaScript value.
    pub fn promise_resolve(&mut self, value: Value) -> Result<Value, HeapError> {
        if let Some(reference) = value.gc_ref()
            && self.promises.contains_key(&reference)
        {
            return Ok(value);
        }
        let promise = self.allocate_promise_record(super::PromiseState::Fulfilled(value))?;
        let index = self.object_index(promise)?;
        self.add_value_edge(index, value)?;
        Ok(promise)
    }

    /// Allocates an already rejected Promise.
    pub fn promise_reject(&mut self, reason: Value) -> Result<Value, HeapError> {
        let promise = self.allocate_promise_record(super::PromiseState::Rejected(reason))?;
        let index = self.object_index(promise)?;
        self.add_value_edge(index, reason)?;
        Ok(promise)
    }

    /// Returns true when a value is a Runtime-managed Promise object.
    #[must_use]
    pub fn is_promise(&self, value: Value) -> bool {
        value
            .gc_ref()
            .is_some_and(|reference| self.promises.contains_key(&reference))
    }

    /// Returns the current state of a Promise.
    pub fn promise_state(&self, promise: Value) -> Result<super::PromiseState, HeapError> {
        let reference = promise.gc_ref().ok_or(HeapError::NotObject)?;
        self.promises
            .get(&reference)
            .map(|record| record.state)
            .ok_or(HeapError::NotObject)
    }

    /// Creates a child Promise and attaches fulfillment/rejection reactions.
    ///
    /// If the source is already settled, the returned reaction must be queued
    /// by the embedding Host. Pending sources retain the reaction until settlement.
    pub fn promise_then(
        &mut self,
        promise: Value,
        on_fulfilled: Option<Value>,
        on_rejected: Option<Value>,
    ) -> Result<(Value, Option<super::PromiseReaction>), HeapError> {
        let source = promise.gc_ref().ok_or(HeapError::NotObject)?;
        let state = self
            .promises
            .get(&source)
            .map(|record| record.state)
            .ok_or(HeapError::NotObject)?;
        let child = self.allocate_promise_record(super::PromiseState::Pending)?;

        match state {
            super::PromiseState::Pending => {
                let child_index = self.object_index(child)?;
                self.add_value_edge(child_index, promise)?;
                let source_index = self.validate_ref(source)?;
                if let Some(callback) = on_fulfilled {
                    self.add_value_edge(source_index, callback)?;
                }
                if let Some(callback) = on_rejected {
                    self.add_value_edge(source_index, callback)?;
                }
                self.add_value_edge(source_index, child)?;

                let accounting =
                    self.charge_external_memory(std::mem::size_of::<super::PromiseReaction>())?;
                let reserve_failed = self
                    .promises
                    .get_mut(&source)
                    .ok_or(HeapError::NotObject)?
                    .reactions
                    .try_reserve(1)
                    .is_err();
                if reserve_failed {
                    self.release_external_memory(accounting)?;
                    return Err(HeapError::AllocationFailed);
                }
                self.promises
                    .get_mut(&source)
                    .ok_or(HeapError::NotObject)?
                    .reactions
                    .push(super::ChargedPromiseReaction {
                        reaction: super::PromiseReaction {
                            on_fulfilled,
                            on_rejected,
                            argument: Value::undefined(),
                            child,
                            rejected: false,
                        },
                        accounting,
                    });
                Ok((child, None))
            }
            super::PromiseState::Fulfilled(argument) => Ok((
                child,
                Some(super::PromiseReaction {
                    on_fulfilled,
                    on_rejected,
                    argument,
                    child,
                    rejected: false,
                }),
            )),
            super::PromiseState::Rejected(argument) => Ok((
                child,
                Some(super::PromiseReaction {
                    on_fulfilled,
                    on_rejected,
                    argument,
                    child,
                    rejected: true,
                }),
            )),
        }
    }

    /// Adopts another Promise's eventual state into a pending target Promise.
    ///
    /// The source retains the target through a reaction record, so the target
    /// remains reachable until settlement. If the source is already settled,
    /// a reaction is returned for the host to enqueue as a microtask.
    pub fn promise_forward_to(
        &mut self,
        source: Value,
        target: Value,
    ) -> Result<Vec<super::PromiseReaction>, HeapError> {
        let source_ref = source.gc_ref().ok_or(HeapError::NotObject)?;
        let target_ref = target.gc_ref().ok_or(HeapError::NotObject)?;
        if source_ref == target_ref {
            return Err(HeapError::InvalidObjectKind);
        }
        let source_state = self
            .promises
            .get(&source_ref)
            .map(|record| record.state)
            .ok_or(HeapError::NotObject)?;
        let target_state = self
            .promises
            .get(&target_ref)
            .map(|record| record.state)
            .ok_or(HeapError::NotObject)?;
        if !matches!(target_state, super::PromiseState::Pending) {
            return Ok(Vec::new());
        }
        match source_state {
            super::PromiseState::Pending => {
                let source_index = self.validate_ref(source_ref)?;
                self.add_value_edge(source_index, target)?;
                let target_index = self.validate_ref(target_ref)?;
                self.add_value_edge(target_index, source)?;

                let accounting =
                    self.charge_external_memory(std::mem::size_of::<super::PromiseReaction>())?;
                let reserve_failed = self
                    .promises
                    .get_mut(&source_ref)
                    .ok_or(HeapError::NotObject)?
                    .reactions
                    .try_reserve(1)
                    .is_err();
                if reserve_failed {
                    self.release_external_memory(accounting)?;
                    return Err(HeapError::AllocationFailed);
                }
                self.promises
                    .get_mut(&source_ref)
                    .ok_or(HeapError::NotObject)?
                    .reactions
                    .push(super::ChargedPromiseReaction {
                        reaction: super::PromiseReaction {
                            on_fulfilled: None,
                            on_rejected: None,
                            argument: Value::undefined(),
                            child: target,
                            rejected: false,
                        },
                        accounting,
                    });
                Ok(Vec::new())
            }
            super::PromiseState::Fulfilled(argument) => Ok(vec![super::PromiseReaction {
                on_fulfilled: None,
                on_rejected: None,
                argument,
                child: target,
                rejected: false,
            }]),
            super::PromiseState::Rejected(argument) => Ok(vec![super::PromiseReaction {
                on_fulfilled: None,
                on_rejected: None,
                argument,
                child: target,
                rejected: true,
            }]),
        }
    }

    /// Settles a pending Promise and returns reactions in registration order.
    ///
    /// Repeated settlement attempts are ignored. The caller must enqueue each
    /// returned reaction as a PromiseJob before another GC safepoint.
    pub fn settle_promise(
        &mut self,
        promise: Value,
        state: super::PromiseState,
    ) -> Result<Vec<super::PromiseReaction>, HeapError> {
        if matches!(state, super::PromiseState::Pending) {
            return Err(HeapError::InvalidObjectKind);
        }
        let reference = promise.gc_ref().ok_or(HeapError::NotObject)?;
        let current = self
            .promises
            .get(&reference)
            .map(|record| record.state)
            .ok_or(HeapError::NotObject)?;
        if !matches!(current, super::PromiseState::Pending) {
            return Ok(Vec::new());
        }
        let value = match state {
            super::PromiseState::Fulfilled(value) | super::PromiseState::Rejected(value) => value,
            super::PromiseState::Pending => unreachable!("checked above"),
        };
        let index = self.validate_ref(reference)?;
        self.add_value_edge(index, value)?;
        let (reactions, retired, charges) = {
            let record = self
                .promises
                .get_mut(&reference)
                .ok_or(HeapError::NotObject)?;
            record.state = state;
            let charged = std::mem::take(&mut record.reactions);
            let mut reactions = Vec::with_capacity(charged.len());
            let mut retired = Vec::with_capacity(charged.len());
            let mut charges = Vec::with_capacity(charged.len());
            for item in charged {
                reactions.push(super::PromiseReaction {
                    argument: value,
                    rejected: matches!(state, super::PromiseState::Rejected(_)),
                    ..item.reaction
                });
                retired.push(item.reaction);
                charges.push(item.accounting);
            }
            (reactions, retired, charges)
        };
        for reaction in retired {
            self.remove_value_edge(index, reaction.child)?;
            let child_index = self.object_index(reaction.child)?;
            self.remove_value_edge(child_index, promise)?;
            if let Some(callback) = reaction.on_fulfilled {
                self.remove_value_edge(index, callback)?;
            }
            if let Some(callback) = reaction.on_rejected {
                self.remove_value_edge(index, callback)?;
            }
        }
        for charge in charges {
            self.release_external_memory(charge)?;
        }
        Ok(reactions)
    }

    fn add_value_edge(&mut self, parent_index: usize, value: Value) -> Result<(), HeapError> {
        if let Some(reference) = value.gc_ref() {
            let parent = GcRef {
                heap_id: self.heap_id,
                index: u32::try_from(parent_index).map_err(|_| HeapError::HeapCapacityExceeded)?,
                generation: self.slots[parent_index].generation,
            };
            self.add_edge_ref(parent, reference)?;
        }
        Ok(())
    }

    fn remove_value_edge(&mut self, parent_index: usize, value: Value) -> Result<(), HeapError> {
        if let Some(reference) = value.gc_ref() {
            let parent = GcRef {
                heap_id: self.heap_id,
                index: u32::try_from(parent_index).map_err(|_| HeapError::HeapCapacityExceeded)?,
                generation: self.slots[parent_index].generation,
            };
            self.remove_edge_ref(parent, reference)?;
        }
        Ok(())
    }

    fn replace_property_value(
        &mut self,
        index: usize,
        old: Value,
        new: Value,
    ) -> Result<(), HeapError> {
        if old.gc_ref() == new.gc_ref() {
            return Ok(());
        }
        self.add_value_edge(index, new)?;
        self.remove_value_edge(index, old)
    }

    fn define_assignment_property(
        &mut self,
        receiver: usize,
        key: usize,
        value: Value,
    ) -> Result<bool, HeapError> {
        if self.is_array_length_key(key)
            && matches!(
                &self.slots[receiver]
                    .cell
                    .as_ref()
                    .ok_or(HeapError::StaleReference)?
                    .kind,
                CellKind::Array(_)
            )
        {
            return self.define_array_length(receiver, PropertyDescriptor::value(value));
        }
        if let Some(position) = self.find_own_property(receiver, key)? {
            let current = self.object_data(receiver)?.properties[position].descriptor;
            if !current.writable {
                return Ok(false);
            }
            self.replace_property_value(receiver, current.value, value)?;
            self.object_data_mut(receiver)?.properties[position]
                .descriptor
                .value = value;
            return Ok(true);
        }
        if !self.object_data(receiver)?.extensible {
            return Ok(false);
        }
        if let Some(array_index) = self.array_index_from_atom(key)
            && let CellKind::Array(array) = &self.slots[receiver]
                .cell
                .as_ref()
                .ok_or(HeapError::StaleReference)?
                .kind
            && array_index >= array.length
            && !array.length_writable
        {
            return Ok(false);
        }
        self.reserve_object_properties(receiver, 1)?;
        self.add_value_edge(receiver, value)?;
        self.object_data_mut(receiver)?
            .properties
            .push(OwnProperty {
                key: key as u32,
                descriptor: PropertyDescriptorData {
                    value,
                    writable: true,
                    enumerable: true,
                    configurable: true,
                },
            });
        if let Some(array_index) = self.array_index_from_atom(key)
            && let CellKind::Array(array) = &mut self.slots[receiver]
                .cell
                .as_mut()
                .ok_or(HeapError::StaleReference)?
                .kind
        {
            array.length = array.length.max(array_index + 1);
        }
        Ok(true)
    }

    fn define_array_length(
        &mut self,
        index: usize,
        descriptor: PropertyDescriptor,
    ) -> Result<bool, HeapError> {
        let (old_length, old_writable) = match &self.slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Array(array) => (array.length, array.length_writable),
            _ => return Err(HeapError::InvalidObjectKind),
        };
        if descriptor.configurable == Some(true)
            || descriptor.enumerable == Some(true)
            || (!old_writable && descriptor.writable == Some(true))
        {
            return Ok(false);
        }
        let new_length = if let Some(value) = descriptor.value {
            let number = self.to_number(value)?;
            if !number.is_finite()
                || number < 0.0
                || number.fract() != 0.0
                || number > f64::from(u32::MAX)
            {
                return Err(HeapError::InvalidArrayLength);
            }
            Some(number as u32)
        } else {
            None
        };
        if let Some(new_length) = new_length {
            if !old_writable && new_length != old_length {
                return Ok(false);
            }
            if new_length < old_length {
                loop {
                    let candidate = self
                        .object_data(index)?
                        .properties
                        .iter()
                        .enumerate()
                        .filter_map(|(position, property)| {
                            self.array_index_from_atom(property.key as usize)
                                .filter(|array_index| *array_index >= new_length)
                                .map(|array_index| (position, array_index, property.descriptor))
                        })
                        .max_by_key(|entry| entry.1);
                    let Some((position, array_index, property)) = candidate else {
                        break;
                    };
                    if !property.configurable {
                        if let CellKind::Array(array) = &mut self.slots[index]
                            .cell
                            .as_mut()
                            .ok_or(HeapError::StaleReference)?
                            .kind
                        {
                            array.length = array_index + 1;
                            if descriptor.writable == Some(false) {
                                array.length_writable = false;
                            }
                        }
                        return Ok(false);
                    }
                    self.object_data_mut(index)?.properties.remove(position);
                    self.remove_value_edge(index, property.value)?;
                }
            }
            if let CellKind::Array(array) = &mut self.slots[index]
                .cell
                .as_mut()
                .ok_or(HeapError::StaleReference)?
                .kind
            {
                array.length = new_length;
            }
        }
        if descriptor.writable == Some(false)
            && let CellKind::Array(array) = &mut self.slots[index]
                .cell
                .as_mut()
                .ok_or(HeapError::StaleReference)?
                .kind
        {
            array.length_writable = false;
        }
        Ok(true)
    }

    fn resolve_binding_storage(
        &self,
        mut environment: GcRef,
        binding: BindingId,
    ) -> Result<BindingStorage, HeapError> {
        for _ in 0..=self.slots.len() {
            let index = self.validate_ref(environment)?;
            let data = match &self.slots[index]
                .cell
                .as_ref()
                .ok_or(HeapError::StaleReference)?
                .kind
            {
                CellKind::Environment(data) => data,
                _ => return Err(HeapError::InvalidObjectKind),
            };
            if let Some(found) = data.bindings.iter().find(|entry| entry.id == binding) {
                return Ok(found.storage);
            }
            let Some(parent) = data.parent else {
                return Err(HeapError::MissingBinding);
            };
            environment = parent;
        }
        Err(HeapError::PrototypeCycle)
    }

    fn find_environment_binding(
        &self,
        mut environment: GcRef,
        binding: BindingId,
    ) -> Result<(usize, usize), HeapError> {
        for _ in 0..=self.slots.len() {
            let index = self.validate_ref(environment)?;
            let data = match &self.slots[index]
                .cell
                .as_ref()
                .ok_or(HeapError::StaleReference)?
                .kind
            {
                CellKind::Environment(data) => data,
                _ => return Err(HeapError::InvalidObjectKind),
            };
            if let Some(position) = data.bindings.iter().position(|entry| entry.id == binding) {
                return Ok((index, position));
            }
            let Some(parent) = data.parent else {
                return Err(HeapError::MissingBinding);
            };
            environment = parent;
        }
        Err(HeapError::PrototypeCycle)
    }

    fn allocate_cell_with_retry<F>(&mut self, make: F) -> Result<GcRef, HeapError>
    where
        F: Fn() -> CellKind,
    {
        match self.try_allocate_cell(make()) {
            Err(HeapError::QuotaExceeded { .. }) => {
                self.collect()?;
                self.try_allocate_cell(make())
            }
            result => result,
        }
    }

    fn scope_root_reference(
        &mut self,
        scope: super::ScopeToken,
        reference: GcRef,
    ) -> Result<(), HeapError> {
        self.check_scope(scope)?;
        self.validate_ref(reference)?;
        let index = self
            .scopes
            .len()
            .checked_sub(1)
            .ok_or(HeapError::InvalidScope)?;
        reserve_vec(&mut self.budget, &mut self.scopes[index].roots, 1)?;
        self.scopes[index].roots.push(reference);
        Ok(())
    }

    fn frame_root_reference(
        &mut self,
        frame: super::FrameToken,
        reference: GcRef,
    ) -> Result<(), HeapError> {
        self.check_frame(frame)?;
        self.validate_ref(reference)?;
        let index = self
            .frames
            .len()
            .checked_sub(1)
            .ok_or(HeapError::InvalidFrame)?;
        reserve_vec(&mut self.budget, &mut self.frames[index].roots, 1)?;
        self.frames[index].roots.push(reference);
        Ok(())
    }

    fn add_edge_ref(&mut self, parent: GcRef, child: GcRef) -> Result<(), HeapError> {
        let parent_index = self.validate_ref(parent)?;
        self.validate_ref(child)?;
        let cell = self.slots[parent_index]
            .cell
            .as_mut()
            .ok_or(HeapError::StaleReference)?;
        reserve_vec(&mut self.budget, &mut cell.edges, 1)?;
        cell.edges.push(child);
        Ok(())
    }

    fn remove_edge_ref(&mut self, parent: GcRef, child: GcRef) -> Result<(), HeapError> {
        let index = self.validate_ref(parent)?;
        let cell = self.slots[index]
            .cell
            .as_mut()
            .ok_or(HeapError::StaleReference)?;
        let position = cell
            .edges
            .iter()
            .position(|candidate| *candidate == child)
            .ok_or(HeapError::AccountingInvariant)?;
        cell.edges.remove(position);
        Ok(())
    }

    fn replace_cell_value(&mut self, cell: GcRef, old: Value, new: Value) -> Result<(), HeapError> {
        if old.gc_ref() == new.gc_ref() {
            return Ok(());
        }
        if let Some(reference) = new.gc_ref() {
            self.add_edge_ref(cell, reference)?;
        }
        if let Some(reference) = old.gc_ref() {
            self.remove_edge_ref(cell, reference)?;
        }
        Ok(())
    }

    fn value_for_object_ref(&self, reference: GcRef) -> Result<Value, HeapError> {
        let index = self.validate_ref(reference)?;
        match &self.slots[index]
            .cell
            .as_ref()
            .ok_or(HeapError::StaleReference)?
            .kind
        {
            CellKind::Function(_) => Ok(Value::from_function(reference)),
            CellKind::Object(_) | CellKind::Array(_) => Ok(Value::from_object(reference)),
            _ => Err(HeapError::NotObject),
        }
    }
}

fn number_to_property_string(number: f64) -> String {
    if number.is_nan() {
        return "NaN".to_owned();
    }
    if number == f64::INFINITY {
        return "Infinity".to_owned();
    }
    if number == f64::NEG_INFINITY {
        return "-Infinity".to_owned();
    }
    if number == 0.0 {
        return "0".to_owned();
    }
    let absolute = number.abs();
    if !(1e-6..1e21).contains(&absolute) {
        let scientific = format!("{number:e}");
        if let Some((mantissa, exponent)) = scientific.split_once('e') {
            let exp = exponent.parse::<i32>().unwrap_or(0);
            return format!("{mantissa}e{}{}", if exp >= 0 { "+" } else { "" }, exp);
        }
        scientific
    } else {
        number.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heap() -> Heap {
        Heap::new(super::super::HeapConfig::new(1024 * 1024, 256 * 1024)).unwrap()
    }

    fn key(heap: &mut Heap, text: &str) -> PropertyKey {
        heap.intern_property_key(text).unwrap()
    }

    #[test]
    fn ordinary_properties_prototypes_descriptors_and_receiver_are_shared() {
        let mut heap = heap();
        let prototype = heap.allocate_object().unwrap();
        let object = heap.allocate_object().unwrap();
        let receiver = heap.allocate_object().unwrap();
        let roots = heap.open_scope().unwrap();
        for value in [prototype, object, receiver] {
            heap.scope_root(roots, value).unwrap();
        }
        let x = key(&mut heap, "x");
        assert!(
            heap.define_own_property(
                prototype,
                x,
                PropertyDescriptor::data(Value::number(1.0), true, true, false)
            )
            .unwrap()
        );
        assert!(heap.set_prototype_of(object, Some(prototype)).unwrap());
        assert_eq!(heap.get_property(object, x).unwrap().as_number(), Some(1.0));
        assert!(
            heap.set_property_with_receiver(object, x, Value::number(2.0), receiver)
                .unwrap()
        );
        assert_eq!(
            heap.get_property(prototype, x).unwrap().as_number(),
            Some(1.0)
        );
        assert_eq!(
            heap.get_property(receiver, x).unwrap().as_number(),
            Some(2.0)
        );
        assert!(!heap.delete_property(prototype, x).unwrap());
        assert!(!heap.set_prototype_of(prototype, Some(object)).unwrap());
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn non_configurable_and_non_extensible_properties_reject_mutation() {
        let mut heap = heap();
        let object = heap.allocate_object().unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, object).unwrap();
        let fixed = key(&mut heap, "fixed");
        assert!(
            heap.define_own_property(
                object,
                fixed,
                PropertyDescriptor::data(Value::number(3.0), false, false, false)
            )
            .unwrap()
        );
        assert!(
            !heap
                .define_own_property(object, fixed, PropertyDescriptor::value(Value::number(4.0)))
                .unwrap()
        );
        assert!(
            !heap
                .set_property(object, fixed, Value::number(4.0))
                .unwrap()
        );
        assert!(!heap.delete_property(object, fixed).unwrap());
        heap.prevent_extensions(object).unwrap();
        let other = key(&mut heap, "other");
        assert!(
            !heap
                .define_own_property(object, other, PropertyDescriptor::value(Value::number(1.0)))
                .unwrap()
        );
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn property_key_conversion_uses_ecmascript_number_format_boundaries() {
        let mut heap = heap();
        let cases = [
            (Value::number(-0.0), "0"),
            (Value::number(f64::NAN), "NaN"),
            (Value::number(f64::INFINITY), "Infinity"),
            (Value::number(f64::NEG_INFINITY), "-Infinity"),
            (Value::number(1e-6), "0.000001"),
            (Value::number(1e-7), "1e-7"),
            (Value::number(1e20), "100000000000000000000"),
            (Value::number(1e21), "1e+21"),
        ];
        for (value, expected) in cases {
            let key = heap.to_property_key(value).unwrap();
            let actual = String::from_utf16(heap.property_key_utf16(key).unwrap()).unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn string_property_key_conversion_preserves_unpaired_surrogates() {
        let mut heap = heap();
        let string = heap.allocate_string_utf16(&[0x0061, 0xD800]).unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, string).unwrap();

        let key = heap.to_property_key(string).unwrap();
        assert_eq!(heap.property_key_utf16(key).unwrap(), &[0x0061, 0xD800]);
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn own_property_keys_order_indices_then_strings_and_array_length() {
        let mut heap = heap();
        let object = heap.allocate_object().unwrap();
        let array = heap.allocate_array().unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, object).unwrap();
        heap.scope_root(roots, array).unwrap();

        for name in ["tail", "10", "2", "01", "4294967295", "0"] {
            let key = heap.intern_property_key(name).unwrap();
            assert!(heap.set_property(object, key, Value::undefined()).unwrap());
        }
        let names = heap
            .own_property_keys(object)
            .unwrap()
            .iter()
            .map(|key| String::from_utf16(heap.property_key_utf16(*key).unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["0", "2", "10", "tail", "01", "4294967295"]);

        let one = heap.intern_property_key("1").unwrap();
        let three = heap.intern_property_key("3").unwrap();
        let extra = heap.intern_property_key("extra").unwrap();
        assert!(heap.set_property(array, three, Value::undefined()).unwrap());
        assert!(heap.set_property(array, extra, Value::undefined()).unwrap());
        assert!(heap.set_property(array, one, Value::undefined()).unwrap());
        let names = heap
            .own_property_keys(array)
            .unwrap()
            .iter()
            .map(|key| String::from_utf16(heap.property_key_utf16(*key).unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["1", "3", "length", "extra"]);
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn bounded_own_property_keys_preserve_order_and_limit_results() {
        let mut heap = heap();
        let object = heap.allocate_object().unwrap();
        let array = heap.allocate_array().unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, object).unwrap();
        heap.scope_root(roots, array).unwrap();

        for name in ["tail", "10", "2", "01", "4294967295", "0", "later"] {
            let key = heap.intern_property_key(name).unwrap();
            assert!(heap.set_property(object, key, Value::undefined()).unwrap());
        }
        let names = |heap: &Heap, keys: &[PropertyKey]| {
            keys.iter()
                .map(|key| String::from_utf16(heap.property_key_utf16(*key).unwrap()).unwrap())
                .collect::<Vec<_>>()
        };

        let (keys, truncated) = heap.own_property_keys_bounded(object, 3).unwrap();
        assert_eq!(names(&heap, &keys), vec!["0", "2", "10"]);
        assert!(truncated);

        let (keys, truncated) = heap.own_property_keys_bounded(object, 5).unwrap();
        assert_eq!(names(&heap, &keys), vec!["0", "2", "10", "tail", "01"]);
        assert!(truncated);

        let (keys, truncated) = heap.own_property_keys_bounded(object, 8).unwrap();
        assert_eq!(
            names(&heap, &keys),
            vec!["0", "2", "10", "tail", "01", "4294967295", "later"]
        );
        assert!(!truncated);

        let (keys, truncated) = heap.own_property_keys_bounded(object, 0).unwrap();
        assert!(keys.is_empty());
        assert!(truncated);

        for name in ["3", "extra", "1", "tail"] {
            let key = heap.intern_property_key(name).unwrap();
            assert!(heap.set_property(array, key, Value::undefined()).unwrap());
        }
        let (keys, truncated) = heap.own_property_keys_bounded(array, 3).unwrap();
        assert_eq!(names(&heap, &keys), vec!["1", "3", "length"]);
        assert!(truncated);

        let (keys, truncated) = heap.own_property_keys_bounded(array, 5).unwrap();
        assert_eq!(
            names(&heap, &keys),
            vec!["1", "3", "length", "extra", "tail"]
        );
        assert!(!truncated);
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn inherited_length_assignment_uses_array_receiver_semantics() {
        let mut heap = heap();
        let prototype = heap.allocate_object().unwrap();
        let array = heap.allocate_array().unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, prototype).unwrap();
        heap.scope_root(roots, array).unwrap();
        let length = key(&mut heap, "length");
        assert!(
            heap.define_own_property(
                prototype,
                length,
                PropertyDescriptor::data(Value::number(8.0), true, true, true),
            )
            .unwrap()
        );

        assert!(
            heap.set_property_with_receiver(prototype, length, Value::number(2.0), array)
                .unwrap()
        );
        assert_eq!(heap.array_length(array).unwrap(), 2);
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn sparse_arrays_preserve_holes_and_length_semantics() {
        let mut heap = heap();
        let array = heap.allocate_array().unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, array).unwrap();
        let zero = key(&mut heap, "0");
        let two = key(&mut heap, "2");
        let length = key(&mut heap, "length");
        assert!(heap.set_property(array, zero, Value::number(10.0)).unwrap());
        assert!(heap.set_property(array, two, Value::number(30.0)).unwrap());
        assert_eq!(heap.array_length(array).unwrap(), 3);
        assert!(heap.delete_property(array, zero).unwrap());
        assert_eq!(heap.array_length(array).unwrap(), 3);
        assert!(!heap.has_own_property(array, zero).unwrap());
        assert_eq!(
            heap.get_property(array, zero).unwrap().value_type(),
            ValueType::Undefined
        );
        assert_eq!(
            heap.get_property(array, length).unwrap().as_number(),
            Some(3.0)
        );
        assert!(
            heap.define_own_property(array, length, PropertyDescriptor::value(Value::number(1.0)))
                .unwrap()
        );
        assert_eq!(heap.array_length(array).unwrap(), 1);
        assert!(!heap.has_own_property(array, two).unwrap());
        assert_eq!(
            heap.define_own_property(
                array,
                length,
                PropertyDescriptor::value(Value::number(-1.0))
            ),
            Err(HeapError::InvalidArrayLength)
        );
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn array_length_failed_shrink_keeps_blocking_index_and_applies_read_only_flag() {
        let mut heap = heap();
        let array = heap.allocate_array().unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, array).unwrap();
        let index = key(&mut heap, "3");
        let length = key(&mut heap, "length");
        assert!(
            heap.define_own_property(
                array,
                index,
                PropertyDescriptor::data(Value::number(3.0), true, true, false),
            )
            .unwrap()
        );
        assert_eq!(heap.array_length(array).unwrap(), 4);

        assert!(
            !heap
                .define_own_property(
                    array,
                    length,
                    PropertyDescriptor {
                        value: Some(Value::number(1.0)),
                        writable: Some(false),
                        enumerable: None,
                        configurable: None,
                    },
                )
                .unwrap()
        );
        assert_eq!(heap.array_length(array).unwrap(), 4);
        let descriptor = heap
            .get_own_property_descriptor(array, length)
            .unwrap()
            .unwrap();
        assert!(!descriptor.writable);
        assert!(
            !heap
                .set_property(array, length, Value::number(2.0))
                .unwrap()
        );
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn property_edges_keep_children_alive_and_release_them_after_delete() {
        let mut heap = heap();
        let owner = heap.allocate_object().unwrap();
        let child = heap.allocate_object().unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, owner).unwrap();
        let child_key = key(&mut heap, "child");
        assert!(heap.set_property(owner, child_key, child).unwrap());

        // The owner is the only explicit root; the graph edge retains the child.
        let report = heap.collect().unwrap();
        assert!(report.marked_objects >= 2);
        let found = heap.get_property(owner, child_key).unwrap();
        assert!(heap.strict_equal(found, child).unwrap());

        assert!(heap.delete_property(owner, child_key).unwrap());
        let report = heap.collect().unwrap();
        assert!(report.reclaimed_objects >= 1);
        assert!(matches!(heap.get_property(owner, child_key), Ok(value) if value.is_undefined()));
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn property_atoms_preserve_utf16_and_reject_foreign_heap_keys() {
        let mut first = heap();
        let mut second = heap();
        let key = first.intern_property_key_utf16(&[0x0061, 0xD800]).unwrap();
        let duplicate = first.intern_property_key_utf16(&[0x0061, 0xD800]).unwrap();
        assert_eq!(key, duplicate);
        assert_eq!(first.property_key_utf16(key).unwrap(), &[0x0061, 0xD800]);
        let object = second.allocate_object().unwrap();
        assert!(matches!(
            second.get_property(object, key),
            Err(HeapError::WrongHeap)
        ));
    }

    #[test]
    fn closure_cell_captures_share_mutations_and_value_captures_copy() {
        let mut heap = heap();
        let outer = heap
            .create_environment(
                None,
                &[
                    BindingDefinition::mutable(BindingId(1), Value::number(1.0)),
                    BindingDefinition::immutable(BindingId(2), Value::number(5.0)),
                ],
            )
            .unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root_environment(roots, outer).unwrap();
        let definition = FunctionDefinition {
            id: FunctionId(7),
            parameters: vec![BindingDefinition::mutable(BindingId(3), Value::undefined())],
            locals: Vec::new(),
            captures: vec![
                CaptureDefinition {
                    source: BindingId(1),
                    alias: BindingId(11),
                    mode: CaptureMode::Cell,
                },
                CaptureDefinition {
                    source: BindingId(2),
                    alias: BindingId(12),
                    mode: CaptureMode::Value,
                },
            ],
            kind: FunctionKind::Ordinary,
            strict: true,
        };
        let closure = heap
            .create_closure(&definition, outer, Value::undefined())
            .unwrap();
        heap.scope_root(roots, closure).unwrap();
        let activation = heap
            .enter_function_call(
                closure,
                &definition,
                Value::undefined(),
                &[Value::number(9.0)],
                Value::undefined(),
            )
            .unwrap();
        assert_eq!(
            heap.load_binding(activation.environment(), BindingId(3))
                .unwrap()
                .as_number(),
            Some(9.0)
        );
        heap.store_binding(activation.environment(), BindingId(11), Value::number(2.0))
            .unwrap();
        assert_eq!(
            heap.load_binding(outer, BindingId(1)).unwrap().as_number(),
            Some(2.0)
        );
        assert_eq!(
            heap.load_binding(activation.environment(), BindingId(12))
                .unwrap()
                .as_number(),
            Some(5.0)
        );
        assert!(heap.collect().unwrap().marked_objects >= 4);
        heap.leave_function_call(activation).unwrap();
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn returned_closure_keeps_captures_alive_after_outer_environment_collection() {
        let mut heap = heap();
        let outer = heap
            .create_environment(
                None,
                &[
                    BindingDefinition::mutable(BindingId(1), Value::number(1.0)),
                    BindingDefinition::immutable(BindingId(2), Value::number(5.0)),
                ],
            )
            .unwrap();
        let outer_roots = heap.open_scope().unwrap();
        heap.scope_root_environment(outer_roots, outer).unwrap();
        let definition = FunctionDefinition {
            id: FunctionId(10),
            parameters: vec![BindingDefinition::mutable(BindingId(3), Value::undefined())],
            locals: Vec::new(),
            captures: vec![
                CaptureDefinition {
                    source: BindingId(1),
                    alias: BindingId(11),
                    mode: CaptureMode::Cell,
                },
                CaptureDefinition {
                    source: BindingId(2),
                    alias: BindingId(12),
                    mode: CaptureMode::Value,
                },
            ],
            kind: FunctionKind::Ordinary,
            strict: true,
        };
        let closure = heap
            .create_closure(&definition, outer, Value::undefined())
            .unwrap();

        // Model returning the closure: the enclosing function's environment
        // ceases to be rooted, while the returned function remains reachable.
        heap.close_scope(outer_roots).unwrap();
        let closure_roots = heap.open_scope().unwrap();
        heap.scope_root(closure_roots, closure).unwrap();
        let collection = heap.collect().unwrap();
        assert!(collection.reclaimed_objects >= 1);

        let first_call = heap
            .enter_function_call(
                closure,
                &definition,
                Value::undefined(),
                &[Value::number(9.0)],
                Value::undefined(),
            )
            .unwrap();
        assert_eq!(
            heap.load_binding(first_call.environment(), BindingId(3))
                .unwrap()
                .as_number(),
            Some(9.0)
        );
        assert_eq!(
            heap.load_binding(first_call.environment(), BindingId(11))
                .unwrap()
                .as_number(),
            Some(1.0)
        );
        heap.store_binding(first_call.environment(), BindingId(11), Value::number(2.0))
            .unwrap();
        heap.leave_function_call(first_call).unwrap();
        heap.collect().unwrap();

        // A later invocation observes the same captured mutable cell even
        // after the previous activation and original environment were swept.
        let second_call = heap
            .enter_function_call(
                closure,
                &definition,
                Value::undefined(),
                &[Value::number(10.0)],
                Value::undefined(),
            )
            .unwrap();
        assert_eq!(
            heap.load_binding(second_call.environment(), BindingId(3))
                .unwrap()
                .as_number(),
            Some(10.0)
        );
        assert_eq!(
            heap.load_binding(second_call.environment(), BindingId(11))
                .unwrap()
                .as_number(),
            Some(2.0)
        );
        assert_eq!(
            heap.load_binding(second_call.environment(), BindingId(12))
                .unwrap()
                .as_number(),
            Some(5.0)
        );
        heap.leave_function_call(second_call).unwrap();
        heap.close_scope(closure_roots).unwrap();
    }

    #[test]
    fn nested_function_activations_keep_parent_frames_rooted_during_collection() {
        let mut heap = heap();
        let outer_environment = heap.create_environment(None, &[]).unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root_environment(roots, outer_environment)
            .unwrap();

        let outer_definition = FunctionDefinition {
            id: FunctionId(20),
            parameters: vec![BindingDefinition::mutable(BindingId(1), Value::undefined())],
            locals: Vec::new(),
            captures: Vec::new(),
            kind: FunctionKind::Ordinary,
            strict: true,
        };
        let outer_function = heap
            .create_closure(&outer_definition, outer_environment, Value::undefined())
            .unwrap();
        heap.scope_root(roots, outer_function).unwrap();
        let outer_call = heap
            .enter_function_call(
                outer_function,
                &outer_definition,
                Value::undefined(),
                &[Value::number(42.0)],
                Value::undefined(),
            )
            .unwrap();

        let inner_definition = FunctionDefinition {
            id: FunctionId(21),
            parameters: vec![BindingDefinition::mutable(BindingId(2), Value::undefined())],
            locals: Vec::new(),
            captures: vec![CaptureDefinition {
                source: BindingId(1),
                alias: BindingId(11),
                mode: CaptureMode::Cell,
            }],
            kind: FunctionKind::Ordinary,
            strict: true,
        };
        let inner_function = heap
            .create_closure(
                &inner_definition,
                outer_call.environment(),
                Value::undefined(),
            )
            .unwrap();
        heap.scope_root(roots, inner_function).unwrap();
        let inner_call = heap
            .enter_function_call(
                inner_function,
                &inner_definition,
                Value::undefined(),
                &[Value::number(7.0)],
                Value::undefined(),
            )
            .unwrap();

        let report = heap.collect().unwrap();
        assert!(report.marked_objects >= 6);
        assert_eq!(
            heap.load_binding(outer_call.environment(), BindingId(1))
                .unwrap()
                .as_number(),
            Some(42.0)
        );
        assert_eq!(
            heap.load_binding(inner_call.environment(), BindingId(2))
                .unwrap()
                .as_number(),
            Some(7.0)
        );
        assert_eq!(
            heap.load_binding(inner_call.environment(), BindingId(11))
                .unwrap()
                .as_number(),
            Some(42.0)
        );
        heap.store_binding(inner_call.environment(), BindingId(11), Value::number(99.0))
            .unwrap();
        assert_eq!(
            heap.load_binding(outer_call.environment(), BindingId(1))
                .unwrap()
                .as_number(),
            Some(99.0)
        );

        heap.leave_function_call(inner_call).unwrap();
        heap.collect().unwrap();
        assert_eq!(
            heap.load_binding(outer_call.environment(), BindingId(1))
                .unwrap()
                .as_number(),
            Some(99.0)
        );
        heap.leave_function_call(outer_call).unwrap();
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn closure_exposes_function_table_identity_and_unbound_function_is_not_callable() {
        let mut heap = heap();
        let outer = heap.create_environment(None, &[]).unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root_environment(roots, outer).unwrap();

        let unbound = heap.allocate_function().unwrap();
        heap.scope_root(roots, unbound).unwrap();
        assert!(matches!(
            heap.function_id(unbound),
            Err(HeapError::NotCallable)
        ));

        let definition = FunctionDefinition {
            id: FunctionId(33),
            parameters: Vec::new(),
            locals: Vec::new(),
            captures: Vec::new(),
            kind: FunctionKind::Ordinary,
            strict: true,
        };
        let closure = heap
            .create_closure(&definition, outer, Value::undefined())
            .unwrap();
        heap.scope_root(roots, closure).unwrap();
        assert_eq!(heap.function_id(closure).unwrap(), definition.id);
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn arrow_uses_lexical_receiver_and_does_not_root_ignored_call_receiver() {
        let mut heap = heap();
        let lexical_this = heap.allocate_object().unwrap();
        let ignored_call_this = heap.allocate_object().unwrap();
        let outer = heap.create_environment(None, &[]).unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, lexical_this).unwrap();
        heap.scope_root_environment(roots, outer).unwrap();

        let definition = FunctionDefinition {
            id: FunctionId(34),
            parameters: Vec::new(),
            locals: Vec::new(),
            captures: Vec::new(),
            kind: FunctionKind::Arrow,
            strict: false,
        };
        let arrow = heap
            .create_closure(&definition, outer, lexical_this)
            .unwrap();
        heap.scope_root(roots, arrow).unwrap();
        let activation = heap
            .enter_function_call(
                arrow,
                &definition,
                ignored_call_this,
                &[],
                Value::undefined(),
            )
            .unwrap();

        let report = heap.collect().unwrap();
        assert!(report.reclaimed_objects >= 1);
        assert!(
            heap.strict_equal(activation.this_value(), lexical_this)
                .unwrap()
        );
        heap.leave_function_call(activation).unwrap();
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn ordinary_function_does_not_capture_or_retain_lexical_this() {
        let mut heap = heap();
        let outer = heap.create_environment(None, &[]).unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root_environment(roots, outer).unwrap();

        let unused_lexical_this = heap.allocate_object().unwrap();
        let definition = FunctionDefinition {
            id: FunctionId(35),
            parameters: Vec::new(),
            locals: Vec::new(),
            captures: Vec::new(),
            kind: FunctionKind::Ordinary,
            strict: true,
        };
        let function = heap
            .create_closure(&definition, outer, unused_lexical_this)
            .unwrap();
        heap.scope_root(roots, function).unwrap();

        let report = heap.collect().unwrap();
        assert!(report.reclaimed_objects >= 1);
        let activation = heap
            .enter_function_call(
                function,
                &definition,
                Value::null(),
                &[],
                Value::undefined(),
            )
            .unwrap();
        assert!(activation.this_value().is_null());
        heap.leave_function_call(activation).unwrap();
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn sloppy_ordinary_call_substitutes_global_receiver_for_nullish_this() {
        let mut heap = heap();
        let global_this = heap.allocate_object().unwrap();
        let outer = heap.create_environment(None, &[]).unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, global_this).unwrap();
        heap.scope_root_environment(roots, outer).unwrap();
        let definition = FunctionDefinition {
            id: FunctionId(36),
            parameters: Vec::new(),
            locals: Vec::new(),
            captures: Vec::new(),
            kind: FunctionKind::Ordinary,
            strict: false,
        };
        let function = heap
            .create_closure(&definition, outer, Value::undefined())
            .unwrap();
        heap.scope_root(roots, function).unwrap();

        let activation = heap
            .enter_function_call(function, &definition, Value::undefined(), &[], global_this)
            .unwrap();
        assert!(
            heap.strict_equal(activation.this_value(), global_this)
                .unwrap()
        );
        heap.leave_function_call(activation).unwrap();
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn lexical_store_rejects_tdz_and_initialization_is_one_shot() {
        let mut heap = heap();
        let environment = heap
            .create_environment(
                None,
                &[BindingDefinition::uninitialized(BindingId(1), true)],
            )
            .unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root_environment(roots, environment).unwrap();

        assert!(matches!(
            heap.store_binding(environment, BindingId(1), Value::number(1.0)),
            Err(HeapError::UninitializedBinding)
        ));
        assert_eq!(
            heap.initialize_binding(environment, BindingId(1), Value::number(1.0))
                .unwrap()
                .as_number(),
            Some(1.0)
        );
        assert!(matches!(
            heap.initialize_binding(environment, BindingId(1), Value::number(2.0)),
            Err(HeapError::AlreadyInitialized)
        ));
        assert_eq!(
            heap.store_binding(environment, BindingId(1), Value::number(3.0))
                .unwrap()
                .as_number(),
            Some(3.0)
        );
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn imported_binding_alias_is_live_and_read_only() {
        let mut heap = heap();
        let roots = heap.open_scope().unwrap();
        let exporter = heap
            .create_environment(
                None,
                &[BindingDefinition::mutable(
                    BindingId(1),
                    Value::number(10.0),
                )],
            )
            .unwrap();
        heap.scope_root_environment(roots, exporter).unwrap();
        let importer = heap
            .create_environment(
                None,
                &[BindingDefinition::uninitialized(BindingId(2), false)],
            )
            .unwrap();
        heap.scope_root_environment(roots, importer).unwrap();

        heap.link_binding_alias(importer, BindingId(2), exporter, BindingId(1))
            .unwrap();
        assert_eq!(
            heap.load_binding(importer, BindingId(2))
                .unwrap()
                .as_number(),
            Some(10.0)
        );
        heap.store_binding(exporter, BindingId(1), Value::number(11.0))
            .unwrap();
        assert_eq!(
            heap.load_binding(importer, BindingId(2))
                .unwrap()
                .as_number(),
            Some(11.0)
        );
        assert!(matches!(
            heap.store_binding(importer, BindingId(2), Value::number(12.0)),
            Err(HeapError::ImmutableBinding)
        ));
        assert!(matches!(
            heap.initialize_binding(importer, BindingId(2), Value::number(12.0)),
            Err(HeapError::ImmutableBinding)
        ));
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn imported_binding_alias_survives_gc_and_remains_live_through_closure_capture() {
        let mut heap = heap();

        // The importer is rooted by the outer scope. The exporter is rooted
        // only by a nested scope that is closed after the alias is installed.
        let importer_roots = heap.open_scope().unwrap();
        let importer = heap
            .create_environment(
                None,
                &[BindingDefinition::uninitialized(BindingId(2), false)],
            )
            .unwrap();
        heap.scope_root_environment(importer_roots, importer)
            .unwrap();

        let exporter_roots = heap.open_scope().unwrap();
        let exporter = heap
            .create_environment(
                None,
                &[BindingDefinition::mutable(
                    BindingId(1),
                    Value::number(20.0),
                )],
            )
            .unwrap();
        heap.scope_root_environment(exporter_roots, exporter)
            .unwrap();
        heap.link_binding_alias(importer, BindingId(2), exporter, BindingId(1))
            .unwrap();
        heap.close_scope(exporter_roots).unwrap();
        heap.collect().unwrap();

        let definition = FunctionDefinition {
            id: FunctionId(90),
            parameters: Vec::new(),
            locals: Vec::new(),
            captures: vec![CaptureDefinition {
                source: BindingId(2),
                alias: BindingId(10),
                mode: CaptureMode::Cell,
            }],
            kind: FunctionKind::Ordinary,
            strict: true,
        };
        let closure = heap
            .create_closure(&definition, importer, Value::undefined())
            .unwrap();
        heap.scope_root(importer_roots, closure).unwrap();
        heap.collect().unwrap();

        let activation = heap
            .enter_function_call(
                closure,
                &definition,
                Value::undefined(),
                &[],
                Value::undefined(),
            )
            .unwrap();
        assert_eq!(
            heap.load_binding(activation.environment(), BindingId(10))
                .unwrap()
                .as_number(),
            Some(20.0)
        );
        heap.store_binding(exporter, BindingId(1), Value::number(21.0))
            .unwrap();
        assert_eq!(
            heap.load_binding(activation.environment(), BindingId(10))
                .unwrap()
                .as_number(),
            Some(21.0)
        );
        heap.leave_function_call(activation).unwrap();
        heap.close_scope(importer_roots).unwrap();
    }

    #[test]
    fn promise_settlement_releases_pending_reaction_edges() {
        let mut heap = heap();
        let source = heap
            .allocate_promise_record(super::super::PromiseState::Pending)
            .unwrap();
        let (child, reaction) = heap.promise_then(source, None, None).unwrap();
        assert!(reaction.is_none());

        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, child).unwrap();
        let pending_accounting = heap.stats().external_memory_bytes;
        assert!(pending_accounting > 0);
        heap.collect().unwrap();
        assert!(
            heap.is_promise(source),
            "the pending child retains its source"
        );

        let reactions = heap
            .settle_promise(
                source,
                super::super::PromiseState::Fulfilled(Value::number(7.0)),
            )
            .unwrap();
        assert_eq!(reactions.len(), 1);
        assert_eq!(
            heap.stats().external_memory_bytes,
            pending_accounting - std::mem::size_of::<super::super::PromiseReaction>()
        );

        heap.collect().unwrap();
        assert!(
            !heap.is_promise(source),
            "a settled child must not retain its source"
        );
        assert!(heap.is_promise(child));
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn promise_adoption_releases_bidirectional_edges_after_source_settles() {
        let mut heap = heap();
        let source = heap
            .allocate_promise_record(super::super::PromiseState::Pending)
            .unwrap();
        let target = heap
            .allocate_promise_record(super::super::PromiseState::Pending)
            .unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, target).unwrap();

        assert!(heap.promise_forward_to(source, target).unwrap().is_empty());
        let pending_accounting = heap.stats().external_memory_bytes;
        assert!(pending_accounting > 0);
        heap.collect().unwrap();
        assert!(
            heap.is_promise(source),
            "the adopting promise retains the source"
        );

        let reactions = heap
            .settle_promise(
                source,
                super::super::PromiseState::Fulfilled(Value::number(9.0)),
            )
            .unwrap();
        assert_eq!(reactions.len(), 1);
        assert_eq!(
            heap.stats().external_memory_bytes,
            pending_accounting - std::mem::size_of::<super::super::PromiseReaction>()
        );

        heap.collect().unwrap();
        assert!(
            !heap.is_promise(source),
            "settlement removes the adoption links"
        );
        assert!(heap.is_promise(target));
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn imported_binding_alias_cycles_are_rejected_before_publication() {
        let mut heap = heap();
        let environment = heap
            .create_environment(
                None,
                &[
                    BindingDefinition::uninitialized(BindingId(1), false),
                    BindingDefinition::uninitialized(BindingId(2), false),
                ],
            )
            .unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root_environment(roots, environment).unwrap();

        heap.link_binding_alias(environment, BindingId(1), environment, BindingId(2))
            .unwrap();
        assert_eq!(
            heap.link_binding_alias(environment, BindingId(2), environment, BindingId(1)),
            Err(HeapError::BindingAliasCycle)
        );
        assert!(matches!(
            heap.load_binding(environment, BindingId(1)),
            Err(HeapError::UninitializedBinding)
        ));
        assert!(matches!(
            heap.load_binding(environment, BindingId(2)),
            Err(HeapError::UninitializedBinding)
        ));
        heap.close_scope(roots).unwrap();
    }

    #[test]
    fn call_activation_roots_receiver_parameters_and_tdz_until_exit() {
        let mut heap = heap();
        let global = heap.allocate_object().unwrap();
        let outer = heap.create_environment(None, &[]).unwrap();
        let roots = heap.open_scope().unwrap();
        heap.scope_root(roots, global).unwrap();
        heap.scope_root_environment(roots, outer).unwrap();
        let definition = FunctionDefinition {
            id: FunctionId(9),
            parameters: vec![BindingDefinition::mutable(BindingId(1), Value::undefined())],
            locals: vec![BindingDefinition::uninitialized(BindingId(2), true)],
            captures: Vec::new(),
            kind: FunctionKind::Ordinary,
            strict: true,
        };
        let function = heap
            .create_closure(&definition, outer, Value::undefined())
            .unwrap();
        heap.scope_root(roots, function).unwrap();
        let call = heap
            .enter_function_call(function, &definition, global, &[Value::number(4.0)], global)
            .unwrap();
        assert!(heap.strict_equal(call.this_value(), global).unwrap());
        assert_eq!(
            heap.load_binding(call.environment(), BindingId(1))
                .unwrap()
                .as_number(),
            Some(4.0)
        );
        assert!(matches!(
            heap.load_binding(call.environment(), BindingId(2)),
            Err(HeapError::UninitializedBinding)
        ));
        heap.initialize_binding(call.environment(), BindingId(2), Value::number(6.0))
            .unwrap();
        heap.leave_function_call(call).unwrap();
        heap.close_scope(roots).unwrap();
        assert!(heap.collect().unwrap().reclaimed_objects > 0);
    }
}
