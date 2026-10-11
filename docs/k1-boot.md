# SpacemiT K1 boot contract

## Entry and board assumptions

Enter from K1 BootROM on RV64 M-mode hart 0 at `0xc0801000`. Secondary harts and
DMA are stopped. This is a cold-boot SRAM program, not an S-mode U-Boot `go`
application. The pinned `spacemit-rt/k1-bootrom` startup establishes the stack,
initializes data/BSS and X60 cache/coherency state, and parks on return.

The board setup expects the BootROM's 24 MHz reference and locked 2.4576 GHz
PLL1, and validates their existing configuration rather than retuning the PLL.
UART0 must already have its clock gates/reset established by BootROM. It uses
GPIO68/69 AF2 and the K1 XScale-style UART at `0xd4017000`.

I2C2 GPIO84/85 accesses the board's 24x02 EEPROM. I2C8 accesses SPM8821; BUCK1
is set to 1.05 V and BUCK1/BUCK3 are enabled before a 10 ms settling delay.
The initial board profiles use LPDDR4X, two chip selects, 2400 MT/s and 80-ohm
TX ODT. EEPROM values that contradict these settings are rejected.

## SRAM layout

| Region | Start | End (exclusive) |
| --- | --- | --- |
| DDR metadata | `0xc0800000` | `0xc0800600` |
| Resident code/data before DDR blob | `0xc0801000` | `0xc082d000` |
| DDR firmware and workspace | `0xc082d000` | `0xc0837000` |
| BSS | `0xc0837000` | `0xc0839000` |
| Stack including 256-byte lower guard | `0xc0839000` | `0xc0840000` |

The v0.2 DDR blob is loaded directly at its execution address and invoked using
the integer C ABI `(controller_base, chip_selects, data_rate, puts)` with
controller base `0xc0000000`. Its undocumented raw return value is printed;
the decision to proceed depends on destructive pattern/address-line tests in
unused DRAM and the stack guard. All test memory must be disposable at entry.

The linker rejects collisions among resident code, firmware workspace, BSS and
stack. The raw-image limit is `0x34f00`; the complete FSBL limit is `0x36000`.

The release profile disables LTO: LLVM 22 can lose RISC-V ISA features while
inspecting module-level startup assembly during LTO, rejecting valid multiply
and atomic instructions. See [rust-lang/rust#111637](https://github.com/rust-lang/rust/issues/111637).
The linked image is still optimized for size and checked against SRAM limits.

## Images and handoff

The K1 container has a 4 KiB prefix, a payload padded to 32 bytes, and a 256-byte
RSA signature. Its two `AIHD` headers are at `0x100` and `0xfe0`. The packer uses
explicit PEM inputs and verifies the generated container. Structural/signature
inspection against embedded keys does not establish a trusted root key.

The NOR loader uses polling QSPI and a CRC-checked environment at `0x60000`.
It locates `opensbi` and `uboot` partitions, accepts the supported in-tree FIT
encoding and CRC32 hashes, and selects the board's explicit product description.
The SBI FIT may contain a compatible RustSBI dynamic firmware even though the
vendor FIT OS tag is `opensbi`.

SBI occupies the low 2 MiB, U-Boot/DTB must stay below 16 MiB, and source FITs
are staged from 16 MiB. Loading checks addresses and overlaps and reserves the
low 128 MiB during boot. DRAM physically starts at zero on K1.

The final M-mode handoff uses `a0=0`, `a1=dtb`, `a2=DynamicInfo` (magic
`0x4942534f`, version 2, S-mode next stage, boot hart 0). Data is flushed and
instructions synchronized before transfer. Secondary-hart startup is delegated
to SBI. No flash erase/write or automatic hardware download is part of a build.

## Primary sources

- [Vendor K1 memory configuration](https://github.com/spacemit-com/uboot-2022.10/blob/1fa1ca64e9705a3650bcc7c21f6666949290830f/configs/k1_defconfig)
- [Vendor FSBL format](https://github.com/spacemit-com/uboot-2022.10/blob/1fa1ca64e9705a3650bcc7c21f6666949290830f/board/spacemit/k1-x/configs/fsbl.json)
- [Vendor pin control](https://github.com/spacemit-com/uboot-2022.10/blob/1fa1ca64e9705a3650bcc7c21f6666949290830f/arch/riscv/dts/k1-x_pinctrl.dtsi)
- [Official DDR v0.2 integration ABI](https://lists.denx.de/pipermail/u-boot/2026-February/609826.html)
- [SpacemiT DDR firmware](https://github.com/spacemit-com/spacemit-firmware/tree/5969642a5b46fee5ba7f21ca54c3129bff7bb049/k1/v0.2)
- [Vendor dynamic SBI handoff](https://github.com/spacemit-com/uboot-2022.10/blob/1fa1ca64e9705a3650bcc7c21f6666949290830f/common/spl/spl_opensbi.c)
- [U-Boot SRAM download test guide](https://docs.u-boot.org/en/latest/board/spacemit/k1-spl.html)
