//! `deranged` is a proof-of-concept implementation of ranged integers.
//!  Fork optimizes `max(utility) = max(correctness - friction)`, which upstream totally rejects (no literals, no 3rd party crates, etc)

#![cfg_attr(docsrs, feature(doc_cfg))]
#![no_std]
#![doc(test(attr(deny(warnings))))]

#[cfg(all(
    feature = "alloc",
    any(
        feature = "serde",
        feature = "quickcheck",
        feature = "sqlx09-pg",
        feature = "schemars"
    )
))]
extern crate alloc;

#[cfg(test)]
mod tests;
mod unsafe_wrapper;

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::error::Error;
use core::fmt;
use core::hint::assert_unchecked;
use core::mem::{align_of, size_of};
use core::num::NonZero;
use core::str::FromStr;

pub use core::num::{IntErrorKind, ParseIntError};

/// A macro to define a ranged integer with an automatically computed inner type.
///
/// The minimum and maximum values are provided as integer literals, and the macro will compute an
/// appropriate inner type to represent the range. This will be the smallest integer type that can
/// store both the minimum and maximum values, with a preference for unsigned types if both are
/// possible. To specifically request a signed or unsigned type, you can append a `i` or `u` suffix
/// to either or both of the minimum and maximum values, respectively.
///
/// # Examples
///
/// ```rust,ignore
/// int!(0, 100);  // RangedU8<0, 100>
/// int!(0i, 100); // RangedI8<0, 100>
/// int!(-5, 5);   // RangedI8<-5, 5>
/// int!(-5u, 5);  // compile error (-5 cannot be unsigned)
/// ```
#[cfg(docsrs)]
#[doc(cfg(feature = "macros"))]
#[macro_export]
macro_rules! int {
    ($min:literal, $max:literal) => {};
}

/// A macro to define an optional ranged integer with an automatically computed inner type.
///
/// The minimum and maximum values are provided as integer literals, and the macro will compute an
/// appropriate inner type to represent the range. This will be the smallest integer type that can
/// store both the minimum and maximum values, with a preference for unsigned types if both are
/// possible. To specifically request a signed or unsigned type, you can append a `i` or `u` suffix
/// to either or both of the minimum and maximum values, respectively.
///
/// # Examples
///
/// ```rust,ignore
/// opt_int!(0, 100);  // OptionRangedU8<0, 100>
/// opt_int!(0i, 100); // OptionRangedI8<0, 100>
/// opt_int!(-5, 5);   // OptionRangedI8<-5, 5>
/// opt_int!(-5u, 5);  // compile error (-5 cannot be unsigned)
/// ```
#[cfg(docsrs)]
#[doc(cfg(feature = "macros"))]
#[macro_export]
macro_rules! opt_int {
    ($min:literal, $max:literal) => {};
}

#[cfg(all(not(docsrs), feature = "macros"))]
pub use deranged_macros::int;
#[cfg(all(not(docsrs), feature = "macros"))]
pub use deranged_macros::opt_int;
#[cfg(feature = "powerfmt")]
use powerfmt::smart_display;

use crate::unsafe_wrapper::Unsafe;

/// The error type returned when a checked integral type conversion fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TryFromIntError;

impl fmt::Display for TryFromIntError {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("out of range integral type conversion attempted")
    }
}

impl Error for TryFromIntError {}

/// Constructs a primitive parse error with the provided kind.
#[inline(always)]
const fn parse_int_error_from_kind(kind: IntErrorKind) -> ParseIntError {
    // Safety: Proved by const time asserts that `ParseIntError` has the same layout as `IntErrorKind`.
    unsafe { core::mem::transmute_copy(&kind) }
}

/// Asserts that a parse result failed with the expected error kind.
const fn assert_error_kind<T: Copy>(result: Result<T, ParseIntError>, expected: IntErrorKind) {
    let Err(error) = result else {
        const_panic::concat_panic!("expected parse error")
    };
    let expected = parse_int_error_from_kind(expected);
    let actual_bytes: [u8; size_of::<ParseIntError>()] =
        // Safety: is called only from constant at compile time, will panic if not safe
        unsafe { core::mem::transmute_copy(&error) };
    let expected_bytes: [u8; size_of::<ParseIntError>()] =
        // Safety: is called only from constant at compile time, will panic if not safe
        unsafe { core::mem::transmute_copy(&expected) };
    let mut index = 0;
    while index < size_of::<ParseIntError>() {
        const_panic::concat_assert!(
            actual_bytes[index] == expected_bytes[index],
            "unsupported version of rust std/compiler: ParseIntError byte ",
            index,
            " was ",
            actual_bytes[index],
            ", expected ",
            expected_bytes[index],
        );
        index += 1;
    }
}

/// `core::num::ParseIntError` must have the same layout as `core::num::IntErrorKind`.
///
/// `ParseIntError` is not copy because, up to Rust 1.44, `source` and `cause`
/// returned a `str` reference that may have been stored in the struct.
///
/// Also there are several attempts to open this error,
/// in general peopl are not agains.
const _: () = {
    const_panic::concat_assert!(
        size_of::<ParseIntError>() == size_of::<IntErrorKind>(),
        "unsupported version of rust std/compiler: ParseIntError size ",
        size_of::<ParseIntError>(),
        " != IntErrorKind size ",
        size_of::<IntErrorKind>(),
    );
    const_panic::concat_assert!(
        align_of::<ParseIntError>() == align_of::<IntErrorKind>(),
        "unsupported version of rust std/compiler: ParseIntError alignment ",
        align_of::<ParseIntError>(),
        " != IntErrorKind alignment ",
        align_of::<IntErrorKind>(),
    );

    assert_error_kind(u8::from_str_radix("", 10), IntErrorKind::Empty);
    assert_error_kind(i32::from_str_radix(":>", 10), IntErrorKind::InvalidDigit);
    assert_error_kind(u8::from_str_radix("256", 10), IntErrorKind::PosOverflow);
    assert_error_kind(i8::from_str_radix("-129", 10), IntErrorKind::NegOverflow);
    // assert_error_kind!(
    //     // The real parser source for this is `NonZero::<u8>::from_str_radix("0", 10)`,
    //     // but that is not stable as a const fn yet.
    //     NonZero::<u8>::from_str_radix("0", 10),
    //     IntErrorKind::Zero,
    //     "expected zero parse error layout"
    // );
};

/// `?` for `Option` types, usable in `const` contexts.
macro_rules! const_try_opt {
    ($e:expr) => {
        match $e {
            Some(value) => value,
            None => return None,
        }
    };
}

/// Output the given tokens if the type is signed, otherwise output nothing.
macro_rules! if_signed {
    (true $($x:tt)*) => { $($x)*};
    (false $($x:tt)*) => {};
}

/// Output the given tokens if the type is unsigned, otherwise output nothing.
macro_rules! if_unsigned {
    (true $($x:tt)*) => {};
    (false $($x:tt)*) => { $($x)* };
}

/// The smallest JSON integer that is safe for Web clients.
#[cfg(any(feature = "serde", feature = "schemars"))]
#[allow(clippy::decimal_literal_representation)]
const JSON_SAFE_SIGNED_INTEGER_MIN: i128 = -9_007_199_254_740_991;

/// The largest JSON integer that is safe for Web clients.
#[cfg(any(feature = "serde", feature = "schemars"))]
#[allow(clippy::decimal_literal_representation)]
const JSON_SAFE_SIGNED_INTEGER_MAX: i128 = 9_007_199_254_740_991;

/// The largest unsigned JSON integer that is safe for Web clients.
#[cfg(any(feature = "serde", feature = "schemars"))]
#[allow(clippy::decimal_literal_representation)]
const JSON_SAFE_UNSIGNED_INTEGER_MAX: u128 = 9_007_199_254_740_991;

/// Whether a signed range is within Web JSON's safe integer range.
#[cfg(any(feature = "serde", feature = "schemars"))]
#[inline(always)]
const fn signed_json_safe_integer_range(min: i128, max: i128) -> bool {
    min >= JSON_SAFE_SIGNED_INTEGER_MIN && max <= JSON_SAFE_SIGNED_INTEGER_MAX
}

/// Whether an unsigned range is within Web JSON's safe integer range.
#[cfg(any(feature = "serde", feature = "schemars"))]
#[inline(always)]
const fn unsigned_json_safe_integer_range(max: u128) -> bool {
    max <= JSON_SAFE_UNSIGNED_INTEGER_MAX
}

/// The string pattern for a JSON integer.
#[cfg(feature = "schemars")]
const JSON_SIGNED_INTEGER_PATTERN: &str = "^-?(?:0|[1-9][0-9]*)$";

/// The string pattern for an unsigned JSON integer.
#[cfg(feature = "schemars")]
const JSON_UNSIGNED_INTEGER_PATTERN: &str = "^(?:0|[1-9][0-9]*)$";

/// `"A"` if `true`, `"An"` if `false`.
macro_rules! article {
    (true) => {
        "An"
    };
    (false) => {
        "A"
    };
}

/// Assert that a generated ranged type fits in its SQL storage type.
/// Ideally we'd conditionally implement sqlx enc/dec based on
/// range, via witness types on stable or via const cmp on nightly             
#[cfg(feature = "sqlx09-pg")]
macro_rules! assert_sql_storage_range {
    ($is_signed:ident, $type:ident, $sql_int:ident, $min:expr, $max:expr) => {
        if_signed! { $is_signed
            const_panic::concat_assert!(
                $min as i128 >= <$sql_int>::MIN as i128,
                stringify!($type),
                "<",
                $min,
                ", ",
                $max,
                "> minimum ",
                $min,
                " does not fit SQL storage type ",
                stringify!($sql_int),
                " range ",
                <$sql_int>::MIN,
                "..=",
                <$sql_int>::MAX,
            );
            const_panic::concat_assert!(
                $max as i128 <= <$sql_int>::MAX as i128,
                stringify!($type),
                "<",
                $min,
                ", ",
                $max,
                "> maximum ",
                $max,
                " does not fit SQL storage type ",
                stringify!($sql_int),
                " range ",
                <$sql_int>::MIN,
                "..=",
                <$sql_int>::MAX,
            );
        }
        if_unsigned! { $is_signed
            const_panic::concat_assert!(
                $max as u128 <= <$sql_int>::MAX as u128,
                stringify!($type),
                "<",
                $min,
                ", ",
                $max,
                "> maximum ",
                $max,
                " does not fit SQL storage type ",
                stringify!($sql_int),
                " range ",
                <$sql_int>::MIN,
                "..=",
                <$sql_int>::MAX,
            );
        }
    };
}

/// The capacity of the temporary buffer used to format integration type names.
#[cfg(feature = "schemars")]
const TYPE_NAME_CAPACITY: usize = 192;

/// Output the provided code if and only if the list does not include `rand_09`.
#[allow(unused_macro_rules)]
macro_rules! if_not_manual_rand_09 {
    ([rand_09 $($rest:ident)*] $($output:tt)*) => {};
    ([] $($output:tt)*) => {
        $($output)*
    };
    ([$first:ident $($rest:ident)*] $($output:tt)*) => {
        if_not_manual_rand_09!([$($rest)*] $($output)*);
    };
}

/// Output the provided code if and only if the list does not include `rand_010`.
#[allow(unused_macro_rules)]
macro_rules! if_not_manual_rand_010 {
    ([rand_010 $($rest:ident)*] $($output:tt)*) => {};
    ([] $($output:tt)*) => {
        $($output)*
    };
    ([$first:ident $($rest:ident)*] $($output:tt)*) => {
        if_not_manual_rand_010!([$($rest)*] $($output)*);
    };
}

/// Implement a ranged integer type.
macro_rules! impl_ranged {
    ($(
        $type:ident {
            mod_name: $mod_name:ident
            alias: $alias:ident
            internal: $internal:ident
            signed: $is_signed:ident
            unsigned: $unsigned_type:ident
            optional: $optional_type:ident
            optional_alias: $optional_alias:ident
            from: [$($from:ident($from_internal:ident))+]
            $(prost_type: $prost_type:ident)?
            $(sqlx: $sql_int:ident)?
            $(manual: [$($skips:ident)+])?
        }
    )*) => {$(
        #[doc = concat!("Equivalent to `", stringify!($type), "`")]
        #[expect(non_camel_case_types, reason = "symmetry with primitives")]
        pub type $alias<const MIN: $internal, const MAX: $internal> = $type<MIN, MAX>;

        #[doc = concat!("Equivalent to `", stringify!($optional_type), "`")]
        #[expect(non_camel_case_types, reason = "closest visually to `Option<T>`")]
        pub type $optional_alias<const MIN: $internal, const MAX: $internal>
            = $optional_type<MIN, MAX>;

        #[doc = concat!(
            article!($is_signed),
            " `",
            stringify!($internal),
            "` that is known to be in the range `MIN..=MAX`.",
        )]
        #[repr(transparent)]
        #[derive(Clone, Copy, Eq, Ord, Hash)]
        #[cfg_attr(feature = "zerocopy", derive(
            zerocopy_derive::FromBytes,
            zerocopy_derive::IntoBytes,
            zerocopy_derive::Immutable,
            zerocopy_derive::KnownLayout,
        ))]
        pub struct $type<const MIN: $internal, const MAX: $internal>(
            Unsafe<$internal>,
        );

        #[doc = concat!(
            "An optional `",
            stringify!($type),
            "`; similar to `Option<",
            stringify!($type),
            ">` with better optimization.",
        )]
        ///
        #[doc = concat!(
            "If `MIN` is [`",
            stringify!($internal),
            "::MIN`] _and_ `MAX` is [`",
            stringify!($internal)
            ,"::MAX`] then compilation will fail. This is because there is no way to represent \
            the niche value.",
        )]
        ///
        /// This type is useful when you need to store an optional ranged value in a struct, but
        /// do not want the overhead of an `Option` type. This reduces the size of the struct
        /// overall, and is particularly useful when you have a large number of optional fields.
        /// Note that most operations must still be performed on the [`Option`] type, which is
        #[doc = concat!("obtained with [`", stringify!($optional_type), "::get`].")]
        #[repr(transparent)]
        #[derive(Clone, Copy, Eq, Hash)]
        #[cfg_attr(feature = "zerocopy", derive(
            zerocopy_derive::FromBytes,
            zerocopy_derive::IntoBytes,
            zerocopy_derive::Immutable,
            zerocopy_derive::KnownLayout,
        ))]
        pub struct $optional_type<const MIN: $internal, const MAX: $internal>(
            $internal,
        );

        #[cfg(feature = "bytemuck")]
        // Safety: The type is `repr(transparent)` over an integer type, which has no uninitialized
        // bytes. The range invariant restricts valid values, so this must not imply `AnyBitPattern`.
        unsafe impl<const MIN: $internal, const MAX: $internal> bytemuck::NoUninit
            for $type<MIN, MAX>
        {
        }

        #[cfg(feature = "bytemuck")]
        // Safety: The type is `repr(transparent)` over an integer type, which has no uninitialized
        // bytes. The niche invariant restricts valid values, so this must not imply `AnyBitPattern`.
        unsafe impl<const MIN: $internal, const MAX: $internal> bytemuck::NoUninit
            for $optional_type<MIN, MAX>
        {
        }

        impl $type<0, 0> {
            #[doc = concat!("A ", stringify!($type), " that is always `VALUE`.")]
            #[inline(always)]
            pub const fn exact<const VALUE: $internal>() -> $type<VALUE, VALUE> {
                // Safety: The value is the only one in range.
                unsafe { $type::new_unchecked(VALUE) }
            }
        }

        if_unsigned! { $is_signed
        impl $type<1, { $internal::MAX }> {
            /// Creates a ranged integer from a non-zero value.
            #[inline(always)]
            pub const fn from_nonzero(value: NonZero<$internal>) -> Self {
                // Safety: The value is non-zero, so it is in range.
                unsafe { Self::new_unchecked(value.get()) }
            }

            /// Creates a non-zero value from a ranged integer.
            #[inline(always)]
            pub const fn to_nonzero(self) -> NonZero<$internal> {
                // Safety: The value is in range, so it is non-zero.
                unsafe { NonZero::new_unchecked(self.get()) }
            }
        }}

        impl<const MIN: $internal, const MAX: $internal> $type<MIN, MAX> {
            /// The smallest value that can be represented by this type.
            // Safety: `MIN` is in range by definition.
            pub const MIN: Self = Self::new_static::<MIN>();

            /// The largest value that can be represented by this type.
            // Safety: `MAX` is in range by definition.
            pub const MAX: Self = Self::new_static::<MAX>();

            /// The range of values that can be represented by this type.
            #[inline(always)]
            pub const fn range() -> core::ops::RangeInclusive<$internal> {
                const { assert!(MIN <= MAX); }
                MIN..=MAX
            }

            /// Creates a ranged integer without checking the value.
            ///
            /// # Safety
            ///
            /// The value must be within the range `MIN..=MAX`.
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn new_unchecked(value: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the value is in range.
                unsafe {
                    assert_unchecked(MIN <= value && value <= MAX);
                    Self(Unsafe::new(value))
                }
            }

            /// Returns the value as a primitive type.
            ///
            /// A call to this function will output a hint to the compiler that the value is in
            /// range. In general this will help the optimizer to generate better code, but in edge
            /// cases this may lead to worse code generation. To avoid outputting the hint, you can
            #[doc = concat!("use [`", stringify!($type), "::get_without_hint`].")]
            #[track_caller]
            #[inline(always)]
            pub const fn get(self) -> $internal {
                const { assert!(MIN <= MAX); }
                // Safety: A stored value is always in range.
                unsafe { assert_unchecked(MIN <= *self.0.get() && *self.0.get() <= MAX) };
                *self.0.get()
            }

            /// Returns the value as a primitive type.
            ///
            #[doc = concat!("The returned value is identical to [`", stringify!($type), "::get`].")]
            /// Unlike `get`, no hints are output to the compiler indicating the range that the
            /// value is in. Depending on the scenario, this may with be helpful or harmful to
            /// optimization.
            #[inline(always)]
            pub const fn get_without_hint(self) -> $internal {
                const { assert!(MIN <= MAX); }
                *self.0.get()
            }

            #[track_caller]
            #[inline(always)]
            pub(crate) const fn get_ref(&self) -> &$internal {
                const { assert!(MIN <= MAX); }
                let value = self.0.get();
                // Safety: A stored value is always in range.
                unsafe { assert_unchecked(MIN <= *value && *value <= MAX) };
                value
            }

            /// Creates a ranged integer if the given value is in the range `MIN..=MAX`.
            #[inline(always)]
            pub const fn new(value: $internal) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                if value < MIN || value > MAX {
                    None
                } else {
                    // Safety: The value is in range.
                    Some(unsafe { Self::new_unchecked(value) })
                }
            }

            /// Creates a ranged integer with a statically known value. **Fails to compile** if the
            /// value is not in range.
            #[inline(always)]
            pub const fn new_static<const VALUE: $internal>() -> Self {
                const {
                    assert!(MIN <= VALUE);
                    assert!(VALUE <= MAX);
                }
                // Safety: The value is in range.
                unsafe { Self::new_unchecked(VALUE) }
            }

            /// Creates a ranged integer with the given value, saturating if it is out of range.
            #[inline]
            pub const fn new_saturating(value: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                if value < MIN {
                    Self::MIN
                } else if value > MAX {
                    Self::MAX
                } else {
                    // Safety: The value is in range.
                    unsafe { Self::new_unchecked(value) }
                }
            }

            /// Emit a hint to the compiler that the value is in range.
            ///
            /// In some situations, this can help the optimizer to generate better code. In edge
            /// cases this may lead to **worse** code generation. If you are unsure whether this is
            /// helpful, harmful, or neutral, you should use [`cargo-show-asm`] to compare the
            /// generated assembly.
            ///
            /// Aside from potentially affecting optimization, this function is a no-op.
            ///
            /// [`cargo-show-asm`]: https://crates.io/crates/cargo-show-asm
            #[inline(always)]
            pub const fn emit_range_hint(self) {
                const { assert!(MIN <= MAX); }
                let value = self.0.get();
                // Safety: A stored value is always in range.
                unsafe { assert_unchecked(MIN <= *value && *value <= MAX) };
            }

            /// Expand the range that the value may be in. **Fails to compile** if the new range is
            /// not a superset of the current range.
            #[inline(always)]
            pub const fn expand<const NEW_MIN: $internal, const NEW_MAX: $internal>(
                self,
            ) -> $type<NEW_MIN, NEW_MAX> {
                const {
                    assert!(MIN <= MAX);
                    assert!(NEW_MIN <= NEW_MAX);
                    assert!(NEW_MIN <= MIN);
                    assert!(NEW_MAX >= MAX);
                }
                // Safety: The range is widened.
                unsafe { $type::new_unchecked(self.get()) }
            }

            /// Attempt to narrow the range that the value may be in. Returns `None` if the value
            /// is outside the new range. **Fails to compile** if the new range is not a subset of
            /// the current range.
            #[inline(always)]
            pub const fn narrow<
                const NEW_MIN: $internal,
                const NEW_MAX: $internal,
            >(self) -> Option<$type<NEW_MIN, NEW_MAX>> {
                const {
                    assert!(MIN <= MAX);
                    assert!(NEW_MIN <= NEW_MAX);
                    assert!(NEW_MIN >= MIN);
                    assert!(NEW_MAX <= MAX);
                }
                $type::<NEW_MIN, NEW_MAX>::new(self.get())
            }

            /// Narrow the range that the value may be in. **Fails to compile** if the new range is
            /// not a subset of the current range.
            ///
            /// # Safety
            ///
            /// The value must in the range `NEW_MIN..=NEW_MAX`.
            #[inline(always)]
            pub const unsafe fn narrow_unchecked<
                const NEW_MIN: $internal,
                const NEW_MAX: $internal,
            >(self) -> $type<NEW_MIN, NEW_MAX> {
                const {
                    assert!(MIN <= MAX);
                    assert!(NEW_MIN <= NEW_MAX);
                    assert!(NEW_MIN >= MIN);
                    assert!(NEW_MAX <= MAX);
                }
                // Safety: The caller must ensure that the value is in the new range.
                unsafe { $type::new_unchecked(self.get()) }
            }

            /// Converts a string slice in a given base to an integer.
            ///
            /// The string is expected to be an optional `+` or `-` sign followed by digits. Leading
            /// and trailing whitespace represent an error. Digits are a subset of these characters,
            /// depending on `radix`:
            ///
            /// - `0-9`
            /// - `a-z`
            /// - `A-Z`
            ///
            /// # Panics
            ///
            /// Panics if `radix` is not in the range `2..=36`.
            ///
            /// # Examples
            ///
            /// Basic usage:
            ///
            /// ```rust
            #[doc = concat!("# use deranged::", stringify!($type), ";")]
            #[doc = concat!(
                "assert_eq!(",
                stringify!($type),
                "::<5, 10>::from_str_radix(\"A\", 16), Ok(",
                stringify!($type),
                "::new_static::<10>()));",
            )]
            /// ```
            #[inline]
            pub const fn from_str_radix(src: &str, radix: u32) -> Result<Self, ParseIntError> {
                const { assert!(MIN <= MAX); }
                match $internal::from_str_radix(src, radix) {
                    Ok(value) if value > MAX => {
                        // Safety: `ParseIntError` stores an `IntErrorKind`.
                        Err(parse_int_error_from_kind(IntErrorKind::PosOverflow))
                    }
                    Ok(value) if value < MIN => {
                        // Safety: `ParseIntError` stores an `IntErrorKind`.
                        Err(parse_int_error_from_kind(IntErrorKind::NegOverflow))
                    }
                    // Safety: If the value was out of range, it would have been caught in a
                    // previous arm.
                    Ok(value) => Ok(unsafe { Self::new_unchecked(value) }),
                    Err(e) => Err(e),
                }
            }

            /// Checked integer addition. Computes `self + rhs`, returning `None` if the resulting
            /// value is out of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_add(self, rhs: $internal) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_add(rhs)))
            }

            /// Unchecked integer addition. Computes `self + rhs`, assuming that the result is in
            /// range.
            ///
            /// # Safety
            ///
            /// The result of `self + rhs` must be in the range `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_add(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range.
                unsafe {
                    Self::new_unchecked(self.get().unchecked_add(rhs))
                }
            }

            /// Checked integer addition. Computes `self - rhs`, returning `None` if the resulting
            /// value is out of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_sub(self, rhs: $internal) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_sub(rhs)))
            }

            /// Unchecked integer subtraction. Computes `self - rhs`, assuming that the result is in
            /// range.
            ///
            /// # Safety
            ///
            /// The result of `self - rhs` must be in the range `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_sub(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range.
                unsafe {
                    Self::new_unchecked(self.get().unchecked_sub(rhs))
                }
            }

            /// Checked integer addition. Computes `self * rhs`, returning `None` if the resulting
            /// value is out of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_mul(self, rhs: $internal) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_mul(rhs)))
            }

            /// Unchecked integer multiplication. Computes `self * rhs`, assuming that the result is
            /// in range.
            ///
            /// # Safety
            ///
            /// The result of `self * rhs` must be in the range `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_mul(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range.
                unsafe {
                    Self::new_unchecked(self.get().unchecked_mul(rhs))
                }
            }

            /// Checked integer addition. Computes `self / rhs`, returning `None` if `rhs == 0` or
            /// if the resulting value is out of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_div(self, rhs: $internal) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_div(rhs)))
            }

            /// Unchecked integer division. Computes `self / rhs`, assuming that `rhs != 0` and that
            /// the result is in range.
            ///
            /// # Safety
            ///
            /// `self` must not be zero and the result of `self / rhs` must be in the range
            /// `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_div(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range and that `rhs` is not
                // zero.
                unsafe {
                    Self::new_unchecked(self.get().checked_div(rhs).unwrap_unchecked())
                }
            }

            /// Checked Euclidean division. Computes `self.div_euclid(rhs)`, returning `None` if
            /// `rhs == 0` or if the resulting value is out of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_div_euclid(self, rhs: $internal) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_div_euclid(rhs)))
            }

            /// Unchecked Euclidean division. Computes `self.div_euclid(rhs)`, assuming that
            /// `rhs != 0` and that the result is in range.
            ///
            /// # Safety
            ///
            /// `self` must not be zero and the result of `self.div_euclid(rhs)` must be in the
            /// range `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_div_euclid(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range and that `rhs` is not
                // zero.
                unsafe {
                    Self::new_unchecked(
                        self.get().checked_div_euclid(rhs).unwrap_unchecked()
                    )
                }
            }

            if_unsigned!($is_signed
            /// Remainder. Computes `self % rhs`, statically guaranteeing that the returned value
            /// is in range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline]
            pub const fn rem<const RHS_VALUE: $internal>(
                self,
                rhs: $type<RHS_VALUE, RHS_VALUE>,
            ) -> $type<0, RHS_VALUE> {
                const { assert!(MIN <= MAX); }
                // Safety: The result is guaranteed to be in range due to the nature of remainder on
                // unsigned integers.
                unsafe { $type::new_unchecked(self.get() % rhs.get()) }
            });

            /// Checked integer remainder. Computes `self % rhs`, returning `None` if `rhs == 0` or
            /// if the resulting value is out of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_rem(self, rhs: $internal) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_rem(rhs)))
            }

            /// Unchecked remainder. Computes `self % rhs`, assuming that `rhs != 0` and that the
            /// result is in range.
            ///
            /// # Safety
            ///
            /// `self` must not be zero and the result of `self % rhs` must be in the range
            /// `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_rem(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range and that `rhs` is not
                // zero.
                unsafe {
                    Self::new_unchecked(self.get().checked_rem(rhs).unwrap_unchecked())
                }
            }

            /// Checked Euclidean remainder. Computes `self.rem_euclid(rhs)`, returning `None` if
            /// `rhs == 0` or if the resulting value is out of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_rem_euclid(self, rhs: $internal) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_rem_euclid(rhs)))
            }

            /// Unchecked Euclidean remainder. Computes `self.rem_euclid(rhs)`, assuming that
            /// `rhs != 0` and that the result is in range.
            ///
            /// # Safety
            ///
            /// `self` must not be zero and the result of `self.rem_euclid(rhs)` must be in the
            /// range `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_rem_euclid(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range and that `rhs` is not
                // zero.
                unsafe {
                    Self::new_unchecked(
                        self.get().checked_rem_euclid(rhs).unwrap_unchecked()
                    )
                }
            }

            /// Checked negation. Computes `-self`, returning `None` if the resulting value is out
            /// of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_neg(self) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_neg()))
            }

            /// Unchecked negation. Computes `-self`, assuming that `-self` is in range.
            ///
            /// # Safety
            ///
            /// The result of `-self` must be in the range `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_neg(self) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range.
                // TODO(MSRV 1.93) use `unchecked_neg`
                unsafe { Self::new_unchecked(self.get().checked_neg().unwrap_unchecked()) }
            }

            /// Negation. Computes `self.neg()`, **failing to compile** if the result is not
            /// guaranteed to be in range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline(always)]
            pub const fn neg(self) -> Self {
                const {
                    assert!(MIN <= MAX);
                    if_signed! { $is_signed
                        assert!(MIN != $internal::MIN);
                        assert!(-MIN <= MAX);
                        assert!(-MAX >= MIN);
                    }
                    if_unsigned! { $is_signed
                        assert!(MAX == 0);
                    }
                }
                // Safety: The compiler asserts that the result is in range.
                unsafe { self.unchecked_neg() }
            }

            /// Checked shift left. Computes `self << rhs`, returning `None` if the resulting value
            /// is out of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_shl(self, rhs: u32) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_shl(rhs)))
            }

            /// Unchecked shift left. Computes `self << rhs`, assuming that the result is in range.
            ///
            /// # Safety
            ///
            /// The result of `self << rhs` must be in the range `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_shl(self, rhs: u32) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range.
                unsafe {
                    // TOD(MSRV 1.93) use `unchecked_shl`
                    Self::new_unchecked(self.get().checked_shl(rhs).unwrap_unchecked())
                }
            }

            /// Checked shift right. Computes `self >> rhs`, returning `None` if
            /// the resulting value is out of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_shr(self, rhs: u32) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_shr(rhs)))
            }

            /// Unchecked shift right. Computes `self >> rhs`, assuming that the result is in range.
            ///
            /// # Safety
            ///
            /// The result of `self >> rhs` must be in the range `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_shr(self, rhs: u32) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range.
                unsafe {
                    // TODO(MSRV 1.93) use `unchecked_shr`
                    Self::new_unchecked(self.get().checked_shr(rhs).unwrap_unchecked())
                }
            }

            if_signed!($is_signed
            /// Checked absolute value. Computes `self.abs()`, returning `None` if the resulting
            /// value is out of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_abs(self) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_abs()))
            }

            /// Unchecked absolute value. Computes `self.abs()`, assuming that the result is in
            /// range.
            ///
            /// # Safety
            ///
            /// The result of `self.abs()` must be in the range `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_abs(self) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range.
                unsafe { Self::new_unchecked(self.get().checked_abs().unwrap_unchecked()) }
            }

            /// Absolute value. Computes `self.abs()`, **failing to compile** if the result is not
            /// guaranteed to be in range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline(always)]
            pub const fn abs(self) -> Self {
                const {
                    assert!(MIN <= MAX);
                    assert!(MIN != $internal::MIN);
                    assert!(-MIN <= MAX);
                }
                // <Self as $crate::traits::AbsIsSafe>::ASSERT;
                // Safety: The compiler asserts that the result is in range.
                unsafe { self.unchecked_abs() }
            });

            /// Checked exponentiation. Computes `self.pow(exp)`, returning `None` if the resulting
            /// value is out of range.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn checked_pow(self, exp: u32) -> Option<Self> {
                const { assert!(MIN <= MAX); }
                Self::new(const_try_opt!(self.get().checked_pow(exp)))
            }

            /// Unchecked exponentiation. Computes `self.pow(exp)`, assuming that the result is in
            /// range.
            ///
            /// # Safety
            ///
            /// The result of `self.pow(exp)` must be in the range `MIN..=MAX`.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline(always)]
            pub const unsafe fn unchecked_pow(self, exp: u32) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the result is in range.
                unsafe {
                    Self::new_unchecked(self.get().checked_pow(exp).unwrap_unchecked())
                }
            }

            /// Saturating integer addition. Computes `self + rhs`, saturating at the numeric
            /// bounds.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn saturating_add(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                Self::new_saturating(self.get().saturating_add(rhs))
            }

            /// Saturating integer subtraction. Computes `self - rhs`, saturating at the numeric
            /// bounds.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn saturating_sub(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                Self::new_saturating(self.get().saturating_sub(rhs))
            }

            if_signed!($is_signed
            /// Saturating integer negation. Computes `self - rhs`, saturating at the numeric
            /// bounds.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn saturating_neg(self) -> Self {
                const { assert!(MIN <= MAX); }
                Self::new_saturating(self.get().saturating_neg())
            });

            if_signed!($is_signed
            /// Saturating absolute value. Computes `self.abs()`, saturating at the numeric bounds.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn saturating_abs(self) -> Self {
                const { assert!(MIN <= MAX); }
                Self::new_saturating(self.get().saturating_abs())
            });

            /// Saturating integer multiplication. Computes `self * rhs`, saturating at the numeric
            /// bounds.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn saturating_mul(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                Self::new_saturating(self.get().saturating_mul(rhs))
            }

            /// Saturating integer exponentiation. Computes `self.pow(exp)`, saturating at the
            /// numeric bounds.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            pub const fn saturating_pow(self, exp: u32) -> Self {
                const { assert!(MIN <= MAX); }
                Self::new_saturating(self.get().saturating_pow(exp))
            }

            if_signed! { $is_signed
                /// Returns `true` if the number is positive and `false` if the number is zero or
                /// negative.
                #[inline]
                pub const fn is_positive(self) -> bool {
                    const { assert!(MIN <= MAX); }
                    self.get().is_positive()
                }

                /// Returns `true` if the number is negative and `false` if the number is zero or
                /// positive.
                #[inline]
                pub const fn is_negative(self) -> bool {
                    const { assert!(MIN <= MAX); }
                    self.get().is_negative()
                }
            }

            /// Compute the `rem_euclid` of this type with its unsigned type equivalent
            // Not public because it doesn't match stdlib's "method_unsigned implemented only for signed type" tradition.
            // Also because this isn't implemented for normal types in std.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[track_caller]
            #[inline]
            #[allow(trivial_numeric_casts)] // needed since some casts have to send unsigned -> unsigned to handle signed -> unsigned
            const fn rem_euclid_unsigned(
                rhs: $internal,
                range_len: $unsigned_type
            ) -> $unsigned_type {
                #[allow(unused_comparisons)]
                if rhs >= 0 {
                    (rhs as $unsigned_type) % range_len
                } else {
                    // Let ux refer to an n bit unsigned and ix refer to an n bit signed integer.
                    // Can't write -ux or ux::abs() method. This gets around compilation error.
                    // `wrapping_sub` is to handle rhs = ix::MIN since ix::MIN = -ix::MAX-1
                    let rhs_abs = ($internal::wrapping_sub(0, rhs)) as $unsigned_type;
                    // Largest multiple of range_len <= type::MAX is lowest if range_len * 2 > ux::MAX -> range_len >= ux::MAX / 2 + 1
                    // Also = 0 in mod range_len arithmetic.
                    // Sub from this large number rhs_abs (same as sub -rhs = -(-rhs) = add rhs) to get rhs % range_len
                    // ix::MIN = -2^(n-1) so 0 <= rhs_abs <= 2^(n-1)
                    // ux::MAX / 2 + 1 = 2^(n-1) so this subtraction will always be a >= 0 after subtraction
                    // Thus converting rhs signed negative to equivalent positive value in mod range_len arithmetic
                    ((($unsigned_type::MAX / range_len) * range_len) - (rhs_abs)) % range_len
                }
            }

            /// Wrapping integer addition. Computes `self + rhs`, wrapping around the numeric
            /// bounds.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            #[allow(trivial_numeric_casts)] // needed since some casts have to send unsigned -> unsigned to handle signed -> unsigned
            pub const fn wrapping_add(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                // Forward to internal type's impl if same as type.
                if MIN == $internal::MIN && MAX == $internal::MAX {
                    // Safety: std's wrapping methods match ranged arithmetic when the range is the internal datatype's range.
                    return unsafe { Self::new_unchecked(self.get().wrapping_add(rhs)) }
                }

                let inner = self.get();

                // Won't overflow because of std impl forwarding.
                let range_len = MAX.abs_diff(MIN) + 1;

                // Calculate the offset with proper handling for negative rhs
                let offset = Self::rem_euclid_unsigned(rhs, range_len);

                let greater_vals = MAX.abs_diff(inner);
                // No wrap
                if offset <= greater_vals {
                    // Safety:
                    // if inner >= 0 -> No overflow beyond range (offset <= greater_vals)
                    // if inner < 0: Same as >=0 with caveat:
                    // `(signed as unsigned).wrapping_add(unsigned) as signed` is the same as
                    // `signed::checked_add_unsigned(unsigned).unwrap()` or `wrapping_add_unsigned`
                    // (the difference doesn't matter since it won't overflow),
                    // but unsigned integers don't have either method so it won't compile that way.
                    unsafe { Self::new_unchecked(
                        ((inner as $unsigned_type).wrapping_add(offset)) as $internal
                    ) }
                }
                // Wrap
                else {
                    // Safety:
                    // - offset < range_len by rem_euclid (MIN + ... safe)
                    // - offset > greater_vals from if statement (offset - (greater_vals + 1) safe)
                    //
                    // again using `(signed as unsigned).wrapping_add(unsigned) as signed` = `checked_add_unsigned` trick
                    unsafe { Self::new_unchecked(
                        ((MIN as $unsigned_type).wrapping_add(
                            offset - (greater_vals + 1)
                        )) as $internal
                    ) }
                }
            }

            /// Wrapping integer subtraction. Computes `self - rhs`, wrapping around the numeric
            /// bounds.
            #[must_use = "this returns the result of the operation, without modifying the original"]
            #[inline]
            #[allow(trivial_numeric_casts)] // needed since some casts have to send unsigned -> unsigned to handle signed -> unsigned
            pub const fn wrapping_sub(self, rhs: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                // Forward to internal type's impl if same as type.
                if MIN == $internal::MIN && MAX == $internal::MAX {
                    // Safety: std's wrapping methods match ranged arithmetic when the range is the internal datatype's range.
                    return unsafe { Self::new_unchecked(self.get().wrapping_sub(rhs)) }
                }

                let inner = self.get();

                // Won't overflow because of std impl forwarding.
                let range_len = MAX.abs_diff(MIN) + 1;

                // Calculate the offset with proper handling for negative rhs
                let offset = Self::rem_euclid_unsigned(rhs, range_len);

                let lesser_vals = MIN.abs_diff(inner);
                // No wrap
                if offset <= lesser_vals {
                    // Safety:
                    // if inner >= 0 -> No overflow beyond range (offset <= greater_vals)
                    // if inner < 0: Same as >=0 with caveat:
                    // `(signed as unsigned).wrapping_sub(unsigned) as signed` is the same as
                    // `signed::checked_sub_unsigned(unsigned).unwrap()` or `wrapping_sub_unsigned`
                    // (the difference doesn't matter since it won't overflow below 0),
                    // but unsigned integers don't have either method so it won't compile that way.
                    unsafe { Self::new_unchecked(
                        ((inner as $unsigned_type).wrapping_sub(offset)) as $internal
                    ) }
                }
                // Wrap
                else {
                    // Safety:
                    // - offset < range_len by rem_euclid (MAX - ... safe)
                    // - offset > lesser_vals from if statement (offset - (lesser_vals + 1) safe)
                    //
                    // again using `(signed as unsigned).wrapping_sub(unsigned) as signed` = `checked_sub_unsigned` trick
                    unsafe { Self::new_unchecked(
                        ((MAX as $unsigned_type).wrapping_sub(
                            offset - (lesser_vals + 1)
                        )) as $internal
                    ) }
                }
            }
        }

        impl<const MIN: $internal, const MAX: $internal> $optional_type<MIN, MAX> {
            /// The value used as the niche. Must not be in the range `MIN..=MAX`.
            const NICHE: $internal = match (MIN, MAX) {
                ($internal::MIN, $internal::MAX) => panic!("type has no niche"),
                ($internal::MIN, _) => $internal::MAX,
                (_, _) => $internal::MIN,
            };

            /// An optional ranged value that is not present.
            #[allow(non_upper_case_globals)]
            pub const None: Self = Self(Self::NICHE);

            /// Creates an optional ranged value that is present.
            #[allow(non_snake_case)]
            #[inline(always)]
            pub const fn Some(value: $type<MIN, MAX>) -> Self {
                const { assert!(MIN <= MAX); }
                Self(value.get())
            }

            /// Returns the value as the standard library's [`Option`] type.
            #[inline(always)]
            pub const fn get(self) -> Option<$type<MIN, MAX>> {
                const { assert!(MIN <= MAX); }
                if self.0 == Self::NICHE {
                    None
                } else {
                    // Safety: A stored value that is not the niche is always in range.
                    Some(unsafe { $type::new_unchecked(self.0) })
                }
            }

            /// Creates an optional ranged integer without checking the value.
            ///
            /// # Safety
            ///
            /// The value must be within the range `MIN..=MAX`. As the value used for niche
            /// value optimization is unspecified, the provided value must not be the niche
            /// value.
            #[inline(always)]
            #[track_caller]
            pub const unsafe fn some_unchecked(value: $internal) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The caller must ensure that the value is in range.
                unsafe { assert_unchecked(MIN <= value && value <= MAX) };
                Self(value)
            }

            /// Obtain the inner value of the struct. This is useful for comparisons.
            #[inline(always)]
            pub(crate) const fn inner(self) -> $internal {
                const { assert!(MIN <= MAX); }
                self.0
            }

            /// Obtain the value of the struct as an `Option` of the primitive type.
            ///
            /// A call to this function will output a hint to the compiler that the value is in
            /// range. In general this will help the optimizer to generate better code, but in edge
            /// cases this may lead to worse code generation. To avoid outputting the hint, you can
            #[doc = concat!(
                "use [`", stringify!($optional_type), "::get_primitive_without_hint`]."
            )]
            #[inline(always)]
            pub const fn get_primitive(self) -> Option<$internal> {
                const { assert!(MIN <= MAX); }
                Some(const_try_opt!(self.get()).get())
            }

            /// Obtain the value of the struct as an `Option` of the primitive type.
            ///
            #[doc = concat!(
                "The returned value is identical to [`", stringify!($optional_type), "::",
                "get_primitive`]."
            )]
            /// Unlike `get_primitive`, no hints are output to the compiler indicating the range
            /// that the value is in. Depending on the scenario, this may with be helpful or harmful
            /// to optimization.
            #[inline(always)]
            pub const fn get_primitive_without_hint(self) -> Option<$internal> {
                const { assert!(MIN <= MAX); }
                Some(const_try_opt!(self.get()).get_without_hint())
            }

            /// Returns `true` if the value is the niche value.
            #[inline(always)]
            pub const fn is_none(&self) -> bool {
                const { assert!(MIN <= MAX); }
                self.get().is_none()
            }

            /// Returns `true` if the value is not the niche value.
            #[inline(always)]
            pub const fn is_some(&self) -> bool {
                const { assert!(MIN <= MAX); }
                self.get().is_some()
            }
        }

        impl<const MIN: $internal, const MAX: $internal> fmt::Debug for $type<MIN, MAX> {
            #[inline(always)]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                const { assert!(MIN <= MAX); }
                self.get().fmt(f)
            }
        }

        impl<const MIN: $internal, const MAX: $internal> fmt::Debug for $optional_type<MIN, MAX> {
            #[inline(always)]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                const { assert!(MIN <= MAX); }
                self.get().fmt(f)
            }
        }

        impl<const MIN: $internal, const MAX: $internal> fmt::Display for $type<MIN, MAX> {
            #[inline(always)]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                const { assert!(MIN <= MAX); }
                self.get().fmt(f)
            }
        }

        #[cfg(feature = "powerfmt")]
        impl<
            const MIN: $internal,
            const MAX: $internal,
        > smart_display::SmartDisplay for $type<MIN, MAX> {
            type Metadata = <$internal as smart_display::SmartDisplay>::Metadata;

            #[inline(always)]
            fn metadata(
                &self,
                f: smart_display::FormatterOptions,
            ) -> smart_display::Metadata<'_, Self> {
                const { assert!(MIN <= MAX); }
                self.get_ref().metadata(f).reuse()
            }

            #[inline(always)]
            fn fmt_with_metadata(
                &self,
                f: &mut fmt::Formatter<'_>,
                metadata: smart_display::Metadata<'_, Self>,
            ) -> fmt::Result {
                const { assert!(MIN <= MAX); }
                self.get().fmt_with_metadata(f, metadata.reuse())
            }
        }

        impl<const MIN: $internal, const MAX: $internal> Default for $optional_type<MIN, MAX> {
            #[inline(always)]
            fn default() -> Self {
                const { assert!(MIN <= MAX); }
                Self::None
            }
        }

        impl<const MIN: $internal, const MAX: $internal> AsRef<$internal> for $type<MIN, MAX> {
            #[inline(always)]
            fn as_ref(&self) -> &$internal {
                const { assert!(MIN <= MAX); }
                &self.get_ref()
            }
        }

        impl<const MIN: $internal, const MAX: $internal> Borrow<$internal> for $type<MIN, MAX> {
            #[inline(always)]
            fn borrow(&self) -> &$internal {
                const { assert!(MIN <= MAX); }
                &self.get_ref()
            }
        }

        impl<
            const MIN_A: $internal,
            const MAX_A: $internal,
            const MIN_B: $internal,
            const MAX_B: $internal,
        > PartialEq<$type<MIN_B, MAX_B>> for $type<MIN_A, MAX_A> {
            #[inline(always)]
            fn eq(&self, other: &$type<MIN_B, MAX_B>) -> bool {
                const {
                    assert!(MIN_A <= MAX_A);
                    assert!(MIN_B <= MAX_B);
                }
                self.get() == other.get()
            }
        }

        impl<
            const MIN_A: $internal,
            const MAX_A: $internal,
            const MIN_B: $internal,
            const MAX_B: $internal,
        > PartialEq<$optional_type<MIN_B, MAX_B>> for $optional_type<MIN_A, MAX_A> {
            #[inline(always)]
            fn eq(&self, other: &$optional_type<MIN_B, MAX_B>) -> bool {
                const {
                    assert!(MIN_A <= MAX_A);
                    assert!(MIN_B <= MAX_B);
                }
                self.inner() == other.inner()
            }
        }

        impl<
            const MIN_A: $internal,
            const MAX_A: $internal,
            const MIN_B: $internal,
            const MAX_B: $internal,
        > PartialOrd<$type<MIN_B, MAX_B>> for $type<MIN_A, MAX_A> {
            #[inline(always)]
            fn partial_cmp(&self, other: &$type<MIN_B, MAX_B>) -> Option<Ordering> {
                const {
                    assert!(MIN_A <= MAX_A);
                    assert!(MIN_B <= MAX_B);
                }
                self.get().partial_cmp(&other.get())
            }
        }

        impl<
            const MIN_A: $internal,
            const MAX_A: $internal,
            const MIN_B: $internal,
            const MAX_B: $internal,
        > PartialOrd<$optional_type<MIN_B, MAX_B>> for $optional_type<MIN_A, MAX_A> {
            #[inline]
            fn partial_cmp(&self, other: &$optional_type<MIN_B, MAX_B>) -> Option<Ordering> {
                const {
                    assert!(MIN_A <= MAX_A);
                    assert!(MIN_B <= MAX_B);
                }
                if self.is_none() && other.is_none() {
                    Some(Ordering::Equal)
                } else if self.is_none() {
                    Some(Ordering::Less)
                } else if other.is_none() {
                    Some(Ordering::Greater)
                } else {
                    self.inner().partial_cmp(&other.inner())
                }
            }
        }

        impl<
            const MIN: $internal,
            const MAX: $internal,
        > Ord for $optional_type<MIN, MAX> {
            #[inline]
            fn cmp(&self, other: &Self) -> Ordering {
                const { assert!(MIN <= MAX); }
                if self.is_none() && other.is_none() {
                    Ordering::Equal
                } else if self.is_none() {
                    Ordering::Less
                } else if other.is_none() {
                    Ordering::Greater
                } else {
                    self.inner().cmp(&other.inner())
                }
            }
        }

        impl<const MIN: $internal, const MAX: $internal> fmt::Binary for $type<MIN, MAX> {
            #[inline(always)]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                const { assert!(MIN <= MAX); }
                self.get().fmt(f)
            }
        }

        impl<const MIN: $internal, const MAX: $internal> fmt::LowerHex for $type<MIN, MAX> {
            #[inline(always)]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                const { assert!(MIN <= MAX); }
                self.get().fmt(f)
            }
        }

        impl<const MIN: $internal, const MAX: $internal> fmt::UpperHex for $type<MIN, MAX> {
            #[inline(always)]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                const { assert!(MIN <= MAX); }
                self.get().fmt(f)
            }
        }

        impl<const MIN: $internal, const MAX: $internal> fmt::LowerExp for $type<MIN, MAX> {
            #[inline(always)]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                const { assert!(MIN <= MAX); }
                self.get().fmt(f)
            }
        }

        impl<const MIN: $internal, const MAX: $internal> fmt::UpperExp for $type<MIN, MAX> {
            #[inline(always)]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                const { assert!(MIN <= MAX); }
                self.get().fmt(f)
            }
        }

        impl<const MIN: $internal, const MAX: $internal> fmt::Octal for $type<MIN, MAX> {
            #[inline(always)]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                const { assert!(MIN <= MAX); }
                self.get().fmt(f)
            }
        }

        if_unsigned! { $is_signed
            impl From<NonZero<$internal>> for $type<1, { $internal::MAX }> {
                #[inline(always)]
                fn from(value: NonZero<$internal>) -> Self {
                    Self::from_nonzero(value)
                }
            }

            impl From<$type<1, { $internal::MAX }>> for NonZero<$internal> {
                #[inline(always)]
                fn from(value: $type<1, { $internal::MAX }>) -> Self {
                    value.to_nonzero()
                }
            }
        }

        impl<const MIN: $internal, const MAX: $internal> From<$type<MIN, MAX>> for $internal {
            #[inline(always)]
            fn from(value: $type<MIN, MAX>) -> Self {
                const { assert!(MIN <= MAX); }
                value.get()
            }
        }

        impl<
            const MIN: $internal,
            const MAX: $internal,
        > From<$type<MIN, MAX>> for $optional_type<MIN, MAX> {
            #[inline(always)]
            fn from(value: $type<MIN, MAX>) -> Self {
                const { assert!(MIN <= MAX); }
                Self::Some(value)
            }
        }

        impl<
            const MIN: $internal,
            const MAX: $internal,
        > From<Option<$type<MIN, MAX>>> for $optional_type<MIN, MAX> {
            #[inline(always)]
            fn from(value: Option<$type<MIN, MAX>>) -> Self {
                const { assert!(MIN <= MAX); }
                match value {
                    Some(value) => Self::Some(value),
                    None => Self::None,
                }
            }
        }

        impl<
            const MIN: $internal,
            const MAX: $internal,
        > From<$optional_type<MIN, MAX>> for Option<$type<MIN, MAX>> {
            #[inline(always)]
            fn from(value: $optional_type<MIN, MAX>) -> Self {
                const { assert!(MIN <= MAX); }
                value.get()
            }
        }

        impl<const MIN: $internal, const MAX: $internal> TryFrom<$internal> for $type<MIN, MAX> {
            type Error = TryFromIntError;

            #[inline]
            fn try_from(value: $internal) -> Result<Self, Self::Error> {
                const { assert!(MIN <= MAX); }
                Self::new(value).ok_or(TryFromIntError)
            }
        }

        impl<const MIN: $internal, const MAX: $internal> FromStr for $type<MIN, MAX> {
            type Err = ParseIntError;

            #[inline]
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $type::<MIN, MAX>::from_str_radix(s, 10)
            }
        }

        $(impl<
                const MIN_SRC: $from_internal,
                const MAX_SRC: $from_internal,
                const MIN_DST: $internal,
                const MAX_DST: $internal,
            > From<$from<MIN_SRC, MAX_SRC>> for $type<MIN_DST, MAX_DST>
        {
            #[inline(always)]
            #[allow(trivial_numeric_casts, unused_comparisons)]
            fn from(value: $from<MIN_SRC, MAX_SRC>) -> Self {
                const {
                    assert!(MIN_SRC <= MAX_SRC, "source range is invalid");
                    assert!(MIN_DST <= MAX_DST, "target range is invalid");

                    match ($from_internal::MIN == 0, $internal::MIN == 0) {
                        // unsigned -> unsigned
                        (true, true) => {
                            assert!(
                                MIN_SRC as u128 >= MIN_DST as u128,
                                "minimum value cannot be represented in the target range"
                            );
                            assert!(
                                MAX_SRC as u128 <= MAX_DST as u128,
                                "maximum value cannot be represented in the target range"
                            );
                        }
                        // signed -> signed
                        (false, false) => {
                            assert!(
                                MIN_SRC as i128 >= MIN_DST as i128,
                                "minimum value cannot be represented in the target range"
                            );
                            assert!(
                                MAX_SRC as i128 <= MAX_DST as i128,
                                "maximum value cannot be represented in the target range"
                            );
                        }
                        // unsigned -> signed
                        (true, false) => {
                            assert!(
                                MIN_DST < 0 || MIN_SRC as u128 >= MIN_DST as u128,
                                "minimum value cannot be represented in the target range"
                            );
                            assert!(
                                MAX_DST >= 0
                                    && MAX_SRC as u128 <= i128::MAX as u128
                                    && MAX_SRC as i128 <= MAX_DST as i128,
                                "maximum value cannot be represented in the target range"
                            );
                        }
                        // signed -> unsigned
                        (false, true) => {
                            assert!(
                                MIN_SRC >= 0 && MIN_SRC as u128 >= MIN_DST as u128,
                                "minimum value cannot be represented in the target range"
                            );
                            assert!(
                                MAX_SRC >= 0 && MAX_SRC as u128 <= MAX_DST as u128,
                                "maximum value cannot be represented in the target range"
                            );
                        }
                    }
                }

                // Safety: The source range is a subset of the destination range.
                unsafe { $type::new_unchecked(value.get() as $internal) }
            }
        })+

        /// Non-human-readable formats serialize as the primitive integer. Human-readable formats
        /// serialize as an integer when the whole range is within JSON's safe integer range,
        /// otherwise as an integer string.
        #[cfg(feature = "serde")]
        impl<const MIN: $internal, const MAX: $internal> serde_core::Serialize for $type<MIN, MAX> {
            #[inline(always)]
            fn serialize<S: serde_core::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>
            {
                const { assert!(MIN <= MAX); }
                if !serializer.is_human_readable() {
                    return self.get().serialize(serializer);
                }

                #[allow(trivial_numeric_casts)]
                let json_safe_integer_range = if $is_signed {
                    signed_json_safe_integer_range(MIN as i128, MAX as i128)
                } else {
                    unsigned_json_safe_integer_range(MAX as u128)
                };

                if json_safe_integer_range {
                    self.get().serialize(serializer)
                } else {
                    serializer.collect_str(&self.get())
                }
            }
        }

        /// Non-human-readable formats serialize as the primitive niche-encoded integer.
        /// Human-readable formats serialize present values as an integer when the whole range is
        /// within JSON's safe integer range, otherwise as an integer string.
        #[cfg(feature = "serde")]
        impl<
            const MIN: $internal,
            const MAX: $internal,
        > serde_core::Serialize for $optional_type<MIN, MAX> {
            #[inline(always)]
            fn serialize<S: serde_core::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>
            {
                const { assert!(MIN <= MAX); }
                if serializer.is_human_readable() {
                    self.get().serialize(serializer)
                } else {
                    self.inner().serialize(serializer)
                }
            }
        }

        /// Deserialize an integer string or integer number.
        ///
        /// Human-readable formats accept both strings and numbers, then check that the parsed value
        /// fits the ranged bounds. Non-human-readable formats deserialize as primitive integers.
        #[cfg(feature = "serde")]
        impl<
            'de,
            const MIN: $internal,
            const MAX: $internal,
        > serde_core::Deserialize<'de> for $type<MIN, MAX> {
            #[inline]
            fn deserialize<D: serde_core::Deserializer<'de>>(deserializer: D)
                -> Result<Self, D::Error>
            {
                const { assert!(MIN <= MAX); }

                struct Visitor<const MIN: $internal, const MAX: $internal>;

                impl<const MIN: $internal, const MAX: $internal> serde_core::de::Visitor<'_>
                for Visitor<MIN, MAX> {
                    type Value = $type<MIN, MAX>;

                    #[inline]
                    fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                        #[cfg(feature = "alloc")]
                        {
                            write!(formatter, "an integer or integer string in the range {}..={}", MIN, MAX)
                        }
                        #[cfg(not(feature = "alloc"))]
                        {
                            formatter.write_str("an integer or integer string in the valid range")
                        }
                    }

                    #[inline]
                    fn visit_str<E: serde_core::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                        let internal = value.parse::<$internal>().map_err(|_| {
                            E::invalid_value(serde_core::de::Unexpected::Str(value), &self)
                        })?;
                        Self::Value::new(internal).ok_or_else(|| {
                            E::invalid_value(serde_core::de::Unexpected::Str(value), &self)
                        })
                    }

                    #[inline]
                    fn visit_i64<E: serde_core::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                        let internal = <$internal>::try_from(value).map_err(|_| {
                            E::invalid_value(serde_core::de::Unexpected::Signed(value), &self)
                        })?;
                        Self::Value::new(internal).ok_or_else(|| {
                            E::invalid_value(serde_core::de::Unexpected::Signed(value), &self)
                        })
                    }

                    #[inline]
                    fn visit_u64<E: serde_core::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                        let internal = <$internal>::try_from(value).map_err(|_| {
                            E::invalid_value(serde_core::de::Unexpected::Unsigned(value), &self)
                        })?;
                        Self::Value::new(internal).ok_or_else(|| {
                            E::invalid_value(serde_core::de::Unexpected::Unsigned(value), &self)
                        })
                    }
                }

                if deserializer.is_human_readable() {
                    deserializer.deserialize_any(Visitor::<MIN, MAX>)
                } else {
                    let internal = <$internal>::deserialize(deserializer)?;
                    Self::new(internal)
                        .ok_or_else(|| serde_core::de::Error::custom("integer out of range"))
                }
            }
        }

        /// Deserialize an integer string, integer number, or human-readable null.
        #[cfg(feature = "serde")]
        impl<
            'de,
            const MIN: $internal,
            const MAX: $internal,
        > serde_core::Deserialize<'de> for $optional_type<MIN, MAX> {
            #[inline]
            fn deserialize<D: serde_core::Deserializer<'de>>(deserializer: D)
                -> Result<Self, D::Error>
            {
                const { assert!(MIN <= MAX); }
                if deserializer.is_human_readable() {
                    Ok(
                        Option::<$type<MIN, MAX>>::deserialize(deserializer)?
                            .map_or(Self::None, Self::Some),
                    )
                } else {
                    let internal = <$internal>::deserialize(deserializer)?;
                    if internal == Self::NICHE {
                        Ok(Self::None)
                    } else {
                        $type::new(internal)
                            .map(Self::Some)
                            .ok_or_else(|| serde_core::de::Error::custom("integer out of range"))
                    }
                }
            }
        }

        /// Pure binary LE, transparent to underlying type
        /// (no byte reduction based on niche value optimization based on narrow range).
        #[cfg(feature = "borsh")]
        impl<const MIN: $internal, const MAX: $internal> borsh::BorshSerialize for $type<MIN, MAX> {
            #[inline(always)]
            fn serialize<W: borsh::io::Write>(&self, writer: &mut W) -> borsh::io::Result<()> {
                const { assert!(MIN <= MAX); }
                self.get().serialize(writer)
            }
        }

        /// Pure binary LE, transparent to underlying type
        /// (no byte reduction based on niche value optimization based on narrow range).
        #[cfg(feature = "borsh")]
        impl<const MIN: $internal, const MAX: $internal> borsh::BorshSerialize
            for $optional_type<MIN, MAX>
        {
            #[inline(always)]
            fn serialize<W: borsh::io::Write>(&self, writer: &mut W) -> borsh::io::Result<()> {
                const { assert!(MIN <= MAX); }
                self.inner().serialize(writer)
            }
        }

        /// Pure binary LE, transparent to underlying type
        /// (no byte reduction based on niche value optimization based on narrow range).
        #[cfg(feature = "borsh")]
        impl<const MIN: $internal, const MAX: $internal> borsh::BorshDeserialize
            for $type<MIN, MAX>
        {
            #[inline]
            fn deserialize_reader<R: borsh::io::Read>(reader: &mut R) -> borsh::io::Result<Self> {
                const { assert!(MIN <= MAX); }
                let internal = <$internal>::deserialize_reader(reader)?;
                Self::new(internal).ok_or_else(|| borsh::io::ErrorKind::InvalidData.into())
            }
        }

        /// Pure binary LE, transparent to underlying type
        /// (no byte reduction based on niche value optimization based on narrow range).
        #[cfg(feature = "borsh")]
        impl<const MIN: $internal, const MAX: $internal> borsh::BorshDeserialize
            for $optional_type<MIN, MAX>
        {
            #[inline]
            fn deserialize_reader<R: borsh::io::Read>(reader: &mut R) -> borsh::io::Result<Self> {
                const { assert!(MIN <= MAX); }
                let internal = <$internal>::deserialize_reader(reader)?;
                if internal == Self::NICHE {
                    Ok(Self::None)
                } else {
                    $type::new(internal)
                        .map(Self::Some)
                        .ok_or_else(|| borsh::io::ErrorKind::InvalidData.into())
                }
            }
        }

        /// Pure binary LE, transparent to underlying type
        /// (no byte reduction based on niche value optimization based on narrow range).
        ///
        /// Whole schema is delegated to the underlying type.
        /// Because `deranged::Reanged$type<$min, $max>"` is infinite amount of types to handle,
        /// and `BorshSchema` handles only byte boundaries, not bits nor decimal.
        #[cfg(feature = "borsh_schema")]
        impl<const MIN: $internal, const MAX: $internal> borsh::BorshSchema for $type<MIN, MAX> {
            #[inline]
            fn add_definitions_recursively(
                definitions: &mut borsh::__private::maybestd::collections::BTreeMap<
                    borsh::schema::Declaration,
                    borsh::schema::Definition,
                >,
            ) {
                const { assert!(MIN <= MAX); }
                definitions.insert(
                    Self::declaration(),
                    borsh::schema::Definition::Primitive(size_of::<$internal>() as u8),
                );
            }

            #[inline(always)]
            fn declaration() -> borsh::schema::Declaration {
                const { assert!(MIN <= MAX); }
                <$internal as borsh::BorshSchema>::declaration()
            }
        }

        /// Pure binary LE, transparent to underlying type
        /// (no byte reduction based on niche value optimization based on narrow range).
        #[cfg(feature = "borsh_schema")]
        impl<const MIN: $internal, const MAX: $internal> borsh::BorshSchema
            for $optional_type<MIN, MAX>
        {
            #[inline]
            fn add_definitions_recursively(
                definitions: &mut borsh::__private::maybestd::collections::BTreeMap<
                    borsh::schema::Declaration,
                    borsh::schema::Definition,
                >,
            ) {
                const { assert!(MIN <= MAX); }
                definitions.insert(
                    Self::declaration(),
                    borsh::schema::Definition::Primitive(size_of::<$internal>() as u8),
                );
            }

            #[inline(always)]
            fn declaration() -> borsh::schema::Declaration {
                const { assert!(MIN <= MAX); }
                <$internal as borsh::BorshSchema>::declaration()
            }
        }

        /// If MIN and MAX are within the safe JSON integer range, then schema is integer with min/max,
        /// otherwise string with pattern and max length.
        ///
        /// This schema describes the serialized JSON shape. Deserialization intentionally accepts a
        /// wider human-readable input shape: integer strings or integer numbers, provided they fit
        /// the ranged bounds.
        #[cfg(feature = "schemars")]
        impl<const MIN: $internal, const MAX: $internal> $type<MIN, MAX> {
            #[inline(always)]
            const fn human_type_name_str() -> const_format::StrWriter<[u8; TYPE_NAME_CAPACITY]> {
                const { assert!(MIN <= MAX); }
                let mut type_name = const_format::StrWriter::new([0; TYPE_NAME_CAPACITY]);
                const_format::unwrap!(const_format::writec!(
                    type_name,
                    "{}_{}_{}",
                    stringify!($type),
                    MIN,
                    MAX,
                ));
                type_name
            }

            #[inline]
            fn human_type_name() -> alloc::borrow::Cow<'static, str> {
                let type_name = Self::human_type_name_str();
                alloc::borrow::Cow::Owned(type_name.r().as_str().into())
            }
        }

        #[cfg(feature = "schemars")]
        impl<
            const MIN: $internal,
            const MAX: $internal,
        > schemars::JsonSchema for $type<MIN, MAX> {
            fn inline_schema() -> bool {
                // avoid aliasing all $defs due to same schema name
                true
            }

            fn schema_name() -> alloc::borrow::Cow<'static, str> {
                Self::human_type_name()
            }

            fn json_schema(
                _generator: &mut schemars::SchemaGenerator
            ) -> schemars::Schema {
                #[allow(trivial_numeric_casts)]
                let json_safe_integer_range = if $is_signed {
                    signed_json_safe_integer_range(MIN as i128, MAX as i128)
                } else {
                    unsigned_json_safe_integer_range(MAX as u128)
                };

                let schema = if json_safe_integer_range {
                    let mut schema = schemars::json_schema!({
                        "type": "integer",
                    });

                    if let Ok(minimum) = schemars::_private::serde_json::to_value(MIN) {
                        schema.insert("minimum".into(), minimum);
                    }

                    if let Ok(maximum) = schemars::_private::serde_json::to_value(MAX) {
                        schema.insert("maximum".into(), maximum);
                    }

                    schema
                } else {
                    let maximum_length = usize::max(
                        alloc::string::ToString::to_string(&MIN).len(),
                        alloc::string::ToString::to_string(&MAX).len(),
                    );
                    let mut schema = schemars::json_schema!({
                        "type": "string",
                        "minLength": 1,
                        "pattern": if $is_signed {
                            JSON_SIGNED_INTEGER_PATTERN
                        } else {
                            JSON_UNSIGNED_INTEGER_PATTERN
                        },
                    });

                    if let Ok(max_length) = schemars::_private::serde_json::to_value(maximum_length)
                    {
                        schema.insert("maxLength".into(), max_length);
                    }

                    schema
                };

                schema
            }
        }

        $(
        #[cfg(feature = "prost-types")]
        impl<const MIN: $internal, const MAX: $internal> From<$type<MIN, MAX>>
            for prost_types::Value
        {
            #[inline(always)]
            #[allow(trivial_numeric_casts)]
            fn from(value: $type<MIN, MAX>) -> Self {
                const { assert!(MIN <= MAX); }
                (value.get() as $prost_type).into()
            }
        }

        #[cfg(feature = "prost-types")]
        impl<const MIN: $internal, const MAX: $internal> From<$optional_type<MIN, MAX>>
            for prost_types::Value
        {
            #[inline(always)]
            #[allow(trivial_numeric_casts)]
            fn from(value: $optional_type<MIN, MAX>) -> Self {
                const { assert!(MIN <= MAX); }
                match value.get_primitive() {
                    Some(value) => (value as $prost_type).into(),
                    None => prost_types::value::Kind::NullValue(
                        prost_types::NullValue::NullValue as i32,
                    )
                    .into(),
                }
            }
        }
        )?

        $(
        #[cfg(feature = "sqlx09-pg")]
        impl<const MIN: $internal, const MAX: $internal> sqlx09::Type<sqlx09::Postgres>
            for $type<MIN, MAX>
        {
            #[inline]
            fn type_info() -> sqlx09::postgres::PgTypeInfo {
                <$sql_int as sqlx09::Type<sqlx09::Postgres>>::type_info()
            }
        }

        #[cfg(feature = "sqlx09-pg")]
        impl<const MIN: $internal, const MAX: $internal> sqlx09::postgres::PgHasArrayType
            for $type<MIN, MAX>
        {
            #[inline]
            fn array_type_info() -> sqlx09::postgres::PgTypeInfo {
                <$sql_int as sqlx09::postgres::PgHasArrayType>::array_type_info()
            }
        }

        #[cfg(feature = "sqlx09-pg")]
        impl<const MIN: $internal, const MAX: $internal>
            sqlx09::Encode<'_, sqlx09::Postgres> for $type<MIN, MAX>
        {
            #[inline]
            fn encode_by_ref(
                &self,
                buf: &mut <sqlx09::Postgres as sqlx09::Database>::ArgumentBuffer,
            ) -> Result<
                sqlx09::encode::IsNull,
                alloc::boxed::Box<dyn core::error::Error + 'static + Send + Sync>,
            > {
                const { assert!(MIN <= MAX); }
                assert_sql_storage_range!($is_signed, $type, $sql_int, MIN, MAX);

                let value: $sql_int = self
                    .get()
                    .try_into()
                    .expect(concat!(stringify!($type), " value does not fit SQL storage type"));

                <$sql_int as sqlx09::Encode<sqlx09::Postgres>>::encode_by_ref(&value, buf)
            }
        }

        #[cfg(feature = "sqlx09-pg")]
        impl<'r, const MIN: $internal, const MAX: $internal>
            sqlx09::Decode<'r, sqlx09::Postgres> for $type<MIN, MAX>
        {
            #[inline]
            fn decode(
                value: <sqlx09::Postgres as sqlx09::Database>::ValueRef<'r>,
            ) -> Result<Self, alloc::boxed::Box<dyn core::error::Error + 'static + Send + Sync>> {
                const { assert!(MIN <= MAX); }
                assert_sql_storage_range!($is_signed, $type, $sql_int, MIN, MAX);

                let value = <$sql_int as sqlx09::Decode<sqlx09::Postgres>>::decode(value)?;
                let value: $internal = value.try_into().map_err(|_| TryFromIntError)?;

                Self::new(value).ok_or(TryFromIntError).map_err(Into::into)
            }
        }
        )?

        /// Store as the primitive integer in redb.
        #[cfg(feature = "redb")]
        impl<const MIN: $internal, const MAX: $internal> redb::Value for $type<MIN, MAX> {
            type SelfType<'a>
                = Self
            where
                Self: 'a;
            type AsBytes<'a>
                = [u8; core::mem::size_of::<$internal>()]
            where
                Self: 'a;

            #[inline(always)]
            fn fixed_width() -> Option<usize> {
                Some(core::mem::size_of::<$internal>())
            }

            #[inline]
            fn from_bytes<'a>(data: &'a [u8]) -> Self
            where
                Self: 'a,
            {
                const { assert!(MIN <= MAX); }
                let value = <$internal>::from_le_bytes(
                    data.try_into().expect("invalid redb integer width"),
                );
                Self::new(value).expect("redb value is outside the ranged integer bounds")
            }

            #[inline(always)]
            fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a>
            where
                Self: 'b,
            {
                const { assert!(MIN <= MAX); }
                value.get().to_le_bytes()
            }

            #[inline(always)]
            fn type_name() -> redb::TypeName {
                redb::TypeName::new(stringify!($internal))
            }
        }

        #[cfg(feature = "redb")]
        impl<const MIN: $internal, const MAX: $internal> redb::Key for $type<MIN, MAX> {
            #[inline]
            fn compare(data1: &[u8], data2: &[u8]) -> Ordering {
                <Self as redb::Value>::from_bytes(data1)
                    .cmp(&<Self as redb::Value>::from_bytes(data2))
            }
        }

        /// Store as the niche-encoded primitive integer in redb.
        #[cfg(feature = "redb")]
        impl<const MIN: $internal, const MAX: $internal> redb::Value
            for $optional_type<MIN, MAX>
        {
            type SelfType<'a>
                = Self
            where
                Self: 'a;
            type AsBytes<'a>
                = [u8; core::mem::size_of::<$internal>()]
            where
                Self: 'a;

            #[inline(always)]
            fn fixed_width() -> Option<usize> {
                Some(core::mem::size_of::<$internal>())
            }

            #[inline]
            fn from_bytes<'a>(data: &'a [u8]) -> Self
            where
                Self: 'a,
            {
                const { assert!(MIN <= MAX); }
                let value = <$internal>::from_le_bytes(
                    data.try_into().expect("invalid redb integer width"),
                );
                if value == Self::NICHE {
                    Self::None
                } else {
                    $type::new(value)
                        .map(Self::Some)
                        .expect("redb value is outside the optional ranged integer bounds")
                }
            }

            #[inline(always)]
            fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a>
            where
                Self: 'b,
            {
                const { assert!(MIN <= MAX); }
                value.inner().to_le_bytes()
            }

            #[inline(always)]
            fn type_name() -> redb::TypeName {
                redb::TypeName::new(stringify!($internal))
            }
        }

        #[cfg(feature = "redb")]
        impl<const MIN: $internal, const MAX: $internal> redb::Key for $optional_type<MIN, MAX> {
            #[inline]
            fn compare(data1: &[u8], data2: &[u8]) -> Ordering {
                <Self as redb::Value>::from_bytes(data1)
                    .cmp(&<Self as redb::Value>::from_bytes(data2))
            }
        }

        #[cfg(feature = "rand08")]
        impl<
            const MIN: $internal,
            const MAX: $internal,
        > rand08::distributions::Distribution<$type<MIN, MAX>> for rand08::distributions::Standard {
            #[inline]
            fn sample<R: rand08::Rng + ?Sized>(&self, rng: &mut R) -> $type<MIN, MAX> {
                const { assert!(MIN <= MAX); }
                $type::new(rng.gen_range(MIN..=MAX)).expect("rand failed to generate a valid value")
            }
        }

        if_not_manual_rand_09! {
            [$($($skips)+)?]
            #[cfg(feature = "rand09")]
            impl<
                const MIN: $internal,
                const MAX: $internal,
            > rand09::distr::Distribution<$type<MIN, MAX>> for rand09::distr::StandardUniform {
                #[inline]
                fn sample<R: rand09::Rng + ?Sized>(&self, rng: &mut R) -> $type<MIN, MAX> {
                    const { assert!(MIN <= MAX); }
                    $type::new(rng.random_range(MIN..=MAX)).expect("rand failed to generate a valid value")
                }
            }
        }

        if_not_manual_rand_010! {
            [$($($skips)+)?]
            #[cfg(feature = "rand010")]
            impl<
                const MIN: $internal,
                const MAX: $internal,
            > rand010::distr::Distribution<$type<MIN, MAX>> for rand010::distr::StandardUniform {
                #[inline]
                fn sample<R: rand010::Rng + ?Sized>(&self, rng: &mut R) -> $type<MIN, MAX> {
                    const { assert!(MIN <= MAX); }
                    use rand010::RngExt as _;
                    $type::new(rng.random_range(MIN..=MAX)).expect("rand failed to generate a valid value")
                }
            }
        }

        #[cfg(feature = "rand08")]
        impl<
            const MIN: $internal,
            const MAX: $internal,
        > rand08::distributions::Distribution<$optional_type<MIN, MAX>>
        for rand08::distributions::Standard {
            #[inline]
            fn sample<R: rand08::Rng + ?Sized>(&self, rng: &mut R) -> $optional_type<MIN, MAX> {
                const { assert!(MIN <= MAX); }
                rng.r#gen::<Option<$type<MIN, MAX>>>().into()
            }
        }

        #[cfg(feature = "rand09")]
        impl<
            const MIN: $internal,
            const MAX: $internal,
        > rand09::distr::Distribution<$optional_type<MIN, MAX>>
        for rand09::distr::StandardUniform {
            #[inline]
            fn sample<R: rand09::Rng + ?Sized>(&self, rng: &mut R) -> $optional_type<MIN, MAX> {
                const { assert!(MIN <= MAX); }
                if rng.random() {
                    $optional_type::None
                } else {
                    $optional_type::Some(rng.random::<$type<MIN, MAX>>())
                }
            }
        }

        #[cfg(feature = "rand010")]
        impl<
            const MIN: $internal,
            const MAX: $internal,
        > rand010::distr::Distribution<$optional_type<MIN, MAX>>
        for rand010::distr::StandardUniform {
            #[inline]
            fn sample<R: rand010::Rng + ?Sized>(&self, rng: &mut R) -> $optional_type<MIN, MAX> {
                const { assert!(MIN <= MAX); }
                use rand010::RngExt as _;
                if rng.random() {
                    $optional_type::None
                } else {
                    $optional_type::Some(rng.random::<$type<MIN, MAX>>())
                }
            }
        }

        #[cfg(feature = "num")]
        impl<const MIN: $internal, const MAX: $internal> num_traits::Bounded for $type<MIN, MAX> {
            #[inline(always)]
            fn min_value() -> Self {
                const { assert!(MIN <= MAX); }
                Self::MIN
            }

            #[inline(always)]
            fn max_value() -> Self {
                const { assert!(MIN <= MAX); }
                Self::MAX
            }
        }

        #[cfg(feature = "arbitrary")]
        impl<'a, const MIN: $internal, const MAX: $internal> arbitrary::Arbitrary<'a>
            for $type<MIN, MAX>
        {
            #[inline]
            fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
                const { assert!(MIN <= MAX); }
                let value = u.int_in_range(MIN..=MAX)?;

                // Safety: `int_in_range` only generates values in MIN..=MAX.
                Ok(unsafe { Self::new_unchecked(value) })
            }

            #[inline]
            fn size_hint(depth: usize) -> (usize, Option<usize>) {
                <$internal as arbitrary::Arbitrary<'a>>::size_hint(depth)
            }
        }

        #[cfg(feature = "arbitrary")]
        impl<
            'a,
            const MIN: $internal,
            const MAX: $internal,
        > arbitrary::Arbitrary<'a> for $optional_type<MIN, MAX> {
            #[inline]
            fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
                const { assert!(MIN <= MAX); }
                Option::<$type<MIN, MAX>>::arbitrary(u).map(Self::from)
            }

            #[inline]
            fn size_hint(depth: usize) -> (usize, Option<usize>) {
                Option::<$type<MIN, MAX>>::size_hint(depth)
            }
        }

        #[cfg(feature = "proptest")]
        impl<const MIN: $internal, const MAX: $internal> proptest::arbitrary::Arbitrary
            for $type<MIN, MAX>
        {
            type Parameters = ();
            type Strategy = proptest::strategy::Map<
                core::ops::RangeInclusive<$internal>,
                fn($internal) -> Self,
            >;

            #[inline]
            fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
                const { assert!(MIN <= MAX); }
                use proptest::strategy::Strategy as _;

                (MIN..=MAX).prop_map(|value| {
                    // Safety: The strategy only generates values in MIN..=MAX.
                    unsafe { Self::new_unchecked(value) }
                })
            }
        }

        #[cfg(feature = "proptest")]
        impl<
            const MIN: $internal,
            const MAX: $internal,
        > proptest::arbitrary::Arbitrary for $optional_type<MIN, MAX> {
            type Parameters = ();
            type Strategy = proptest::strategy::Map<
                proptest::option::OptionStrategy<
                    <$type<MIN, MAX> as proptest::arbitrary::Arbitrary>::Strategy,
                >,
                fn(Option<$type<MIN, MAX>>) -> Self,
            >;

            #[inline]
            fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
                const { assert!(MIN <= MAX); }
                use proptest::strategy::Strategy as _;

                proptest::option::of(proptest::arbitrary::any::<$type<MIN, MAX>>())
                    .prop_map(Self::from)
            }
        }

        #[cfg(feature = "quickcheck")]
        impl<const MIN: $internal, const MAX: $internal> quickcheck::Arbitrary for $type<MIN, MAX> {
            #[inline]
            fn arbitrary(g: &mut quickcheck::Gen) -> Self {
                const { assert!(MIN <= MAX); }
                // Safety: The `rem_euclid` call and addition ensure that the value is in range.
                unsafe {
                    Self::new_unchecked($internal::arbitrary(g).rem_euclid(MAX - MIN + 1) + MIN)
                }
            }

            #[inline]
            fn shrink(&self) -> ::alloc::boxed::Box<dyn Iterator<Item = Self>> {
                ::alloc::boxed::Box::new(
                    self.get()
                        .shrink()
                        .filter_map(Self::new)
                )
            }
        }

        #[cfg(feature = "quickcheck")]
        impl<
            const MIN: $internal,
            const MAX: $internal,
        > quickcheck::Arbitrary for $optional_type<MIN, MAX> {
            #[inline]
            fn arbitrary(g: &mut quickcheck::Gen) -> Self {
                const { assert!(MIN <= MAX); }
                Option::<$type<MIN, MAX>>::arbitrary(g).into()
            }

            #[inline]
            fn shrink(&self) -> ::alloc::boxed::Box<dyn Iterator<Item = Self>> {
                ::alloc::boxed::Box::new(self.get().shrink().map(Self::from))
            }
        }
    )*};
}

impl_ranged! {
    RangedU8 {
        mod_name: ranged_u8
        alias: ru8
        internal: u8
        signed: false
        unsigned: u8
        optional: OptionRangedU8
        optional_alias: Option_ru8
        from: [
            RangedU16(u16)
            RangedU32(u32)
            RangedU64(u64)
            RangedU128(u128)
            RangedUsize(usize)
            RangedI8(i8)
            RangedI16(i16)
            RangedI32(i32)
            RangedI64(i64)
            RangedI128(i128)
            RangedIsize(isize)
        ]
        prost_type: u8
        sqlx: i16
    }
    RangedU16 {
        mod_name: ranged_u16
        alias: ru16
        internal: u16
        signed: false
        unsigned: u16
        optional: OptionRangedU16
        optional_alias: Option_ru16
        from: [
            RangedU8(u8)
            RangedU32(u32)
            RangedU64(u64)
            RangedU128(u128)
            RangedUsize(usize)
            RangedI8(i8)
            RangedI16(i16)
            RangedI32(i32)
            RangedI64(i64)
            RangedI128(i128)
            RangedIsize(isize)
        ]
        prost_type: u16
        sqlx: i16
    }
    RangedU32 {
        mod_name: ranged_u32
        alias: ru32
        internal: u32
        signed: false
        unsigned: u32
        optional: OptionRangedU32
        optional_alias: Option_ru32
        from: [
            RangedU8(u8)
            RangedU16(u16)
            RangedU64(u64)
            RangedU128(u128)
            RangedUsize(usize)
            RangedI8(i8)
            RangedI16(i16)
            RangedI32(i32)
            RangedI64(i64)
            RangedI128(i128)
            RangedIsize(isize)
        ]
        prost_type: u32
        sqlx: i32
    }
    RangedU64 {
        mod_name: ranged_u64
        alias: ru64
        internal: u64
        signed: false
        unsigned: u64
        optional: OptionRangedU64
        optional_alias: Option_ru64
        from: [
            RangedU8(u8)
            RangedU16(u16)
            RangedU32(u32)
            RangedU128(u128)
            RangedUsize(usize)
            RangedI8(i8)
            RangedI16(i16)
            RangedI32(i32)
            RangedI64(i64)
            RangedI128(i128)
            RangedIsize(isize)
        ]
        sqlx: i64
    }
    RangedU128 {
        mod_name: ranged_u128
        alias: ru128
        internal: u128
        signed: false
        unsigned: u128
        optional: OptionRangedU128
        optional_alias: Option_ru128
        from: [
            RangedU8(u8)
            RangedU16(u16)
            RangedU32(u32)
            RangedU64(u64)
            RangedUsize(usize)
            RangedI8(i8)
            RangedI16(i16)
            RangedI32(i32)
            RangedI64(i64)
            RangedI128(i128)
            RangedIsize(isize)
        ]
    }
    RangedUsize {
        mod_name: ranged_usize
        alias: rusize
        internal: usize
        signed: false
        unsigned: usize
        optional: OptionRangedUsize
        optional_alias: Option_rusize
        from: [
            RangedU8(u8)
            RangedU16(u16)
            RangedU32(u32)
            RangedU64(u64)
            RangedU128(u128)
            RangedI8(i8)
            RangedI16(i16)
            RangedI32(i32)
            RangedI64(i64)
            RangedI128(i128)
            RangedIsize(isize)
        ]
        manual: [rand_09 rand_010]
    }
    RangedI8 {
        mod_name: ranged_i8
        alias: ri8
        internal: i8
        signed: true
        unsigned: u8
        optional: OptionRangedI8
        optional_alias: Option_ri8
        from: [
            RangedU8(u8)
            RangedU16(u16)
            RangedU32(u32)
            RangedU64(u64)
            RangedU128(u128)
            RangedUsize(usize)
            RangedI16(i16)
            RangedI32(i32)
            RangedI64(i64)
            RangedI128(i128)
            RangedIsize(isize)
        ]
        prost_type: i8
        sqlx: i16
    }
    RangedI16 {
        mod_name: ranged_i16
        alias: ri16
        internal: i16
        signed: true
        unsigned: u16
        optional: OptionRangedI16
        optional_alias: Option_ri16
        from: [
            RangedU8(u8)
            RangedU16(u16)
            RangedU32(u32)
            RangedU64(u64)
            RangedU128(u128)
            RangedUsize(usize)
            RangedI8(i8)
            RangedI32(i32)
            RangedI64(i64)
            RangedI128(i128)
            RangedIsize(isize)
        ]
        prost_type: i16
        sqlx: i16
    }
    RangedI32 {
        mod_name: ranged_i32
        alias: ri32
        internal: i32
        signed: true
        unsigned: u32
        optional: OptionRangedI32
        optional_alias: Option_ri32
        from: [
            RangedU8(u8)
            RangedU16(u16)
            RangedU32(u32)
            RangedU64(u64)
            RangedU128(u128)
            RangedUsize(usize)
            RangedI8(i8)
            RangedI16(i16)
            RangedI64(i64)
            RangedI128(i128)
            RangedIsize(isize)
        ]
        prost_type: i32
        sqlx: i32
    }
    RangedI64 {
        mod_name: ranged_i64
        alias: ri64
        internal: i64
        signed: true
        unsigned: u64
        optional: OptionRangedI64
        optional_alias: Option_ri64
        from: [
            RangedU8(u8)
            RangedU16(u16)
            RangedU32(u32)
            RangedU64(u64)
            RangedU128(u128)
            RangedUsize(usize)
            RangedI8(i8)
            RangedI16(i16)
            RangedI32(i32)
            RangedI128(i128)
            RangedIsize(isize)
        ]
        sqlx: i64
    }
    RangedI128 {
        mod_name: ranged_i128
        alias: ri128
        internal: i128
        signed: true
        unsigned: u128
        optional: OptionRangedI128
        optional_alias: Option_ri128
        from: [
            RangedU8(u8)
            RangedU16(u16)
            RangedU32(u32)
            RangedU64(u64)
            RangedU128(u128)
            RangedUsize(usize)
            RangedI8(i8)
            RangedI16(i16)
            RangedI32(i32)
            RangedI64(i64)
            RangedIsize(isize)
        ]
    }
    RangedIsize {
        mod_name: ranged_isize
        alias: risize
        internal: isize
        signed: true
        unsigned: usize
        optional: OptionRangedIsize
        optional_alias: Option_risize
        from: [
            RangedU8(u8)
            RangedU16(u16)
            RangedU32(u32)
            RangedU64(u64)
            RangedU128(u128)
            RangedUsize(usize)
            RangedI8(i8)
            RangedI16(i16)
            RangedI32(i32)
            RangedI64(i64)
            RangedI128(i128)
        ]
        manual: [rand_09 rand_010]
    }
}

#[cfg(feature = "sqlx09-pg")]
impl<const MIN: u128, const MAX: u128> sqlx09::Type<sqlx09::Postgres> for RangedU128<MIN, MAX> {
    #[inline]
    fn type_info() -> sqlx09::postgres::PgTypeInfo {
        <sqlx09::types::BigDecimal as sqlx09::Type<sqlx09::Postgres>>::type_info()
    }
}

#[cfg(feature = "sqlx09-pg")]
impl<const MIN: u128, const MAX: u128> sqlx09::postgres::PgHasArrayType for RangedU128<MIN, MAX> {
    #[inline]
    fn array_type_info() -> sqlx09::postgres::PgTypeInfo {
        <sqlx09::types::BigDecimal as sqlx09::postgres::PgHasArrayType>::array_type_info()
    }
}

#[cfg(feature = "sqlx09-pg")]
impl<const MIN: u128, const MAX: u128> sqlx09::Encode<'_, sqlx09::Postgres>
    for RangedU128<MIN, MAX>
{
    #[inline]
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx09::Postgres as sqlx09::Database>::ArgumentBuffer,
    ) -> Result<sqlx09::encode::IsNull, alloc::boxed::Box<dyn Error + 'static + Send + Sync>> {
        let value = sqlx09::types::BigDecimal::from(self.get());
        <sqlx09::types::BigDecimal as sqlx09::Encode<sqlx09::Postgres>>::encode_by_ref(&value, buf)
    }
}

#[cfg(feature = "sqlx09-pg")]
impl<'r, const MIN: u128, const MAX: u128> sqlx09::Decode<'r, sqlx09::Postgres>
    for RangedU128<MIN, MAX>
{
    #[inline]
    fn decode(
        value: <sqlx09::Postgres as sqlx09::Database>::ValueRef<'r>,
    ) -> Result<Self, alloc::boxed::Box<dyn Error + 'static + Send + Sync>> {
        let value = <sqlx09::types::BigDecimal as sqlx09::Decode<sqlx09::Postgres>>::decode(value)?;

        if !value.is_integer() {
            return Err(TryFromIntError.into());
        }

        use num_traits::ToPrimitive as _;
        let value = value.to_u128().ok_or(TryFromIntError)?;

        Ok(Self::new(value).ok_or(TryFromIntError)?)
    }
}

#[cfg(feature = "rand09")]
impl<const MIN: usize, const MAX: usize> rand09::distr::Distribution<RangedUsize<MIN, MAX>>
    for rand09::distr::StandardUniform
{
    #[inline]
    fn sample<R: rand09::Rng + ?Sized>(&self, rng: &mut R) -> RangedUsize<MIN, MAX> {
        const {
            assert!(MIN <= MAX);
        }

        #[cfg(target_pointer_width = "16")]
        let value = rng.random_range(MIN as u16..=MAX as u16) as usize;
        #[cfg(target_pointer_width = "32")]
        let value = rng.random_range(MIN as u32..=MAX as u32) as usize;
        #[cfg(target_pointer_width = "64")]
        let value = rng.random_range(MIN as u64..=MAX as u64) as usize;
        #[cfg(not(any(
            target_pointer_width = "16",
            target_pointer_width = "32",
            target_pointer_width = "64"
        )))]
        compile_error("platform has unusual (and unsupported) pointer width");

        RangedUsize::new(value).expect("rand failed to generate a valid value")
    }
}

#[cfg(feature = "rand09")]
impl<const MIN: isize, const MAX: isize> rand09::distr::Distribution<RangedIsize<MIN, MAX>>
    for rand09::distr::StandardUniform
{
    #[inline]
    fn sample<R: rand09::Rng + ?Sized>(&self, rng: &mut R) -> RangedIsize<MIN, MAX> {
        const {
            assert!(MIN <= MAX);
        }

        #[cfg(target_pointer_width = "16")]
        let value = rng.random_range(MIN as i16..=MAX as i16) as isize;
        #[cfg(target_pointer_width = "32")]
        let value = rng.random_range(MIN as i32..=MAX as i32) as isize;
        #[cfg(target_pointer_width = "64")]
        let value = rng.random_range(MIN as i64..=MAX as i64) as isize;
        #[cfg(not(any(
            target_pointer_width = "16",
            target_pointer_width = "32",
            target_pointer_width = "64"
        )))]
        compile_error("platform has unusual (and unsupported) pointer width");

        RangedIsize::new(value).expect("rand failed to generate a valid value")
    }
}

#[cfg(feature = "rand010")]
impl<const MIN: usize, const MAX: usize> rand010::distr::Distribution<RangedUsize<MIN, MAX>>
    for rand010::distr::StandardUniform
{
    #[inline]
    fn sample<R: rand010::Rng + ?Sized>(&self, rng: &mut R) -> RangedUsize<MIN, MAX> {
        const {
            assert!(MIN <= MAX);
        }

        use rand010::RngExt as _;

        #[cfg(target_pointer_width = "16")]
        let value = rng.random_range(MIN as u16..=MAX as u16) as usize;
        #[cfg(target_pointer_width = "32")]
        let value = rng.random_range(MIN as u32..=MAX as u32) as usize;
        #[cfg(target_pointer_width = "64")]
        let value = rng.random_range(MIN as u64..=MAX as u64) as usize;
        #[cfg(not(any(
            target_pointer_width = "16",
            target_pointer_width = "32",
            target_pointer_width = "64"
        )))]
        compile_error("platform has unusual (and unsupported) pointer width");

        RangedUsize::new(value).expect("rand failed to generate a valid value")
    }
}

#[cfg(feature = "rand010")]
impl<const MIN: isize, const MAX: isize> rand010::distr::Distribution<RangedIsize<MIN, MAX>>
    for rand010::distr::StandardUniform
{
    #[inline]
    fn sample<R: rand010::Rng + ?Sized>(&self, rng: &mut R) -> RangedIsize<MIN, MAX> {
        const {
            assert!(MIN <= MAX);
        }

        use rand010::RngExt as _;

        #[cfg(target_pointer_width = "16")]
        let value = rng.random_range(MIN as i16..=MAX as i16) as isize;
        #[cfg(target_pointer_width = "32")]
        let value = rng.random_range(MIN as i32..=MAX as i32) as isize;
        #[cfg(target_pointer_width = "64")]
        let value = rng.random_range(MIN as i64..=MAX as i64) as isize;
        #[cfg(not(any(
            target_pointer_width = "16",
            target_pointer_width = "32",
            target_pointer_width = "64"
        )))]
        compile_error("platform has unusual (and unsupported) pointer width");

        RangedIsize::new(value).expect("rand failed to generate a valid value")
    }
}
