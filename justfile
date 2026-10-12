set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

default:
    @just --list

boards:
    cargo xtask boards

build board="muse-pi-pro":
    cargo xtask build --board {{board}}

hello board="muse-pi-pro":
    cargo xtask build --board {{board}} --diagnostic hello-world

eeprom board="muse-pi-pro":
    cargo xtask build --board {{board}} --diagnostic eeprom-info

check: fmt
    cargo clippy --locked --workspace --lib -- -D warnings
    cargo clippy --locked -p satboot-xtask --bin satboot-xtask -- -D warnings

fmt:
    cargo fmt --all -- --check
