#![no_std]

mod macros;

pub mod board;
pub mod console;
#[cfg(all(feature = "ddr", target_arch = "riscv64", target_os = "none"))]
pub mod ddr;
pub mod eeprom;
mod error;
#[cfg(feature = "ddr")]
pub mod handoff;
#[cfg(feature = "ddr")]
pub mod image;
pub mod io;
mod panic;
pub mod platform;

pub use error::{Error, Result};
pub use platform::Board;
pub use satboot_macros::entry;

#[doc(hidden)]
pub use ufmt as __ufmt;
