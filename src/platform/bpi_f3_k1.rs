//! Banana Pi BPI-F3 K1 board identity.

/// Compile-time hardware identity and boot parameters.
pub const SPEC: crate::board::BoardSpec = crate::board::BPI_F3;

/// Vendor FIT product name.
pub const PRODUCT_NAME: &str = SPEC.product_name;
