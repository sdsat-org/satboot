# Initial validation

Validated on Windows x86-64 with Rust 1.98.1 on 2026-10-11. The default board
is MUSE Pi Pro. These are software checks; this revision has not been run on
a board. UART output, DDR training and the complete NOR-to-SBI boot sequence
still require a hardware test.

## Automated checks

- `cargo test --locked --workspace --lib`: 35 tests passed, including board
  selection/identity, EEPROM parsing, console handling, FIT CRCs, load bounds,
  product selection and malformed images.
- `cargo test --locked -p satboot-xtask`: 11 tests passed, including ELF layout,
  FSBL packing/signatures, corruption rejection and CLI parsing.
- Host library and xtask Clippy checks passed with `-D warnings`.
- RV64 Clippy passed for all three MUSE Pi Pro binaries with `-D warnings`.

The localized MSVC linker prints an informational import-library creation
message when building the host proc macro. Rust labels this as `linker_messages`;
it does not indicate a failed build or a firmware diagnostic.

## Cross compilation and image checks

`cargo xtask build --board <board>` passed for `muse-pi-pro`, `bpi-f3` and
`muse-card`. Each command validated the linked ELF and generated BIN and MAP
outputs. MUSE Pi Pro's `hello-world` and `eeprom-info` diagnostics also built.

| Image | Raw BIN bytes | Development FSBL bytes |
| --- | ---: | ---: |
| MUSE Pi Pro satboot | 216,472 | 220,832 |
| MUSE Pi Pro hello-world | 13,619 | 17,984 |
| MUSE Pi Pro eeprom-info | 24,411 | 28,768 |
| BPI-F3 satboot | 216,472 | Not packed |
| MUSE Card satboot | 216,472 | Not packed |

LLVM inspection confirmed an ELF entry of `0xc0801000`, resident code/data
ending at `0xc080db80`, the 36,248-byte DDR blob at `0xc082d000`, BSS at
`0xc0837000..0xc08372b8` and a stack ending at `0xc0840000` for MUSE Pi Pro.
The BIN includes the gap up to the fixed DDR firmware address. The raw and
packed images fit the BootROM limits documented in [the boot contract](k1-boot.md).

The three local MUSE Pi Pro FSBL files were packed with the vendor's
[public development keys](https://github.com/spacemit-com/uboot-2022.10/tree/676971d3a61f2583dfd53d8f7dfbc9607cea74b7/board/spacemit/k1-x/configs/key).
`cargo xtask inspect` accepted their structure and embedded-key signatures.
These are `secure=0` development images. No keys are committed to satboot.

CI defines Windows, Linux and macOS host checks and Linux builds for all three
boards. Local validation alone does not establish results on the other hosts.
No hardware download, flash write or production key provisioning was performed.
