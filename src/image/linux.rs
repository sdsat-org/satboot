//! Linux RISC-V Image v0.2 validation for the K1 S-mode boot contract.
// Header: https://docs.kernel.org/arch/riscv/boot-image-header.html
// Placement: https://docs.kernel.org/arch/riscv/boot.html

use super::{Error, Result, Segment};

const HEADER_SIZE: usize = 64;
const TEXT_OFFSET: usize = 0x0020_0000;

/// Returns the complete runtime footprint, not merely the on-disk byte length.
pub(super) fn validate(data: &[u8], load: usize, entry: usize) -> Result<Segment> {
    if data.len() < HEADER_SIZE {
        return Err(Error::Format);
    }
    if data.starts_with(b"\x7fELF") || data.starts_with(b"\x1f\x8b") {
        return Err(Error::Unsupported);
    }
    if &data[56..60] != b"RSC\x05" {
        return Err(Error::Format);
    }
    let u64_at = |offset| u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    let version = u32::from_le_bytes(data[32..36].try_into().unwrap());
    if version != 2 || u64_at(24) != 0 {
        return Err(Error::Unsupported);
    }
    if data[36..48].iter().any(|&byte| byte != 0) {
        return Err(Error::Format);
    }
    // text_offset identifies the RV64 S-mode Image convention. Physical placement
    // can move to another PMD boundary to preserve board-specific reservations.
    // Linux v6.6 setup_vm uses the actual &_start for kernel_map.phys_addr:
    // https://github.com/torvalds/linux/blob/v6.6/arch/riscv/mm/init.c
    if u64_at(8) != TEXT_OFFSET as u64 || !load.is_multiple_of(TEXT_OFFSET) || entry != load {
        return Err(Error::Address);
    }
    let size = usize::try_from(u64_at(16)).map_err(|_| Error::Bounds)?;
    if size < data.len() {
        return Err(Error::Bounds);
    }
    let footprint = Segment {
        address: load,
        size,
    };
    footprint.end()?;
    Ok(footprint)
}
