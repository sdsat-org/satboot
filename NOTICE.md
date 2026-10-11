# Source provenance

The initial SpacemiT implementation and host image tooling are adapted from
[`rustsbi/spacemit-hal`](https://atomgit.com/rustsbi/spacemit-hal), commit
`69529cce3021c7b6d50aa004cfcdd3d3db79b3bf`:

- `examples/rot-bootloader/src/`, `macros/`, `link.x`, and `build.rs`;
- `xtask/src/elf2bin.rs` and `xtask/src/fsbl.rs`.

The original MIT and MulanPSL-2.0 license texts are retained in `LICENSE-MIT`
and `LICENSE-MULAN`. Satboot adds explicit board selection and identity checks,
standalone dependency pinning, build commands, user-supplied signing keys, and
additional host verification. HAL, PMIC, runtime and QSPI NOR remain dependencies
on that exact public source revision.

The unmodified DDR binary is from
[`spacemit-com/spacemit-firmware`](https://github.com/spacemit-com/spacemit-firmware),
commit `5969642a5b46fee5ba7f21ca54c3129bff7bb049`, file `k1/v0.2/ddr_fw.bin`:

```text
size:   36248 bytes
sha256: 4ca729dae39580fe6d52ff6a1ead95fd18cd6394c942cc4a323882ccf2840148
```

Its license is retained in `DDR-FIRMWARE-LICENSE`. The target build rejects a
different firmware hash. Distribution of a linked satboot image must retain
that firmware license along with the source-code notices.

Hardware and binary-format references are listed in `docs/k1-boot.md`.
