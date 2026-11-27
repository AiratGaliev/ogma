//! Architecture-neutral tagged values for the M1 Runtime.
//!
//! The representation is deliberately private and has no stable C layout. The
//! embedding boundary exports opaque handles, never this representation.

use std::marker::PhantomData;
use std::rc::Rc;

use crate::heap::GcRef;

/// The ECMAScript value category represented by a Value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
    /// The undefined primitive.
    Undefined,
    /// The null primitive.
    Null,
    /// A Boolean primitive.
    Boolean,
    /// An IEEE-754 binary64 Number primitive.
    Number,
    /// A UTF-16 string primitive.
    String,
    /// An ordinary object reference.
    Object,
    /// A callable object reference.
    Function,
}

/// An opaque, thread-affine Runtime value.
///
/// Values use a private tagged Rust representation. No field, discriminant,
/// pointer, or bit-level encoding is part of the public ABI. In particular,
/// this type is not repr(C) and must not be serialized by copying its memory.
/// Values are deliberately !Send and !Sync and must remain on the owner thread.
#[derive(Clone, Copy)]
pub struct Value(ValueRepr, PhantomData<Rc<()>>);

impl std::fmt::Debug for Value {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("Value")
            .field(&self.value_type())
            .finish()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ValueRepr {
    Undefined,
    Null,
    Boolean(bool),
    Number(f64),
    String(GcRef),
    Object(GcRef),
    Function(GcRef),
}

impl Value {
    /// Creates the JavaScript undefined value.
    #[must_use]
    pub const fn undefined() -> Self {
        Self(ValueRepr::Undefined, PhantomData)
    }

    /// Creates the JavaScript null value.
    #[must_use]
    pub const fn null() -> Self {
        Self(ValueRepr::Null, PhantomData)
    }

    /// Creates a Boolean value.
    #[must_use]
    pub const fn boolean(value: bool) -> Self {
        Self(ValueRepr::Boolean(value), PhantomData)
    }

    /// Creates a Number without canonicalizing NaN or signed zero.
    #[must_use]
    pub const fn number(value: f64) -> Self {
        Self(ValueRepr::Number(value), PhantomData)
    }

    /// Returns this value's ECMAScript category.
    #[must_use]
    pub const fn value_type(self) -> ValueType {
        match self.0 {
            ValueRepr::Undefined => ValueType::Undefined,
            ValueRepr::Null => ValueType::Null,
            ValueRepr::Boolean(_) => ValueType::Boolean,
            ValueRepr::Number(_) => ValueType::Number,
            ValueRepr::String(_) => ValueType::String,
            ValueRepr::Object(_) => ValueType::Object,
            ValueRepr::Function(_) => ValueType::Function,
        }
    }

    /// Returns the contained Number when this is a Number value.
    #[must_use]
    pub const fn as_number(self) -> Option<f64> {
        match self.0 {
            ValueRepr::Number(value) => Some(value),
            _ => None,
        }
    }

    /// Returns the contained Boolean when this is a Boolean value.
    #[must_use]
    pub const fn as_boolean(self) -> Option<bool> {
        match self.0 {
            ValueRepr::Boolean(value) => Some(value),
            _ => None,
        }
    }

    /// Returns whether this value is undefined.
    #[must_use]
    pub const fn is_undefined(self) -> bool {
        matches!(self.0, ValueRepr::Undefined)
    }

    /// Returns whether this value is null.
    #[must_use]
    pub const fn is_null(self) -> bool {
        matches!(self.0, ValueRepr::Null)
    }

    pub(crate) const fn from_string(reference: GcRef) -> Self {
        Self(ValueRepr::String(reference), PhantomData)
    }

    pub(crate) const fn from_object(reference: GcRef) -> Self {
        Self(ValueRepr::Object(reference), PhantomData)
    }

    pub(crate) const fn from_function(reference: GcRef) -> Self {
        Self(ValueRepr::Function(reference), PhantomData)
    }

    pub(crate) const fn gc_ref(self) -> Option<GcRef> {
        match self.0 {
            ValueRepr::String(reference)
            | ValueRepr::Object(reference)
            | ValueRepr::Function(reference) => Some(reference),
            ValueRepr::Undefined
            | ValueRepr::Null
            | ValueRepr::Boolean(_)
            | ValueRepr::Number(_) => None,
        }
    }

    pub(crate) const fn repr(self) -> ValueRepr {
        self.0
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::boolean(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::number(value)
    }
}

impl From<f32> for Value {
    fn from(value: f32) -> Self {
        Self::number(f64::from(value))
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Self::number(f64::from(value))
    }
}

impl From<u32> for Value {
    fn from(value: u32) -> Self {
        Self::number(f64::from(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_number_bit_patterns_round_trip_without_canonicalization() {
        let mut seed = 0x5eed_cafe_d00d_beefu64;
        for _ in 0..1024 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let number = f64::from_bits(seed);
            assert_eq!(Value::number(number).as_number().unwrap().to_bits(), seed);
        }
    }

    #[test]
    fn primitive_values_keep_explicit_categories_and_number_bits() {
        assert_eq!(Value::undefined().value_type(), ValueType::Undefined);
        assert_eq!(Value::null().value_type(), ValueType::Null);
        assert_eq!(Value::boolean(true).as_boolean(), Some(true));
        assert_eq!(
            Value::number(f64::INFINITY).as_number(),
            Some(f64::INFINITY)
        );
        assert_eq!(
            Value::number(f64::NEG_INFINITY).as_number(),
            Some(f64::NEG_INFINITY)
        );
        assert!(Value::number(f64::NAN).as_number().unwrap().is_nan());
        assert_eq!(
            Value::number(-0.0).as_number().unwrap().to_bits(),
            (-0.0_f64).to_bits()
        );
        assert!(Value::undefined().is_undefined());
        assert!(Value::null().is_null());
        assert_eq!(Value::from(42_i32).as_number(), Some(42.0));
    }

    #[test]
    fn reference_categories_are_distinct_from_primitive_categories() {
        let reference = GcRef {
            heap_id: 1,
            index: 0,
            generation: 1,
        };
        assert_eq!(
            Value::from_string(reference).value_type(),
            ValueType::String
        );
        assert_eq!(
            Value::from_object(reference).value_type(),
            ValueType::Object
        );
        assert_eq!(
            Value::from_function(reference).value_type(),
            ValueType::Function
        );
        assert_eq!(Value::from_object(reference).gc_ref(), Some(reference));
    }
}
