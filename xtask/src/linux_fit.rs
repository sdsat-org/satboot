// SPDX-License-Identifier: MIT OR MulanPSL-2.0

//! Host-side FIT construction; the firmware's parser validates every output.

use anyhow::{Context, Result, bail, ensure};
use satboot::image::layout::{ENV_MAX_SIZE, Layout, crc32};
use std::collections::BTreeMap;

const KERNEL_ADDRESS: u64 = 0x0100_0000;
const DTB_ADDRESS: u64 = 0x0300_0000;
const INITRD_ADDRESS: u64 = 0x0340_0000;
const SBI_SIZE: u64 = 0x0020_0000;
const DTB_LIMIT: usize = 0x40000;

#[derive(Clone, Debug, Default)]
struct Node {
    name: String,
    properties: Vec<(String, Vec<u8>)>,
    children: Vec<Node>,
}

impl Node {
    fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    fn set(&mut self, name: &str, value: impl Into<Vec<u8>>) {
        self.properties.retain(|(key, _)| key != name);
        self.properties.push((name.into(), value.into()));
    }

    fn get(&self, name: &str) -> Option<&[u8]> {
        self.properties
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_slice())
    }

    fn child_mut(&mut self, name: &str) -> &mut Self {
        let index = self
            .children
            .iter()
            .position(|node| node.name == name)
            .unwrap_or_else(|| {
                self.children.push(Self::new(name));
                self.children.len() - 1
            });
        &mut self.children[index]
    }
}

#[derive(Default)]
struct Tree {
    root: Node,
    reservations: Vec<(u64, u64)>,
    boot_cpu: u32,
}

fn word(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at.checked_add(4).context("FDT offset overflow")?)
            .context("truncated FDT word")?
            .try_into()?,
    ))
}

fn cstring(bytes: &[u8]) -> Result<(&str, usize)> {
    let end = bytes
        .iter()
        .position(|&b| b == 0)
        .context("unterminated FDT name")?;
    Ok((std::str::from_utf8(&bytes[..end])?, end + 1))
}

fn aligned(value: usize) -> usize {
    (value + 3) & !3
}

impl Tree {
    fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= DTB_LIMIT, "board DTB exceeds 256 KiB");
        ensure!(word(bytes, 0)? == 0xd00d_feed, "not an FDT");
        let size = word(bytes, 4)? as usize;
        ensure!(size >= 40 && size <= bytes.len(), "invalid FDT length");
        let bytes = &bytes[..size];
        ensure!(
            word(bytes, 20)? == 17 && word(bytes, 24)? <= 17,
            "require FDT version 17"
        );
        let structure_at = word(bytes, 8)? as usize;
        let strings_at = word(bytes, 12)? as usize;
        let reserve_at = word(bytes, 16)? as usize;
        let structure_end = structure_at
            .checked_add(word(bytes, 36)? as usize)
            .context("FDT range")?;
        let strings_end = strings_at
            .checked_add(word(bytes, 32)? as usize)
            .context("FDT range")?;
        ensure!(
            structure_at >= 40
                && strings_at >= 40
                && reserve_at >= 40
                && structure_at.is_multiple_of(4)
                && reserve_at.is_multiple_of(8),
            "invalid FDT block offset"
        );
        ensure!(
            structure_end <= strings_at || strings_end <= structure_at,
            "overlapping FDT blocks"
        );
        let structure = bytes
            .get(structure_at..structure_end)
            .context("truncated structure")?;
        let strings = bytes
            .get(strings_at..strings_end)
            .context("truncated strings")?;
        let mut reservations = Vec::new();
        let mut reserve_end = reserve_at;
        loop {
            let entry = bytes
                .get(
                    reserve_end
                        ..reserve_end
                            .checked_add(16)
                            .context("FDT reservation overflow")?,
                )
                .context("unterminated FDT reservation map")?;
            reserve_end += 16;
            let address = u64::from_be_bytes(entry[..8].try_into()?);
            let length = u64::from_be_bytes(entry[8..].try_into()?);
            if address == 0 && length == 0 {
                break;
            }
            ensure!(
                length != 0 && address.checked_add(length).is_some(),
                "invalid FDT reservation"
            );
            reservations.push((address, length));
        }
        ensure!(
            (reserve_end <= structure_at || structure_end <= reserve_at)
                && (reserve_end <= strings_at || strings_end <= reserve_at),
            "overlapping FDT reservation map"
        );
        let mut offset = 0;
        let root = parse_node(structure, strings, &mut offset, 0)?;
        ensure!(root.name.is_empty(), "FDT root must be unnamed");
        while word(structure, offset)? == 4 {
            offset += 4;
        }
        ensure!(word(structure, offset)? == 9, "missing FDT end token");
        ensure!(
            structure[offset + 4..].iter().all(|&b| b == 0),
            "trailing FDT tokens"
        );
        Ok(Self {
            root,
            reservations,
            boot_cpu: word(bytes, 28)?,
        })
    }

    fn encode(&self) -> Result<Vec<u8>> {
        let mut structure = Vec::new();
        let mut strings = Vec::new();
        let mut offsets = BTreeMap::new();
        encode_node(&self.root, &mut structure, &mut strings, &mut offsets)?;
        structure.extend_from_slice(&9u32.to_be_bytes());
        let structure_at = 40 + 16 * (self.reservations.len() + 1);
        let strings_at = structure_at + structure.len();
        let size = strings_at + strings.len();
        let mut bytes = Vec::with_capacity(size);
        for field in [
            0xd00d_feed,
            u32::try_from(size)?,
            u32::try_from(structure_at)?,
            u32::try_from(strings_at)?,
            40,
            17,
            16,
            self.boot_cpu,
            u32::try_from(strings.len())?,
            u32::try_from(structure.len())?,
        ] {
            bytes.extend_from_slice(&field.to_be_bytes());
        }
        for &(address, length) in &self.reservations {
            bytes.extend_from_slice(&address.to_be_bytes());
            bytes.extend_from_slice(&length.to_be_bytes());
        }
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&structure);
        bytes.extend_from_slice(&strings);
        Ok(bytes)
    }
}

fn parse_node(structure: &[u8], strings: &[u8], offset: &mut usize, depth: usize) -> Result<Node> {
    ensure!(
        depth < 32 && word(structure, *offset)? == 1,
        "invalid or deeply nested FDT node"
    );
    *offset += 4;
    let (name, length) = cstring(structure.get(*offset..).context("truncated node")?)?;
    ensure!(
        !name.contains('/') && (depth == 0 || !name.is_empty()),
        "invalid FDT node name"
    );
    let mut node = Node::new(name);
    *offset = aligned(*offset + length);
    let mut children_started = false;
    loop {
        match word(structure, *offset)? {
            1 => {
                children_started = true;
                let child = parse_node(structure, strings, offset, depth + 1)?;
                ensure!(
                    !node.children.iter().any(|n| n.name == child.name),
                    "duplicate FDT child"
                );
                node.children.push(child);
            }
            2 => {
                *offset += 4;
                return Ok(node);
            }
            3 => {
                ensure!(!children_started, "FDT properties must precede children");
                let length = word(structure, *offset + 4)? as usize;
                let name_at = word(structure, *offset + 8)? as usize;
                let (name, _) = cstring(
                    strings
                        .get(name_at..)
                        .context("invalid property name offset")?,
                )?;
                ensure!(
                    !name.is_empty() && node.get(name).is_none(),
                    "duplicate or empty property"
                );
                *offset += 12;
                let end = offset
                    .checked_add(length)
                    .context("property length overflow")?;
                node.set(
                    name,
                    structure.get(*offset..end).context("truncated property")?,
                );
                *offset = aligned(end);
            }
            4 => *offset += 4,
            _ => bail!("invalid FDT token"),
        }
    }
}

fn encode_node(
    node: &Node,
    structure: &mut Vec<u8>,
    strings: &mut Vec<u8>,
    offsets: &mut BTreeMap<String, u32>,
) -> Result<()> {
    structure.extend_from_slice(&1u32.to_be_bytes());
    structure.extend_from_slice(node.name.as_bytes());
    structure.push(0);
    structure.resize(aligned(structure.len()), 0);
    for (name, value) in &node.properties {
        let offset = if let Some(offset) = offsets.get(name) {
            *offset
        } else {
            let offset = u32::try_from(strings.len())?;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            offsets.insert(name.clone(), offset);
            offset
        };
        for word in [3, u32::try_from(value.len())?, offset] {
            structure.extend_from_slice(&word.to_be_bytes());
        }
        structure.extend_from_slice(value);
        structure.resize(aligned(structure.len()), 0);
    }
    for child in &node.children {
        encode_node(child, structure, strings, offsets)?;
    }
    structure.extend_from_slice(&2u32.to_be_bytes());
    Ok(())
}

fn text(value: &str) -> Vec<u8> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

fn reg(address: u64, size: u64) -> Vec<u8> {
    [address.to_be_bytes(), size.to_be_bytes()].concat()
}

// The vendor K1 RCPU heap starts at 1 MiB, within the SBI runtime reservation.
// This initial boot policy keeps that processor off. Preserve unrelated devices
// and memory pools; only disable its referenced pools that intersect SBI.
fn disable_rcpu(node: &mut Node, pools: &mut Vec<u32>) -> Result<()> {
    if node.get("compatible").is_some_and(|bytes| {
        bytes
            .split(|&b| b == 0)
            .any(|name| name == b"spacemit,k1-x-rproc")
    }) {
        if let Some(regions) = node.get("memory-region") {
            ensure!(
                regions.len().is_multiple_of(4),
                "invalid RCPU memory-region list"
            );
            for phandle in regions.as_chunks::<4>().0 {
                pools.push(u32::from_be_bytes(*phandle));
            }
        }
        node.set("status", text("disabled"));
    }
    for child in &mut node.children {
        disable_rcpu(child, pools)?;
    }
    Ok(())
}

fn prepare_dtb(dtb: &[u8], bootargs: &str, initrd: Option<&[u8]>) -> Result<Vec<u8>> {
    ensure!(
        !bootargs.is_empty() && bootargs.len() < 4096 && !bootargs.contains('\0'),
        "bootargs must be a nonempty string shorter than 4096 bytes"
    );
    let mut tree = Tree::parse(dtb)?;
    ensure!(
        tree.root.get("#address-cells") == Some(2u32.to_be_bytes().as_slice())
            && tree.root.get("#size-cells") == Some(2u32.to_be_bytes().as_slice()),
        "require a 64-bit Linux board DTB"
    );
    let mut rcpu_pools = Vec::new();
    disable_rcpu(&mut tree.root, &mut rcpu_pools)?;
    let chosen = tree.root.child_mut("chosen");
    chosen.set("bootargs", text(bootargs));
    chosen
        .properties
        .retain(|(name, _)| name != "linux,initrd-start" && name != "linux,initrd-end");
    if let Some(bytes) = initrd {
        ensure!(!bytes.is_empty(), "initramfs must not be empty");
        chosen.set("linux,initrd-start", INITRD_ADDRESS.to_be_bytes());
        chosen.set(
            "linux,initrd-end",
            (INITRD_ADDRESS
                .checked_add(bytes.len() as u64)
                .context("initramfs size overflow")?)
            .to_be_bytes(),
        );
    }
    let reserved = tree.root.child_mut("reserved-memory");
    for name in ["#address-cells", "#size-cells"] {
        ensure!(
            reserved
                .get(name)
                .is_none_or(|value| value == 2u32.to_be_bytes()),
            "reserved-memory must use two cells"
        );
        reserved.set(name, 2u32.to_be_bytes());
    }
    ensure!(
        reserved.get("ranges").is_none_or(|value| value.is_empty()),
        "reserved-memory ranges must be empty"
    );
    reserved.set("ranges", Vec::new());
    for child in &mut reserved.children {
        let phandle = child.get("phandle").or_else(|| child.get("linux,phandle"));
        let is_rcpu = phandle
            .map(|bytes| word(bytes, 0))
            .transpose()?
            .is_some_and(|id| rcpu_pools.contains(&id));
        if is_rcpu && let Some(ranges) = child.get("reg") {
            ensure!(ranges.len().is_multiple_of(16), "invalid RCPU reservation");
            let mut overlaps_sbi = false;
            for range in ranges.as_chunks::<16>().0 {
                let start = u64::from_be_bytes(range[..8].try_into()?);
                let size = u64::from_be_bytes(range[8..].try_into()?);
                ensure!(
                    size != 0 && start.checked_add(size).is_some(),
                    "invalid RCPU region"
                );
                overlaps_sbi |= start < SBI_SIZE;
            }
            if overlaps_sbi {
                child.set("status", text("disabled"));
            }
        }
    }
    // Extend an existing firmware reservation at zero, retaining its phandle and name.
    let candidate = reserved.children.iter().position(|node| {
        node.get("status")
            .is_none_or(|status| status == b"okay\0" || status == b"ok\0")
            && node.get("no-map").is_some()
            && node
                .get("reg")
                .is_some_and(|value| value.len() == 16 && value[..8] == [0; 8])
    });
    let firmware = if let Some(index) = candidate {
        &mut reserved.children[index]
    } else {
        reserved.child_mut("satboot-sbi@0")
    };
    ensure!(
        firmware.get("reusable").is_none(),
        "SBI reservation cannot be reusable"
    );
    let size = if let Some(value) = firmware.get("reg") {
        ensure!(
            value.len() == 16 && value[..8] == [0; 8],
            "invalid existing SBI reservation"
        );
        u64::from_be_bytes(value[8..].try_into()?).max(SBI_SIZE)
    } else {
        SBI_SIZE
    };
    firmware.set("reg", reg(0, size));
    firmware.set("no-map", Vec::new());
    firmware.set("status", text("okay"));
    let bytes = tree.encode()?;
    ensure!(bytes.len() <= DTB_LIMIT, "prepared DTB exceeds 256 KiB");
    Ok(bytes)
}

fn image(name: &str, kind: &str, data: &[u8], address: u64) -> Node {
    let mut node = Node::new(name);
    node.set("type", text(kind));
    node.set("arch", text("riscv"));
    if kind != "flat_dt" {
        node.set("os", text("linux"));
    }
    node.set("compression", text("none"));
    node.set("load", address.to_be_bytes());
    if kind == "kernel" {
        node.set("entry", address.to_be_bytes());
    }
    node.set("data", data);
    let mut hash = Node::new("hash-1");
    hash.set("algo", text("crc32"));
    hash.set("value", crc32(data).to_be_bytes());
    node.children.push(hash);
    node
}

pub fn create(
    kernel: &[u8],
    dtb: &[u8],
    initrd: Option<&[u8]>,
    product: &str,
    bootargs: &str,
) -> Result<Vec<u8>> {
    ensure!(
        !product.is_empty() && !product.contains('\0'),
        "invalid board product"
    );
    let dtb = prepare_dtb(dtb, bootargs, initrd)?;
    let mut tree = Tree::default();
    tree.root
        .set("description", text("satboot Linux boot images"));
    tree.root.set("#address-cells", 2u32.to_be_bytes());
    let images = tree.root.child_mut("images");
    images
        .children
        .push(image("kernel", "kernel", kernel, KERNEL_ADDRESS));
    images
        .children
        .push(image("dtb", "flat_dt", &dtb, DTB_ADDRESS));
    if let Some(bytes) = initrd {
        images
            .children
            .push(image("initrd", "ramdisk", bytes, INITRD_ADDRESS));
    }
    let configurations = tree.root.child_mut("configurations");
    configurations.set("default", text("linux"));
    let configuration = configurations.child_mut("linux");
    configuration.set("description", text(product));
    configuration.set("kernel", text("kernel"));
    configuration.set("fdt", text("dtb"));
    if initrd.is_some() {
        configuration.set("ramdisk", text("initrd"));
    }
    let bytes = tree.encode()?;
    satboot::image::validate_linux_fit(&bytes, product)
        .map_err(|error| anyhow::anyhow!("Linux FIT rejected by firmware parser: {error:?}"))?;
    Ok(bytes)
}

pub fn environment(capacity: u32) -> Result<Vec<u8>> {
    let mut bytes = vec![0; ENV_MAX_SIZE];
    let parts = b"mtdparts=mtdparts=d420c000.spi-0:128K(bootinfo),256K(fsbl),64K(env),512K(opensbi),-(linux)\0\0";
    bytes[4..4 + parts.len()].copy_from_slice(parts);
    let crc = crc32(&bytes[4..]);
    bytes[..4].copy_from_slice(&crc.to_le_bytes());
    Layout::from_env(&bytes, capacity)
        .map_err(|error| anyhow::anyhow!("invalid NOR capacity/layout: {error:?}"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kernel() -> Vec<u8> {
        let mut bytes = vec![0; 128];
        bytes[8..16].copy_from_slice(&0x20_0000u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&0x30_0000u64.to_le_bytes());
        bytes[32..36].copy_from_slice(&2u32.to_le_bytes());
        bytes[48..56].copy_from_slice(b"RISCV\0\0\0");
        bytes[56..60].copy_from_slice(b"RSC\x05");
        bytes
    }

    fn board_tree() -> Tree {
        let mut tree = Tree {
            boot_cpu: 0,
            reservations: vec![(0x1000_0000, 0x1000)],
            ..Tree::default()
        };
        tree.root.set("#address-cells", 2u32.to_be_bytes());
        tree.root.set("#size-cells", 2u32.to_be_bytes());
        tree.root
            .set("compatible", text("spacemit,k1-x-MUSE-Pi-Pro"));
        let memory = tree.root.child_mut("memory@0");
        memory.set("device_type", text("memory"));
        memory.set(
            "reg",
            [reg(0, 0x8000_0000), reg(0x1_0000_0000, 0x8000_0000)].concat(),
        );
        let chosen = tree.root.child_mut("chosen");
        chosen.set("stdout-path", text("serial0:115200n8"));
        chosen.set("linux,initrd-start", 99u64.to_be_bytes());
        chosen.set("linux,initrd-end", 100u64.to_be_bytes());
        tree
    }

    #[test]
    fn packs_linux_with_and_without_initramfs_and_preserves_board_memory() {
        let input = board_tree();
        let dtb = input.encode().unwrap();
        for initrd in [None, Some(b"test initramfs".as_slice())] {
            let bytes = create(
                &kernel(),
                &dtb,
                initrd,
                "k1-x_MUSE-Pi-Pro",
                "console=ttyS0,115200 rdinit=/init",
            )
            .unwrap();
            let fit = Tree::parse(&bytes).unwrap();
            let image = fit
                .root
                .children
                .iter()
                .find(|node| node.name == "images")
                .unwrap()
                .children
                .iter()
                .find(|node| node.name == "dtb")
                .unwrap();
            let prepared = Tree::parse(image.get("data").unwrap()).unwrap();
            assert_eq!(prepared.reservations, input.reservations);
            assert_eq!(
                prepared
                    .root
                    .children
                    .iter()
                    .find(|n| n.name == "memory@0")
                    .unwrap()
                    .get("reg"),
                input
                    .root
                    .children
                    .iter()
                    .find(|n| n.name == "memory@0")
                    .unwrap()
                    .get("reg")
            );
            let chosen = prepared
                .root
                .children
                .iter()
                .find(|node| node.name == "chosen")
                .unwrap();
            assert_eq!(
                chosen.get("stdout-path"),
                Some(text("serial0:115200n8").as_slice())
            );
            assert_eq!(chosen.get("linux,initrd-start").is_some(), initrd.is_some());
        }
    }

    #[test]
    fn disables_only_rcpu_and_its_sbi_overlapping_pool() {
        let mut tree = board_tree();
        let rcpu = tree.root.child_mut("soc").child_mut("rcpu_rproc@0");
        rcpu.set(
            "compatible",
            b"vendor,alternate\0spacemit,k1-x-rproc\0".as_slice(),
        );
        rcpu.set("status", text("okay"));
        rcpu.set(
            "memory-region",
            [7u32.to_be_bytes(), 8u32.to_be_bytes()].concat(),
        );
        let device = tree.root.child_mut("soc").child_mut("uart@d4017000");
        device.set("status", text("okay"));
        let reserved = tree.root.child_mut("reserved-memory");
        for (name, id, start) in [
            ("rcpu-heap@100000", 7u32, 0x10_0000),
            ("rcpu-code@500000", 8u32, 0x50_0000),
        ] {
            let pool = reserved.child_mut(name);
            pool.set("phandle", id.to_be_bytes());
            pool.set("reg", reg(start, 0x10_0000));
            pool.set("no-map", Vec::new());
        }
        let bytes = prepare_dtb(&tree.encode().unwrap(), "console=ttyS0", None).unwrap();
        let mut prepared = Tree::parse(&bytes).unwrap();
        assert_eq!(
            prepared
                .root
                .child_mut("soc")
                .child_mut("rcpu_rproc@0")
                .get("status"),
            Some(b"disabled\0".as_slice())
        );
        assert_eq!(
            prepared
                .root
                .child_mut("soc")
                .child_mut("uart@d4017000")
                .get("status"),
            Some(b"okay\0".as_slice())
        );
        let reserved = prepared.root.child_mut("reserved-memory");
        assert_eq!(
            reserved.child_mut("rcpu-heap@100000").get("status"),
            Some(b"disabled\0".as_slice())
        );
        assert_eq!(reserved.child_mut("rcpu-code@500000").get("status"), None);
        assert_eq!(
            reserved.child_mut("rcpu-code@500000").get("reg"),
            Some(reg(0x50_0000, 0x10_0000).as_slice())
        );
        create(
            &kernel(),
            &tree.encode().unwrap(),
            None,
            "k1-x_MUSE-Pi-Pro",
            "console=ttyS0",
        )
        .unwrap();
    }

    #[test]
    fn refuses_wrong_kernel_memory_collision_and_ambiguous_dtb() {
        let dtb = board_tree().encode().unwrap();
        assert!(create(&[0; 128], &dtb, None, "k1-x_MUSE-Pi-Pro", "console=ttyS0").is_err());
        let mut oversized = kernel();
        oversized[16..24].copy_from_slice(&0x0400_0000u64.to_le_bytes());
        assert!(create(&oversized, &dtb, None, "k1-x_MUSE-Pi-Pro", "console=ttyS0").is_err());
        let mut bad = board_tree();
        bad.root.children.push(Node::new("chosen"));
        assert!(
            create(
                &kernel(),
                &bad.encode().unwrap(),
                None,
                "k1-x_MUSE-Pi-Pro",
                "console=ttyS0"
            )
            .is_err()
        );
        assert!(
            create(
                &kernel(),
                &dtb,
                None,
                "k1-x_MUSE-Pi-Pro",
                "console=ttyS0\0bad"
            )
            .is_err()
        );
    }

    #[test]
    fn nor_environment_names_linux_and_checks_actual_capacity() {
        let bytes = environment(16 * 1024 * 1024).unwrap();
        let layout = Layout::from_env(&bytes, 16 * 1024 * 1024).unwrap();
        assert_eq!(layout.kernel.offset, 0xf0000);
        assert_eq!(layout.kernel.size, 16 * 1024 * 1024 - 0xf0000);
        assert!(environment(0xf0000).is_err());
    }

    #[test]
    fn malformed_fdt_headers_fail_without_panicking() {
        let dtb = board_tree().encode().unwrap();
        for offset in [4, 8, 12, 16, 32, 36] {
            let mut bad = dtb.clone();
            bad[offset..offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
            assert!(Tree::parse(&bad).is_err());
        }
        for length in [0, 8, 32, dtb.len() - 1] {
            assert!(Tree::parse(&dtb[..length]).is_err());
        }
    }
}
