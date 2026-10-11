#![no_std]
#![no_main]

use satboot::{Board, entry, println};

#[entry]
fn main(b: Board) {
    println!("satboot {} - {}", env!("CARGO_PKG_VERSION"), b.spec.name);
    println!("UART, clocks, EEPROM bus and PMIC initialized; DDR not trained.");
}
