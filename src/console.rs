//! Boot-hart console. Uninitialized, reentrant and other-hart writes are skipped.

use core::convert::Infallible;
use embedded_io::Write;
use spacemit_hal::uart::BlockingUart;
use spin::{Mutex, Once};

struct Console<W> {
    owner: usize,
    writer: Mutex<W>,
}

// SAFETY: Moving this private wrapper does not access the UART; lock still
// requires its original hart. Dropping BlockingUart performs no hardware access.
unsafe impl Send for Console<BlockingUart<'static>> {}
// SAFETY: Only the owner hart can acquire the non-Send UART guard; try_lock
// excludes overlapping interrupt/formatting access.
unsafe impl Sync for Console<BlockingUart<'static>> {}

static CONSOLE: Once<Console<BlockingUart<'static>>> = Once::new();

pub(crate) type Guard<'a, W> = spin::MutexGuard<'a, W>;

impl<W> Console<W> {
    #[inline]
    fn install(slot: &Once<Self>, hart: usize, writer: W) -> Result<(), W> {
        let mut writer = Some(writer);
        slot.call_once(|| Self {
            owner: hart,
            writer: Mutex::new(writer.take().unwrap()),
        });
        writer.map_or(Ok(()), Err)
    }

    #[inline]
    fn lock(&self, hart: usize) -> Option<Guard<'_, W>> {
        if self.owner != hart {
            return None;
        }
        self.writer.try_lock()
    }
}

#[inline]
fn current_hart() -> Option<usize> {
    #[cfg(all(
        target_os = "none",
        any(target_arch = "riscv32", target_arch = "riscv64")
    ))]
    {
        Some(riscv::register::mhartid::read())
    }
    #[cfg(not(all(
        target_os = "none",
        any(target_arch = "riscv32", target_arch = "riscv64")
    )))]
    {
        None
    }
}

pub(crate) fn install(uart: BlockingUart<'static>) -> Result<(), ()> {
    let hart = current_hart().ok_or(())?;
    Console::install(&CONSOLE, hart, uart).map_err(|_| ())
}

#[inline]
pub(crate) fn lock() -> Option<Guard<'static, BlockingUart<'static>>> {
    CONSOLE.get()?.lock(current_hart()?)
}

/// A locked, infallible formatting sink; unavailable consoles discard output.
#[doc(hidden)]
pub struct Writer {
    uart: Option<Guard<'static, BlockingUart<'static>>>,
}

impl ufmt::uWrite for Writer {
    type Error = Infallible;

    #[inline]
    fn write_str(&mut self, text: &str) -> Result<(), Infallible> {
        if let Some(uart) = &mut self.uart {
            for &byte in text.as_bytes() {
                uart.write_byte(byte);
            }
        }
        Ok(())
    }
}

/// Formats and flushes one message without dynamic dispatch.
#[doc(hidden)]
#[inline(never)]
pub fn _print(format: impl FnOnce(&mut Writer) -> Result<(), Infallible>) {
    let mut writer = Writer { uart: lock() };
    let _ = format(&mut writer);
    if let Some(uart) = &mut writer.uart {
        uart.flush();
    }
}

/// Formats cold-path diagnostics using the original `core::fmt` traits.
#[doc(hidden)]
#[cold]
#[inline(never)]
pub fn _eprint(args: core::fmt::Arguments<'_>) {
    if let Some(mut uart) = lock() {
        let _ = uart.write_fmt(args);
        uart.flush();
    }
}
