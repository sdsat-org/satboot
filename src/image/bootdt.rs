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
