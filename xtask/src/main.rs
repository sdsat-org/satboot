// SPDX-License-Identifier: MIT OR MulanPSL-2.0

mod elf2bin;
mod fsbl;

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Board {
    BpiF3,
    MuseCard,
    MusePiPro,
}

impl Board {
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
        "output must not overwrite the input ELF"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_requires_explicit_board_and_keys() {
        Cli::command().debug_assert();
        assert!(Cli::try_parse_from(["satboot-xtask", "boards"]).is_ok());
        assert!(Cli::try_parse_from(["satboot-xtask", "build"]).is_err());
        for board in ["bpi-f3", "muse-card", "muse-pi-pro"] {
            assert!(Cli::try_parse_from(["satboot-xtask", "build", "--board", board]).is_ok());
        }
        assert!(
            Cli::try_parse_from([
                "satboot-xtask",
                "build",
                "--board",
                "bpi-f3",
                "--diagnostic",
                "hello-world"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["satboot-xtask", "pack", "--elf", "satboot.elf"]).is_err());
        assert!(
            Cli::try_parse_from([
                "satboot-xtask",
                "pack",
                "--elf",
                "satboot.elf",
                "--keys",
                "keys"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["satboot-xtask", "inspect", "satboot-FSBL.bin"]).is_ok());
    }

    #[test]
    fn default_container_path_is_beside_elf() {
        assert_eq!(
            output_path(Path::new("out/satboot.elf")),
            PathBuf::from("out/satboot-FSBL.bin")
        );
        assert_eq!(
            output_path(Path::new("satboot")),
            PathBuf::from("satboot-FSBL.bin")
        );
    }
}
