//! Checks a prepared Linux DTB before any payload is copied to DRAM.
//!
//! This verifies a boot contract, not the truth of the supplied hardware description.
//! Memory banks and device properties must come from the actual board configuration.
//!
//! References:
//! - https://docs.kernel.org/arch/riscv/boot.html
//! - https://devicetree-specification.readthedocs.io/en/latest/chapter3-devicenodes.html
//! - https://devicetree-specification.readthedocs.io/en/latest/chapter5-flattened-format.html

use super::{
    Error, Result, Segment,
    fdt::{Fdt, Node, text, word},
};

const DRAM_END: u64 = 0x0800_0000;
const SBI_END: u64 = 0x0020_0000;
const STAGING: Region = Region {
    start: 0x0400_0000,
    end: 0x0608_0000,
};
const MAX_RANGES: usize = 64;
const MAX_BOOTARGS: usize = 4096;

#[derive(Clone, Copy, Default)]
struct Region {
    start: u64,
    end: u64,
}

impl Region {
    fn new(start: u64, size: u64) -> Result<Self> {
        if size == 0 {
            return Err(Error::Bounds);
        }
        Ok(Self {
            start,
            end: start.checked_add(size).ok_or(Error::Bounds)?,
        })
    }

    fn segment(segment: Segment) -> Result<Self> {
        Self::new(segment.address as u64, segment.size as u64)
    }

    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

struct Targets {
    kernel: Region,
    dtb: Region,
    initrd: Option<Region>,
}

struct Memory {
    ranges: [Region; MAX_RANGES],
    count: usize,
}

impl Memory {
    fn contains(&self, requested: Region) -> bool {
        let mut covered = requested.start;
        loop {
            let mut extended = covered;
            for region in &self.ranges[..self.count] {
                if region.start <= covered && region.end > extended {
                    extended = region.end;
                }
            }
            if extended >= requested.end {
                return true;
            }
            if extended == covered {
                return false;
            }
            covered = extended;
        }
    }
}

impl Targets {
    fn check_reservation(&self, reservation: Region) -> Result<()> {
        if reservation.overlaps(self.kernel)
            || reservation.overlaps(self.dtb)
            || reservation.overlaps(STAGING)
            || self
                .initrd
                .is_some_and(|initrd| reservation.overlaps(initrd))
        {
            return Err(Error::Overlap);
        }
        Ok(())
    }
}

/// Validates the final DTB contract without allocation or hardware access.
///
/// `kernel` includes the image header's effective runtime footprint, including
/// bytes beyond the on-flash Image. All intervals use an exclusive upper bound.
/// The FIT loader retains responsibility for image-to-image overlap checks.
///
/// Dynamic reservations require explicit allocation ranges wholly within declared
/// RAM above the loader's low 128 MiB workspace. Disabled reservations are ignored.
pub(super) fn validate(
    bytes: &[u8],
    kernel: Segment,
    dtb: Segment,
    initrd: Option<Segment>,
) -> Result<()> {
    let root = Fdt::root(bytes)?;
    cells(root)?;
    if dtb.address & 7 != 0 || dtb.size != bytes.len() {
        return Err(Error::Address);
    }
    let targets = Targets {
        kernel: Region::segment(kernel)?,
        // SBI may expand the tree in place; raw FIT length alone does not cover
        // its writable DTB workspace or protect that area from reserved owners.
        dtb: Region::new(dtb.address as u64, super::DTB_WORKSPACE as u64)?,
        initrd: initrd.map(Region::segment).transpose()?,
    };
    for region in [Some(targets.kernel), Some(targets.dtb), targets.initrd]
        .into_iter()
        .flatten()
    {
        if region.start < SBI_END || region.end > DRAM_END {
            return Err(Error::Address);
        }
    }
    let memory = memory(root)?;
    chosen(root.child("chosen")?, targets.initrd)?;
    reserved(root.child("reserved-memory")?, &targets, &memory)?;
    reservations(bytes, &targets)
}

fn cells(node: Node<'_>) -> Result<()> {
    if word(node.required("#address-cells")?)? != 2 || word(node.required("#size-cells")?)? != 2 {
        return Err(Error::Unsupported);
    }
    Ok(())
}

fn available(node: Node<'_>) -> Result<bool> {
    Ok(matches!(
        node.property("status")?.map(text).transpose()?,
        None | Some("okay" | "ok")
    ))
}

fn number(bytes: &[u8]) -> Result<u64> {
    Ok(u64::from_be_bytes(
        bytes.try_into().map_err(|_| Error::Format)?,
    ))
}

fn region(tuple: &[u8]) -> Result<Region> {
    Region::new(number(&tuple[..8])?, number(&tuple[8..])?)
}

fn tuples(bytes: &[u8]) -> Result<core::slice::Iter<'_, [u8; 16]>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(16) {
        return Err(Error::Format);
    }
    Ok(bytes.as_chunks::<16>().0.iter())
}

fn memory(root: Node<'_>) -> Result<Memory> {
    let mut ranges = [Region::default(); MAX_RANGES];
    let mut count = 0;
    for child in root.children() {
        let child = child?;
        let memory_name = child.name == "memory" || child.name.starts_with("memory@");
        let device_type = child.property("device_type")?.map(text).transpose()?;
        if !memory_name {
            // Linux identifies memory using device_type. Do not let it consume
            // additional memory hidden behind a node name this validator skips.
            if device_type == Some("memory") {
                return Err(Error::Format);
            }
            continue;
        }
        if !available(child)? {
            continue;
        }
        if device_type != Some("memory") {
            return Err(Error::Format);
        }
        // Linux can prefer this property to reg; this boot path accepts a final,
        // unrestricted board memory map rather than a crash-kernel memory view.
        if child.property("linux,usable-memory")?.is_some() {
            return Err(Error::Unsupported);
        }
        for tuple in tuples(child.required("reg")?)? {
            if count == ranges.len() {
                return Err(Error::Unsupported);
            }
            let next = region(tuple)?;
            if ranges[..count].iter().any(|other| next.overlaps(*other)) {
                return Err(Error::Overlap);
            }
            ranges[count] = next;
            count += 1;
        }
    }
    // Accept adjacent banks in any order; do not turn an MMIO hole into RAM.
    let memory = Memory { ranges, count };
    if !memory.contains(Region {
        start: 0,
        end: DRAM_END,
    }) {
        return Err(Error::Address);
    }
    Ok(memory)
}

fn chosen(node: Node<'_>, initrd: Option<Region>) -> Result<()> {
    let bootargs = node.required("bootargs")?;
    if bootargs.len() > MAX_BOOTARGS {
        return Err(Error::Bounds);
    }
    text(bootargs)?;
    if node.property("linux,usable-memory-range")?.is_some() {
        return Err(Error::Unsupported);
    }
    let start = node.property("linux,initrd-start")?;
    let end = node.property("linux,initrd-end")?;
    match (initrd, start, end) {
        (None, None, None) => Ok(()),
        (Some(initrd), Some(start), Some(end))
            if number(start)? == initrd.start && number(end)? == initrd.end =>
        {
            Ok(())
        }
        _ => Err(Error::Address),
    }
}

fn flag(node: Node<'_>, name: &str) -> Result<bool> {
    match node.property(name)? {
        Some([]) => Ok(true),
        Some(_) => Err(Error::Format),
        None => Ok(false),
    }
}

fn reserved(node: Node<'_>, targets: &Targets, memory: &Memory) -> Result<()> {
    cells(node)?;
    if !node.required("ranges")?.is_empty() || !available(node)? {
        return Err(Error::Unsupported);
    }
    let mut sbi_protected = false;
    for child in node.children() {
        let child = child?;
        if !available(child)? {
            continue;
        }
        let no_map = flag(child, "no-map")?;
        let reusable = flag(child, "reusable")?;
        if no_map && reusable {
            return Err(Error::Format);
        }
        let Some(reg) = child.property("reg")? else {
            dynamic_reservation(child, memory)?;
            continue;
        };
        // With both reg and size, DTSpec makes this a static allocation and
        // ignores size.
        for tuple in tuples(reg)? {
            let reservation = region(tuple)?;
            targets.check_reservation(reservation)?;
            if reservation.start < SBI_END {
                // A legacy SBI carveout can start at zero and be smaller than
                // the complete reservation. Other owners, notably the vendor
                // RCPU heap starting at 1 MiB, cannot coexist with this SBI area.
                if reservation.start != 0 {
                    return Err(Error::Overlap);
                }
                if !no_map || reusable {
                    return Err(Error::Unsupported);
                }
                sbi_protected |= reservation.start == 0 && reservation.end >= SBI_END;
            }
        }
    }
    if !sbi_protected {
        return Err(Error::Missing);
    }
    Ok(())
}

fn dynamic_reservation(node: Node<'_>, memory: &Memory) -> Result<()> {
    let size = number(node.property("size")?.ok_or(Error::Unsupported)?)?;
    if size == 0 {
        return Err(Error::Bounds);
    }
    let alignment = node
        .property("alignment")?
        .map(number)
        .transpose()?
        .unwrap_or(1);
    if !alignment.is_power_of_two() {
        return Err(Error::Unsupported);
    }
    let ranges = node.property("alloc-ranges")?.ok_or(Error::Unsupported)?;
    let mut fits = false;
    for (index, tuple) in tuples(ranges)?.enumerate() {
        if index == MAX_RANGES {
            return Err(Error::Unsupported);
        }
        let allocation = region(tuple)?;
        if allocation.start < DRAM_END || !memory.contains(allocation) {
            return Err(Error::Address);
        }
        let aligned = allocation
            .start
            .checked_add(alignment - 1)
            .ok_or(Error::Bounds)?
            & !(alignment - 1);
        fits |= aligned
            .checked_add(size)
            .is_some_and(|end| end <= allocation.end);
    }
    if !fits {
        return Err(Error::Bounds);
    }
    Ok(())
}

fn reservations(bytes: &[u8], targets: &Targets) -> Result<()> {
    // Fdt::root has already checked the map's bounds, terminator, alignment and
    // separation from the other DTB sections. Still use checked slices here.
    let mut position = word(bytes.get(16..20).ok_or(Error::Format)?)? as usize;
    let mut ranges = [Region::default(); MAX_RANGES];
    let mut count = 0;
    loop {
        let end = position.checked_add(16).ok_or(Error::Bounds)?;
        let tuple = bytes.get(position..end).ok_or(Error::Bounds)?;
        position = end;
        if tuple.iter().all(|&byte| byte == 0) {
            return Ok(());
        }
        if count == ranges.len() {
            return Err(Error::Unsupported);
        }
        let reservation = region(tuple)?;
        targets.check_reservation(reservation)?;
        if ranges[..count]
            .iter()
            .any(|other| reservation.overlaps(*other))
        {
            return Err(Error::Overlap);
        }
        ranges[count] = reservation;
        count += 1;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{vec, vec::Vec};

    const KERNEL: Segment = Segment {
        address: 0x0020_0000,
        size: 0x0040_0000,
    };
    const INITRD: Segment = Segment {
        address: 0x0300_0000,
        size: 0x1000,
    };

    struct Tree {
        structure: Vec<u8>,
        strings: Vec<u8>,
        reservations: Vec<u8>,
    }

    impl Tree {
        fn new() -> Self {
            Self {
                structure: Vec::new(),
                strings: Vec::new(),
                reservations: Vec::new(),
            }
        }

        fn begin(&mut self, name: &str) {
            self.structure.extend(1u32.to_be_bytes());
            self.structure.extend(name.bytes().chain([0]));
            self.pad();
        }

        fn end(&mut self) {
            self.structure.extend(2u32.to_be_bytes());
        }

        fn pad(&mut self) {
            while !self.structure.len().is_multiple_of(4) {
                self.structure.push(0);
            }
        }

        fn prop(&mut self, name: &str, bytes: &[u8]) {
            self.structure.extend(3u32.to_be_bytes());
            self.structure.extend((bytes.len() as u32).to_be_bytes());
            self.structure
                .extend((self.strings.len() as u32).to_be_bytes());
            self.strings.extend(name.bytes().chain([0]));
            self.structure.extend(bytes);
            self.pad();
        }

        fn cells(&mut self) {
            self.prop("#address-cells", &2u32.to_be_bytes());
            self.prop("#size-cells", &2u32.to_be_bytes());
        }

        fn reservation(&mut self, start: u64, size: u64) {
            self.reservations.extend(start.to_be_bytes());
            self.reservations.extend(size.to_be_bytes());
        }

        fn finish(mut self) -> Vec<u8> {
            self.structure.extend(9u32.to_be_bytes());
            self.reservations.extend([0; 16]);
            let structure = 40 + self.reservations.len();
            let strings = structure + self.structure.len();
            let total = strings + self.strings.len();
            let fields = [
                0xd00d_feed,
                total as u32,
                structure as u32,
                strings as u32,
                40,
                17,
                16,
                0,
                self.strings.len() as u32,
                self.structure.len() as u32,
            ];
            let mut blob = Vec::new();
            for field in fields {
                blob.extend(field.to_be_bytes());
            }
            blob.extend(self.reservations);
            blob.extend(self.structure);
            blob.extend(self.strings);
            blob
        }
    }

    fn reg(start: u64, size: u64) -> [u8; 16] {
        let mut result = [0; 16];
        result[..8].copy_from_slice(&start.to_be_bytes());
        result[8..].copy_from_slice(&size.to_be_bytes());
        result
    }

    fn tree(
        memory_regions: &[[u8; 16]],
        bootargs: &[u8],
        initrd: Option<Segment>,
        reserve: impl FnOnce(&mut Tree),
        header: impl FnOnce(&mut Tree),
    ) -> Vec<u8> {
        let mut tree = Tree::new();
        header(&mut tree);
        tree.begin("");
        tree.cells();
        tree.begin("memory@0");
        tree.prop("device_type", b"memory\0");
        tree.prop("reg", &memory_regions.concat());
        tree.end();
        tree.begin("chosen");
        tree.prop("bootargs", bootargs);
        if let Some(initrd) = initrd {
            tree.prop("linux,initrd-start", &(initrd.address as u64).to_be_bytes());
            tree.prop(
                "linux,initrd-end",
                &((initrd.address + initrd.size) as u64).to_be_bytes(),
            );
        }
        tree.end();
        tree.begin("reserved-memory");
        tree.cells();
        tree.prop("ranges", &[]);
        reserve(&mut tree);
        tree.end();
        tree.end();
        tree.finish()
    }

    fn sbi(tree: &mut Tree) {
        tree.begin("sbi@0");
        tree.prop("reg", &reg(0, SBI_END));
        tree.prop("no-map", &[]);
        tree.end();
    }

    fn check(bytes: &[u8], initrd: Option<Segment>) -> Result<()> {
        validate(
            bytes,
            KERNEL,
            Segment {
                address: 0x0200_0000,
                size: bytes.len(),
            },
            initrd,
        )
    }

    #[test]
    fn prepared_board_dtb_with_or_without_initrd_is_accepted() {
        for initrd in [None, Some(INITRD)] {
            let dtb = tree(&[reg(0, DRAM_END)], b"console=ttyS0\0", initrd, sbi, |_| {});
            assert_eq!(check(&dtb, initrd), Ok(()));
        }
        let dtb = tree(&[reg(0, DRAM_END)], b"\0", None, sbi, |_| {});
        assert_eq!(check(&dtb, None), Ok(()));
    }

    #[test]
    fn memory_coverage_allows_banks_and_preserves_high_address_holes() {
        let dtb = tree(
            &[
                reg(0x0400_0000, 0x0400_0000),
                reg(0, 0x0400_0000),
                reg(0x1_0000_0000, 0x4_0000_0000),
            ],
            b"\0",
            None,
            sbi,
            |_| {},
        );
        assert_eq!(check(&dtb, None), Ok(()));
        for memory in [
            vec![reg(0, DRAM_END - 1)],
            vec![reg(0, 0x0100_0000), reg(0x0200_0000, DRAM_END)],
            vec![reg(0, DRAM_END), reg(0x0010_0000, DRAM_END)],
        ] {
            assert!(check(&tree(&memory, b"\0", None, sbi, |_| {}), None).is_err());
        }
    }

    #[test]
    fn initrd_requires_matching_exclusive_endpoints() {
        let present = tree(&[reg(0, DRAM_END)], b"\0", Some(INITRD), sbi, |_| {});
        assert_eq!(check(&present, None), Err(Error::Address));
        assert_eq!(
            check(
                &present,
                Some(Segment {
                    size: INITRD.size - 1,
                    ..INITRD
                })
            ),
            Err(Error::Address)
        );
        let absent = tree(&[reg(0, DRAM_END)], b"\0", None, sbi, |_| {});
        assert_eq!(check(&absent, Some(INITRD)), Err(Error::Address));
    }

    #[test]
    fn bootargs_are_a_bounded_single_string() {
        for bad in [
            &b"unterminated"[..],
            &b"first\0second\0"[..],
            &vec![b'a'; MAX_BOOTARGS + 1],
        ] {
            assert!(check(&tree(&[reg(0, DRAM_END)], bad, None, sbi, |_| {}), None).is_err());
        }
        let mut maximum = vec![b'a'; MAX_BOOTARGS];
        maximum[MAX_BOOTARGS - 1] = 0;
        assert_eq!(
            check(
                &tree(&[reg(0, DRAM_END)], &maximum, None, sbi, |_| {}),
                None
            ),
            Ok(())
        );
    }

    #[test]
    fn sbi_needs_the_complete_non_reusable_no_map_region() {
        for (size, no_map, reusable, disabled) in [
            (SBI_END - 1, true, false, false),
            (SBI_END, false, false, false),
            (SBI_END, true, true, false),
            (SBI_END, true, false, true),
        ] {
            let dtb = tree(
                &[reg(0, DRAM_END)],
                b"\0",
                None,
                |tree| {
                    tree.begin("sbi@0");
                    tree.prop("reg", &reg(0, size));
                    if no_map {
                        tree.prop("no-map", &[]);
                    }
                    if reusable {
                        tree.prop("reusable", &[]);
                    }
                    if disabled {
                        tree.prop("status", b"disabled\0");
                    }
                    tree.end();
                },
                |_| {},
            );
            assert!(check(&dtb, None).is_err());
        }
    }

    #[test]
    fn board_reservations_must_not_cover_images_or_staging() {
        for start in [
            KERNEL.address as u64 + KERNEL.size as u64 - 1,
            0x0200_0000,
            INITRD.address as u64,
            STAGING.start,
            STAGING.end - 1,
        ] {
            let dtb = tree(
                &[reg(0, DRAM_END)],
                b"\0",
                Some(INITRD),
                |tree| {
                    sbi(tree);
                    tree.begin("reserved@1000");
                    tree.prop("reg", &reg(start, 1));
                    tree.end();
                },
                |_| {},
            );
            assert_eq!(check(&dtb, Some(INITRD)), Err(Error::Overlap));
        }
        let dtb = tree(
            &[reg(0, DRAM_END)],
            b"\0",
            None,
            |tree| {
                sbi(tree);
                tree.begin("board-pool@80000000");
                tree.prop("reg", &reg(0x8000_0000, 0x1000));
                tree.prop("reusable", &[]);
                tree.end();
            },
            |_| {},
        );
        assert_eq!(check(&dtb, None), Ok(()));
    }

    #[test]
    fn dtb_expansion_workspace_remains_exclusive() {
        let dtb = tree(
            &[reg(0, DRAM_END)],
            b"\0",
            None,
            |tree| {
                sbi(tree);
                tree.begin("board-reserved@2001000");
                tree.prop("reg", &reg(0x0200_1000, 0x1000));
                tree.end();
            },
            |_| {},
        );
        assert!(dtb.len() < 0x1000);
        assert_eq!(check(&dtb, None), Err(Error::Overlap));
        let dtb = tree(&[reg(0, DRAM_END)], b"\0", None, sbi, |tree| {
            tree.reservation(0x0200_1000, 0x1000)
        });
        assert!(dtb.len() < 0x1000);
        assert_eq!(check(&dtb, None), Err(Error::Overlap));
        // The raw tree fits below 128 MiB; its expanded workspace does not.
        let dtb = tree(&[reg(0, DRAM_END)], b"\0", None, sbi, |_| {});
        assert_eq!(
            validate(
                &dtb,
                KERNEL,
                Segment {
                    address: DRAM_END as usize - 0x1000,
                    size: dtb.len()
                },
                None,
            ),
            Err(Error::Address),
        );
    }

    #[test]
    fn dynamic_reservations_need_explicit_resolution() {
        let dtb = tree(
            &[reg(0, DRAM_END)],
            b"\0",
            None,
            |tree| {
                sbi(tree);
                tree.begin("dynamic-pool");
                tree.prop("size", &0x1000u64.to_be_bytes());
                tree.end();
            },
            |_| {},
        );
        assert_eq!(check(&dtb, None), Err(Error::Unsupported));
    }

    #[test]
    fn another_enabled_owner_cannot_claim_part_of_sbi() {
        let dtb = tree(
            &[reg(0, DRAM_END)],
            b"\0",
            None,
            |tree| {
                sbi(tree);
                tree.begin("rcpu-heap@100000");
                tree.prop("reg", &reg(0x0010_0000, 0x1000));
                tree.prop("no-map", &[]);
                tree.end();
            },
            |_| {},
        );
        assert_eq!(check(&dtb, None), Err(Error::Overlap));
        let dtb = tree(
            &[reg(0, DRAM_END)],
            b"\0",
            None,
            |tree| {
                sbi(tree);
                tree.begin("legacy-sbi@0");
                tree.prop("reg", &reg(0, 0x0008_0000));
                tree.prop("no-map", &[]);
                tree.end();
                tree.begin("rcpu-heap@100000");
                tree.prop("reg", &reg(0x0010_0000, 0x1000));
                tree.prop("no-map", &[]);
                tree.prop("status", b"disabled\0");
                tree.end();
            },
            |_| {},
        );
        assert_eq!(check(&dtb, None), Ok(()));
    }

    fn cma(tree: &mut Tree, start: u64, window: u64, size: u64, alignment: u64) {
        sbi(tree);
        tree.begin("linux,cma");
        tree.prop("compatible", b"shared-dma-pool\0");
        tree.prop("size", &size.to_be_bytes());
        tree.prop("alignment", &alignment.to_be_bytes());
        tree.prop("alloc-ranges", &reg(start, window));
        tree.prop("reusable", &[]);
        tree.end();
    }

    #[test]
    fn bounded_dynamic_cma_above_boot_workspace_is_supported() {
        let dtb = tree(
            &[reg(0, 0x8000_0000)],
            b"\0",
            None,
            |tree| cma(tree, 0x4000_0000, 0x1000_0000, 0x0800_0000, 0x0020_0000),
            |_| {},
        );
        assert_eq!(check(&dtb, None), Ok(()));
    }

    #[test]
    fn dynamic_ranges_must_be_known_ram_and_fit_the_request() {
        for (start, window, size, alignment, expected) in [
            // A permitted range must stay entirely above low 128 MiB.
            (0x0700_0000, 0x1000_0000, 0x0100_0000, 1, Error::Address),
            // The range must stay inside the supplied, unmodified RAM map.
            (0x8000_0000, 0x1000, 0x1000, 1, Error::Address),
            // At least one permitted interval must fit the complete allocation.
            (0x4000_0000, 0x1000, 0x2000, 1, Error::Bounds),
            (0x4000_0001, 0x1000, 0x1000, 0x1000, Error::Bounds),
            (0x4000_0000, 0x1000, 0, 1, Error::Bounds),
            (0x4000_0000, 0x1000, 0x1000, 3, Error::Unsupported),
        ] {
            let dtb = tree(
                &[reg(0, 0x8000_0000)],
                b"\0",
                None,
                |tree| cma(tree, start, window, size, alignment),
                |_| {},
            );
            assert_eq!(check(&dtb, None), Err(expected));
        }
        let dtb = tree(
            &[reg(0, DRAM_END), reg(0x5000_0000, 0x1000_0000)],
            b"\0",
            None,
            |tree| cma(tree, 0x4000_0000, 0x1000, 0x1000, 1),
            |_| {},
        );
        assert_eq!(check(&dtb, None), Err(Error::Address));
    }

    #[test]
    fn header_reservations_cannot_hide_an_overlap_or_overflow() {
        for start in [
            KERNEL.address as u64,
            0x0200_0000,
            INITRD.address as u64,
            STAGING.start,
        ] {
            let dtb = tree(&[reg(0, DRAM_END)], b"\0", Some(INITRD), sbi, |tree| {
                tree.reservation(start, 8)
            });
            assert_eq!(check(&dtb, Some(INITRD)), Err(Error::Overlap));
        }
        let dtb = tree(&[reg(0, DRAM_END)], b"\0", None, sbi, |tree| {
            tree.reservation(u64::MAX, 8)
        });
        assert_eq!(check(&dtb, None), Err(Error::Bounds));
        let dtb = tree(&[reg(0, DRAM_END)], b"\0", None, sbi, |tree| {
            tree.reservation(0x8000_0000, 0x1000);
            tree.reservation(0x8000_0800, 0x1000);
        });
        assert_eq!(check(&dtb, None), Err(Error::Overlap));
        let dtb = tree(&[reg(0, DRAM_END)], b"\0", None, sbi, |tree| {
            tree.reservation(0, SBI_END)
        });
        assert_eq!(check(&dtb, None), Ok(()));
    }
}
