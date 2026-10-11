# Architecture

The executable owns boot policy; the board description owns board identity and
memory parameters; the platform owns SoC-specific initialization and storage;
the external HAL/runtime own peripherals and reset entry.

```text
src/main.rs              boot sequence and failure diagnostics
src/board.rs             compile-time BoardSpec and EEPROM compatibility
src/platform/            K1/M1 clocks, UART, PMIC, EEPROM and QSPI setup
src/ddr.rs               fixed DDR firmware ABI and destructive memory checks
src/image/               checked NOR environment, FDT/FIT and DRAM loading
src/handoff.rs           dynamic SBI parameters and final cache/CSR transition
src/console.rs           boot-hart-only UART ownership
macros/                 board-entry adapter around spacemit-rt
xtask/                  host build, ELF extraction and FSBL packing/inspection
link.x                  SRAM/DDR-firmware/stack layout contract
```

## Adding a board

For another board using the same K1/M1 BootROM contract, establish the actual
UART pins, PMIC, memory technology, populated chip selects, training rate and
NOR/FIT product name. Add a `BoardSpec`, a `board-<id>` feature, and the host
tool's board selection. Reuse shared initialization only when these hardware
conditions match. A new SoC requires its own platform/runtime/link contract.

Target builds require exactly one board feature. The BPI-F3 image explicitly
selects `k1-x_deb1` even when the EEPROM omits a product name. Other identities
must match the selected board; a mismatched board or incompatible recorded DDR
parameter stops boot. This check is compatibility validation, not device
authentication.

The first loader reads the vendor NOR partition table and two FITs. It verifies
their shape, CRCs, load regions, entry addresses, staging separation and DTB
before copying any executable image. Physical DRAM address zero is handled with
architecture stores, never a null Rust reference. Only after cache writeback
and readback does the boot hart transfer to SBI. Runtime support for starting
the other harts belongs to the selected SBI firmware.

Future storage adapters should feed the same checked image pipeline. Satellite
update policy (rollback, redundant images, retry budget and recovery) belongs
above storage; it is not yet implemented in this initial port.
