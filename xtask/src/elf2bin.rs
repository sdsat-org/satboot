// SPDX-License-Identifier: MIT OR MulanPSL-2.0
// Copyright 2020-2022 RustSBI Team
// Adapted from spacemit-hal/xtask/src/elf2bin.rs; see ../README.md.

use anyhow::{Result, ensure};
use object::read::elf::{ElfFile64, ProgramHeader};
use object::{Object, ObjectSection, SectionFlags, SectionKind};

pub const BASE: u64 = 0xc080_1000;
// Vendor k1_defconfig CONFIG_SPL_SIZE_LIMIT, including appended DDR firmware/DTB.
pub const LIMIT: usize = 0x34f00;

// Like allwinner-hal/rfel: copy allocated, file-backed sections and preserve address gaps.
pub fn convert(bytes: &[u8]) -> Result<Vec<u8>> {
    let elf = object::File::parse(bytes)?;
    ensure!(
        elf.format() == object::BinaryFormat::Elf
            && elf.architecture() == object::Architecture::Riscv64
            && elf.is_little_endian()
            && elf.kind() == object::ObjectKind::Executable,
        "expected a little-endian RISC-V64 executable ELF"
    );
    ensure!(elf.entry() == BASE, "expected K1/M1 entry at {BASE:#x}");
    let headers = ElfFile64::<object::Endianness>::parse(bytes)?;
    for segment in headers.elf_program_headers() {
        let endian = headers.endian();
        if segment.p_type(endian) == object::elf::PT_LOAD {
            ensure!(
                segment.p_vaddr(endian) == segment.p_paddr(endian),
                "relocated ELF sections are not supported"
            );
        }
    }
    let mut sections = Vec::new();
    for section in elf.sections() {
        if matches!(section.flags(), SectionFlags::Elf { sh_flags, .. } if sh_flags.intersects(object::elf::SHF_ALLOC))
            && section.kind() != SectionKind::UninitializedData
            && section.size() != 0
        {
            let data = section.data()?;
            ensure!(
                data.len() as u64 == section.size(),
                "unsupported compressed section"
            );
            sections.push((section.address(), data));
        }
    }
    sections.sort_by_key(|(address, _)| *address);
    ensure!(
        sections.first().map(|s| s.0) == Some(BASE),
        "missing SRAM entry section"
    );
    let mut raw = Vec::new();
    for (address, data) in sections {
        let offset = address
            .checked_sub(BASE)
            .ok_or_else(|| anyhow::anyhow!("section below SRAM"))?;
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("section overflow"))?;
        ensure!(
            end <= LIMIT as u64 && offset >= raw.len() as u64,
            "overlapping or oversized SRAM sections"
        );
        raw.resize(end as usize, 0);
        raw[offset as usize..].copy_from_slice(data);
    }
    Ok(raw)
}
