// SPDX-License-Identifier: MIT OR MulanPSL-2.0

mod elf2bin;
mod fsbl;
mod linux_fit;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

const TARGET: &str = "riscv64imac-unknown-none-elf";

#[derive(Parser)]
#[command(about = "Build and inspect satellite bootloader images", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List supported board configurations.
    Boards,
    /// Build a board's ELF, raw binary and linker map without accessing hardware.
    Build {
        #[arg(long, value_enum)]
        board: Board,
        #[arg(long, value_enum)]
        diagnostic: Option<Diagnostic>,
    },
    /// Convert an ELF and sign a development K1 FSBL with explicitly supplied keys.
    Pack {
        #[arg(long)]
        elf: PathBuf,
        /// Directory with the five vendor-format RSA key files.
        #[arg(long)]
        keys: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Check the K1 FSBL structure and signatures using its embedded public keys.
    Inspect { fsbl: PathBuf },
    /// Build a Linux FIT with a prepared board DTB, boot arguments and optional initramfs.
    LinuxFit {
        #[arg(long, value_enum)]
        board: Board,
        /// Uncompressed RV64 Linux Image, not ELF or Image.gz.
        #[arg(long)]
        kernel: PathBuf,
        /// Linux board DTB with the correct installed memory banks.
        #[arg(long)]
        dtb: PathBuf,
        #[arg(long)]
        bootargs: String,
        #[arg(long)]
        initrd: Option<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Create a NOR environment for the dedicated opensbi/linux partition layout.
    LinuxEnv {
        /// Actual NOR capacity in bytes, optionally using K/M suffixes or hexadecimal.
        #[arg(long, value_parser = parse_capacity)]
        capacity: u32,
        #[arg(short, long)]
        output: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Board {
    BpiF3,
    MuseCard,
    MusePiPro,
}

impl Board {
    const fn product(self) -> &'static str {
        match self {
            Self::BpiF3 => satboot::board::BPI_F3.product_name,
            Self::MuseCard => satboot::board::MUSE_CARD.product_name,
            Self::MusePiPro => satboot::board::MUSE_PI_PRO.product_name,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::BpiF3 => "bpi-f3",
            Self::MuseCard => "muse-card",
            Self::MusePiPro => "muse-pi-pro",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::BpiF3 => "Banana Pi BPI-F3 (SpacemiT K1)",
            Self::MuseCard => "MUSE Card (SpacemiT M1)",
            Self::MusePiPro => "MUSE Pi Pro (SpacemiT M1)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Diagnostic {
    HelloWorld,
    EepromInfo,
}

impl Diagnostic {
    const fn binary(self) -> &'static str {
        match self {
            Self::HelloWorld => "hello-world",
            Self::EepromInfo => "eeprom-info",
        }
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Boards => {
            for board in [Board::BpiF3, Board::MuseCard, Board::MusePiPro] {
                println!("{:<12} {}", board.name(), board.description());
            }
            Ok(())
        }
        Command::Build { board, diagnostic } => build(board, diagnostic),
        Command::LinuxFit {
            board,
            kernel,
            dtb,
            bootargs,
            initrd,
            output,
        } => {
            for input in [Some(&kernel), Some(&dtb), initrd.as_ref()]
                .into_iter()
                .flatten()
            {
                ensure_distinct(input, &output)?;
            }
            let kernel = fs::read(&kernel).with_context(|| format!("read {}", kernel.display()))?;
            let dtb = fs::read(&dtb).with_context(|| format!("read {}", dtb.display()))?;
            let initrd = initrd
                .map(|path| fs::read(&path).with_context(|| format!("read {}", path.display())))
                .transpose()?;
            let bytes =
                linux_fit::create(&kernel, &dtb, initrd.as_deref(), board.product(), &bootargs)?;
            fs::write(&output, &bytes).with_context(|| format!("write {}", output.display()))?;
            println!(
                "{}: {} bytes; board {}",
                output.display(),
                bytes.len(),
                board.name()
            );
            println!("Linux entry: 0x1000000; DTB: 0x3000000; optional initramfs: 0x3400000");
            println!(
                "SpacemiT RCPU remoteproc is disabled in the prepared DTB to keep SBI memory exclusive."
            );
            Ok(())
        }
        Command::LinuxEnv { capacity, output } => {
            let bytes = linux_fit::environment(capacity)?;
            fs::write(&output, &bytes).with_context(|| format!("write {}", output.display()))?;
            println!(
                "{}: {} bytes; environment offset 0x60000; Linux FIT offset 0xf0000",
                output.display(),
                bytes.len()
            );
            Ok(())
        }
        Command::Pack { elf, keys, output } => {
            let output = output.unwrap_or_else(|| output_path(&elf));
            ensure_distinct(&elf, &output)?;
            let input = fs::read(&elf).with_context(|| format!("read {}", elf.display()))?;
            let raw = elf2bin::convert(&input).context("validate and convert ELF")?;
            let image = fsbl::wrap(&raw, &keys)?;
            fsbl::validate(&image).context("validate generated FSBL")?;
            fs::write(&output, &image).with_context(|| format!("write {}", output.display()))?;
            println!("{}: {} bytes", output.display(), image.len());
            println!("SHA-256: {:x}", Sha256::digest(&image));
            Ok(())
        }
        Command::Inspect { fsbl: path } => {
            let image = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            fsbl::validate(&image).context("invalid K1 FSBL")?;
            let payload_size = u64::from_le_bytes(image[0xfe8..0xff0].try_into().unwrap());
            println!("{}", path.display());
            println!("Format: K1 AIHD development container (secure=0)");
            println!(
                "Size: {} bytes; aligned payload: {payload_size} bytes",
                image.len()
            );
            println!("Payload entry: {:#x}", elf2bin::BASE);
            println!("Embedded-key signatures: valid");
            println!("SHA-256: {:x}", Sha256::digest(&image));
            println!("Embedded-key validation does not establish a trusted root key.");
            Ok(())
        }
    }
}

fn build(board: Board, diagnostic: Option<Diagnostic>) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask must be in the satboot workspace")?;
    let root = fs::canonicalize(root).context("resolve workspace path")?;
    let binary = diagnostic.map_or("satboot", Diagnostic::binary);
    let output = root.join("target").join(board.name());
    fs::create_dir_all(&output).with_context(|| format!("create {}", output.display()))?;
    let elf = output.join(format!("{binary}.elf"));
    let raw_path = output.join(format!("{binary}.bin"));
    let map = output.join(format!("{binary}.map"));
    let features = format!("board-{},ddr", board.name());
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let status = ProcessCommand::new(cargo)
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", &output)
        .env("SATBOOT_MAP_FILE", &map)
        .args([
            "build",
            "--locked",
            "-p",
            "satboot",
            "--release",
            "--target",
            TARGET,
            "--no-default-features",
            "--features",
            &features,
            "--bin",
            binary,
        ])
        .status()
        .context("run cargo build")?;
    ensure!(status.success(), "cargo build failed: {status}");
    let linked = output.join(TARGET).join("release").join(binary);
    let bytes = fs::read(&linked).with_context(|| format!("read {}", linked.display()))?;
    let raw = elf2bin::convert(&bytes).context("validate linked K1 ELF")?;
    ensure!(map.is_file(), "linker map not generated: {}", map.display());
    fs::write(&elf, &bytes).with_context(|| format!("write {}", elf.display()))?;
    fs::write(&raw_path, &raw).with_context(|| format!("write {}", raw_path.display()))?;
    println!("ELF: {}", elf.display());
    println!("BIN: {} ({} bytes)", raw_path.display(), raw.len());
    println!("MAP: {}", map.display());
    Ok(())
}

fn output_path(input: &Path) -> PathBuf {
    let mut name = input.file_stem().unwrap_or_default().to_os_string();
    name.push("-FSBL.bin");
    input.with_file_name(name)
}

fn ensure_distinct(input: &Path, output: &Path) -> Result<()> {
    ensure!(
        !output.exists() || fs::canonicalize(input)? != fs::canonicalize(output)?,
        "output must not overwrite an input file"
    );
    Ok(())
}

fn parse_capacity(value: &str) -> std::result::Result<u32, String> {
    let (digits, scale) = match value.as_bytes().last() {
        Some(b'K' | b'k') => (&value[..value.len() - 1], 1024),
        Some(b'M' | b'm') => (&value[..value.len() - 1], 1024 * 1024),
        _ => (value, 1),
    };
    let number = if let Some(hex) = digits.strip_prefix("0x") {
        u32::from_str_radix(hex, 16)
    } else {
        digits.parse::<u32>()
    };
    number
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .ok_or_else(|| "invalid NOR capacity".to_owned())
}
