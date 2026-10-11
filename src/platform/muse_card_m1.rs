//! MUSE Card M1 board identity.

/// Compile-time hardware identity and boot parameters.
pub const SPEC: crate::board::BoardSpec = crate::board::MUSE_CARD;

/// EEPROM product name.
pub const PRODUCT_NAME: &str = SPEC.product_name;
