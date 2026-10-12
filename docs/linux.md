# Direct Linux boot

The boot chain is `BootROM -> satboot -> dynamic SBI firmware -> Linux`.
satboot loads the kernel and device tree. SBI provides the resident machine-mode
services and enters the Linux Image in supervisor mode. No U-Boot program or
U-Boot boot script is involved.

## Inputs

- An uncompressed, little-endian RV64 S-mode Linux `Image` with version 0.2 boot
  header and `text_offset=2 MiB`. ELF, XIP, M-mode kernels and `Image.gz` are not
  accepted. Its header must declare the full runtime size, including BSS.
- The matching **Linux** DTB for the exact board and installed memory variant.
  The reduced DTB from an old U-Boot FIT is not a Linux hardware description.
  The root uses two address cells and two size cells. `memory` nodes must cover
  at least physical `0..128 MiB` and accurately describe all installed banks.
  The K1 vendor layout places RAM above the first 2 GiB at physical 4 GiB;
  do not describe the intervening MMIO hole as RAM.
- Dynamic SBI firmware qualified for K1/M1, wrapped in the supported SBI FIT
  (`type=firmware`, `os=opensbi`, `arch=riscv`, no compression, inline data and
  CRC32). Compatible RustSBI firmware may use the same protocol and FIT tag.
  Its complete runtime footprint must fit `0..2 MiB`.
- Optionally a raw or compressed initramfs archive. It is copied unchanged;
  Linux must include the appropriate decompressor and initramfs support.

The Linux configuration and DTB must provide the correct CPU ISA and 24 MHz
timebase, compatible timer support (SBI TIME or Sstc), and built-in drivers for
the chosen console and root filesystem. Accurate memory capacity, MAC addresses
and any driver-specific board properties formerly supplied by another loader
remain responsibilities of the board DTB producer. satboot does not guess them.

## Build a Linux FIT

```text
cargo xtask linux-fit --board muse-pi-pro --kernel Image --dtb muse-pi-pro-linux.dtb --bootargs "console=ttyS0,115200 earlycon rdinit=/init" --initrd rootfs.cpio.gz --output linux.itb
```

For an external root filesystem, omit `--initrd` and specify suitable `root=`,
`rootwait` and filesystem arguments instead of `rdinit=/init`. Loading the kernel
from NOR does not prevent Linux from mounting its root filesystem from eMMC,
SD or another device using its own drivers.

The packer updates `/chosen/bootargs`, sets exclusive-end `linux,initrd-start`
and `linux,initrd-end`, or removes stale initrd properties when no archive is
supplied. It creates or extends an SBI `no-map` reservation at physical zero.
All other memory banks, firmware reservations, device properties, phandles and
the FDT reservation map are retained, with this board-specific exception:

**SpacemiT RCPU remoteproc is disabled.** The vendor `spacemit,k1-x-rproc`
consumer can access a heap at `1..3 MiB`, overlapping the SBI runtime contract.
The packer sets that consumer and its referenced SBI-overlapping reserved pools
to `status = "disabled"`. Other RCPU pools remain reserved. RCPU support needs
a separately qualified firmware and memory layout; `no-map` alone does not stop
a remote processor from writing SBI memory.

Static reservations must not overlap loaded images, the DTB workspace, staging,
or SBI (except its own reservations at zero). Dynamic reserved pools such as
CMA require explicit `alloc-ranges` entirely inside RAM at or above 128 MiB,
with enough room for their size and alignment. Unbounded dynamic allocation is
rejected. These checks apply both in the packer and on the board.

| Object | Packer address | Contract |
| --- | ---: | --- |
| SBI | Specified by SBI FIT, below 2 MiB | All resident memory below 2 MiB |
| Linux | `0x01000000` (16 MiB) | 2 MiB aligned; full runtime size must not overlap other objects |
| DTB | `0x03000000` (48 MiB) | Input at most 256 KiB; 1 MiB reserved for SBI fixups |
| Optional initramfs | `0x03400000` (52 MiB) | Ends no later than 64 MiB |
| FIT staging | `0x04000000` (64 MiB) | 512 KiB SBI FIT + at most 32 MiB Linux FIT |

The kernel's header offset remains 2 MiB while its physical entry is 16 MiB.
RISC-V Linux uses its actual running address and permits this PMD-aligned
placement. The higher address avoids the vendor board's low RCPU carveouts.
The kernel clears its own BSS; satboot reserves that space before copying any
DTB or initramfs into DRAM. The SBI implementation must keep its final DTB
within the 1 MiB fixup workspace.

The FIT contains inline `kernel`, `fdt` and optional `ramdisk` images with CRC32
checksums and a configuration description matching the selected board's EEPROM
product identifier. CRC detects corruption; it does not authenticate a payload.
The packer calls the same validator as the firmware before writing its output.

## NOR layout

```text
cargo xtask linux-env --capacity 16M --output env.bin
```

Use the actual flash capacity. This command produces a fresh 64 KiB environment
file with a CRC-protected `mtdparts` entry; it does not preserve unrelated
variables from an existing installation and does not write hardware.

| Partition | Offset | Size / content |
| --- | ---: | --- |
| bootinfo | `0x00000` | 128 KiB, board's existing BootROM metadata |
| fsbl | `0x20000` | 256 KiB, packed satboot FSBL |
| env | `0x60000` | 64 KiB, generated `env.bin` |
| opensbi | `0x70000` | 512 KiB, qualified dynamic SBI FIT |
| linux | `0xf0000` | Remaining NOR, generated `linux.itb` |

The Linux FIT must fit both its actual NOR partition and the 32 MiB staging
limit. A 16 MiB flash has only `0xf10000` bytes after offset `0xf0000`; a larger
kernel/initramfs needs a smaller build or a future storage adapter. Replacing
an existing layout requires explicit board provisioning; a stock `uboot`
partition and its standalone payload are rejected. Build and pack commands
never provision, erase or flash a board automatically.

## Validation boundary

Host tests cover parsing, addresses, kernel runtime overlap, DTB/bootargs/initrd
consistency, RCPU policy and firmware reservations. Compiling an image does not
demonstrate a Linux boot on MUSE Pi Pro. UART, DDR, SBI services and reaching
Linux userspace still require hardware validation.

Primary references: [Linux boot requirements](https://docs.kernel.org/arch/riscv/boot.html),
[Image header](https://docs.kernel.org/arch/riscv/boot-image-header.html),
[physical kernel mapping](https://github.com/torvalds/linux/blob/v6.6/arch/riscv/mm/init.c#L995),
[vendor RCPU layout](https://github.com/spacemit-com/linux-6.6/blob/k1-bl-v2.2.y/arch/riscv/boot/dts/spacemit/k1-x.dtsi),
[vendor MUSE Pi Pro DT](https://github.com/spacemit-com/linux-6.6/blob/k1-bl-v2.2.y/arch/riscv/boot/dts/spacemit/k1-x_MUSE-Pi-Pro.dts).
