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

test:
    cargo test --locked --workspace --lib
    cargo test --locked -p satboot-xtask

fmt:
    cargo fmt --all -- --check
