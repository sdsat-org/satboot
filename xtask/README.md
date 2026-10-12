# satboot host tools

The tools build firmware and create files without accessing hardware.

```text
cargo xtask boards
cargo xtask build --board muse-pi-pro
cargo xtask build --board muse-card
cargo xtask build --board muse-pi-pro --diagnostic hello-world
cargo xtask build --board muse-pi-pro --diagnostic eeprom-info
cargo xtask pack --elf target/muse-pi-pro/satboot.elf --keys /path/to/keys
cargo xtask inspect target/muse-pi-pro/satboot-FSBL.bin
cargo xtask linux-fit --board muse-pi-pro --kernel Image --dtb muse-pi-pro-linux.dtb --bootargs "console=ttyS0,115200 earlycon rdinit=/init" --initrd rootfs.cpio.gz --output linux.itb
cargo xtask linux-env --capacity 16M --output env.bin
```

Install the `riscv64imac-unknown-none-elf` target before building firmware.
The build command selects exactly one board feature and enables `ddr`. ELF,
raw BIN and linker MAP outputs are kept in `target/<board>/`. Cargo's intermediate
files are also isolated by board. Packing defaults to `<elf-stem>-FSBL.bin` beside
the input ELF; `--output <path>` selects another destination.

`pack` requires five external PEM files in the supplied directory:

| File | Encoding |
| --- | --- |
| `rsakeypair0_prv.key` | PKCS#1 RSA private key |
| `spl_pubkey_prv.key` | PKCS#1 RSA private key |
| `uboot_pubkey_pub.key` | SubjectPublicKeyInfo public key |
| `kernel_pubkey_pub.key` | SubjectPublicKeyInfo public key |
| `rootfs_pubkey_pub.key` | SubjectPublicKeyInfo public key |

All keys must use RSA-2048 and exponent 65537. Private keys are never embedded in
the tool or printed. The resulting K1 container uses the vendor `secure=0` format.
`inspect` checks structure and both signatures against the public keys embedded
in the container; this detects corruption but does not establish a trusted root
key or prove the board's security state.

Run `cargo clippy --locked -p satboot-xtask --bin satboot-xtask -- -D warnings`
for host-tool static checks. The tools validate ELF layout, image limits and
generated FSBL structure and signatures before producing output.

`linux-fit` prepares `/chosen`, the SBI memory reservation and the SpacemiT RCPU
policy, then builds the inline Linux FIT and validates it with the firmware's
own parser. Input DTB memory banks and device descriptions must match the board.
`linux-env` creates the dedicated NOR layout for `opensbi` and `linux`; capacity
must match the actual flash. See [Linux boot requirements](../docs/linux.md) for
addresses, image limits and DTB requirements. Neither command flashes hardware.

## Provenance

`src/elf2bin.rs` and `src/fsbl.rs` are adapted from the corresponding host tools
in [RustSBI's spacemit-hal](https://atomgit.com/rustsbi/spacemit-hal), commit
`69529cce3021c7b6d50aa004cfcdd3d3db79b3bf`, licensed
under MIT OR MulanPSL-2.0. The upstream MIT copyright notice is retained in
these sources and the repository's `LICENSE-MIT`. Changes here replace compiled-in
development keys with explicit key-directory input and add key validation.

The binary protocol follows SpacemiT's
[K1 configuration](https://github.com/spacemit-com/uboot-2022.10/blob/1fa1ca64e9705a3650bcc7c21f6666949290830f/configs/k1_defconfig)
and [FSBL description](https://github.com/spacemit-com/uboot-2022.10/blob/1fa1ca64e9705a3650bcc7c21f6666949290830f/board/spacemit/k1-x/configs/fsbl.json).
