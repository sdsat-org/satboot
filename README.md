# satboot

Bootloader for satellite firmware and system software.

satboot performs early board initialization and loads the next firmware stage.
The first boot path targets **SpacemiT MUSE Pi Pro / K1-M1 platform** and takes the place
of U-Boot SPL: BootROM -> satboot -> dynamic SBI firmware -> U-Boot proper.
The next-stage images are read from an existing vendor-compatible SPI NOR layout.

## Status

The implementation includes SRAM startup through `spacemit-rt`, UART0, EEPROM
identity checks, SPM8821 power setup, DDR training and memory checks, QSPI NOR
reads, FIT validation, and the dynamic SBI handoff. This satboot revision has not
yet been boot-tested on hardware. It is an initial development release.

| Build configuration | SoC | Status |
| --- | --- | --- |
| `muse-pi-pro` | M1 (K1-compatible platform) | Primary port; hardware validation pending |
| `bpi-f3` | K1 | Shared platform port; hardware validation pending |
| `muse-card` | M1 | Shared platform port; hardware validation pending |

SD/eMMC boot, arbitrary FIT encodings, recovery/update policy, and authenticated
payload verification are not implemented. FIT CRC32 checks detect corruption;
they do not authenticate software. DDR initialization uses the unmodified,
hash-pinned SpacemiT v0.2 training firmware included with its redistribution
license. UART polling and vendor DDR training do not have bounded recovery.

## Build

Install Git and Rust with rustup, then run from the repository root:

```text
cargo xtask boards
cargo xtask build --board muse-pi-pro
```

`rust-toolchain.toml` selects Rust 1.98.1 and the RV64 bare-metal target. Cargo
fetches the pinned public SpacemiT HAL/runtime source on the first build; no
neighboring checkout is required. The build produces:

```text
target/muse-pi-pro/satboot.elf
target/muse-pi-pro/satboot.bin
target/muse-pi-pro/satboot.map
```

Optional just shortcuts are `just build muse-pi-pro`, `just hello muse-pi-pro`, and
`just eeprom muse-pi-pro`. Diagnostics initialize board devices but do not train DDR
or hand off to another image.

## Create a BootROM container

The raw binary is not a BootROM FSBL container. Pack the ELF with an explicitly
supplied K1 signing-key directory:

```text
cargo xtask pack --elf target/muse-pi-pro/satboot.elf --keys /path/to/k1/configs/key
cargo xtask inspect target/muse-pi-pro/satboot-FSBL.bin
```

The directory must contain `rsakeypair0_prv.key`, `spl_pubkey_prv.key`,
`uboot_pubkey_pub.key`, `kernel_pubkey_pub.key`, and `rootfs_pubkey_pub.key`, in
the vendor PEM formats. See the pinned
[vendor FSBL configuration](https://github.com/spacemit-com/uboot-2022.10/blob/1fa1ca64e9705a3650bcc7c21f6666949290830f/board/spacemit/k1-x/configs/fsbl.json).
No private key is embedded in or distributed with this repository. The packer
implements the vendor `secure=0` development container, not a production key
provisioning or authenticated-boot policy.

For a MUSE Pi Pro in BootROM USB download mode, an explicit SRAM-only test is:

```text
fastboot stage target/muse-pi-pro/satboot-FSBL.bin
fastboot continue
```

Connect UART0 at 115200 8N1. An existing compatible NOR installation must provide
the environment, SBI FIT and U-Boot/DTB FIT. The supplied build/pack commands do
not access a board or write flash. See [the boot contract](docs/k1-boot.md) before
hardware testing.

## Development

```text
cargo test --locked --workspace --lib
cargo test --locked -p satboot-xtask
cargo fmt --all -- --check
cargo clippy --locked --workspace --lib -- -D warnings
cargo clippy --locked -p satboot-xtask --all-targets -- -D warnings
```

See [architecture and adding boards](docs/architecture.md),
[validation results](docs/validation.md), and [source provenance](NOTICE.md).
Source code is available under
`MIT OR MulanPSL-2.0`; the bundled DDR firmware has its own
[license](DDR-FIRMWARE-LICENSE).
