# esprs — ESP32 Rust (esp-idf-svc) Blink Boilerplate

Rust on ESP32 (Xtensa) using the `esp-idf-svc`/`esp-idf-hal` (std) stack.

## Prerequisites

- Linux (Debian/Ubuntu commands below; adjust package manager for other distros)
- ESP32 devkit connected via USB

## Setup

```bash
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# system deps
sudo apt update && sudo apt install -y build-essential libudev-dev pkg-config python3 python3-pip

# ESP toolchain + tools
cargo install espup ldproxy cargo-generate espflash
espup install
echo 'source $HOME/export-esp.sh' >> ~/.bashrc
source $HOME/export-esp.sh

# serial port permissions (log out/in after this)
sudo usermod -aG dialout $USER
```

## Build & Flash

```bash
git clone <this-repo>
cd esprs
cargo run
```

`cargo run` builds, flashes, and opens the serial monitor (wired via `.cargo/config.toml`).

First build compiles ESP-IDF from scratch: **5–15 min**. Incremental builds after: ~10–30s.

## Troubleshooting

- **Port not found / permission denied**: confirm group membership took effect (`groups` should list `dialout`) and check `ls -l /dev/ttyUSB* /dev/ttyACM*`.
- **`espup` toolchain not picked up**: re-run `source $HOME/export-esp.sh` in the current shell, or open a new terminal after adding it to `~/.bashrc`.
- **Wrong board / LED doesn't blink**: `src/main.rs` assumes onboard LED on GPIO2 (common on generic devkits). Change the pin in `PinDriver::output(peripherals.pins.gpioX)` to match your board.

## Stack notes

- Uses `esp-idf-hal` + `esp-idf-svc` (std, wraps ESP-IDF) — gives Wi-Fi, TLS, MQTT, OTA, secure boot, flash encryption for free via IDF.
- Target: ESP32 Xtensa — requires the `esp-rs` LLVM fork toolchain (installed via `espup`), not stable upstream Rust.
- IDF version pinned in `Cargo.toml` under `[package.metadata.esp-idf-sys]`.
