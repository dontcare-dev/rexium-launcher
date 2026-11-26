# Tensura Launcher

A Windows-first Minecraft Java launcher built with Rust, egui, and mc-launcher-core.

## Features

- Isolated named instances with their own Minecraft files, saves, mods, resource packs, shader packs, and configuration.
- Minecraft Java installation with Fabric, Quilt, Forge, and NeoForge profiles.
- Bundled Temurin Java 21 runtime included in the Windows installer, ready before the first game launch.
- Offline profiles for single-player and servers that permit offline-mode connections.
- Microsoft device-code authentication for accounts that own Minecraft: Java Edition.
- Modrinth discovery filtered by the selected Minecraft version and mod loader, with project artwork and one-click installation.
- Installed mod, resource-pack, and shader-pack inventory with file removal and folder shortcuts.
- Optional Fabric performance mod preset; shader packs remain opt-in.
- Server list with live status refresh.
- Instance selection with isolated saves, mods, resource packs, shader packs, screenshots, and configuration files.
- Launcher settings for Minecraft version, loader, memory allocation, custom backgrounds including animated GIFs, and accent customization.
- Windows installer generated with Inno Setup through GitHub Actions.
- GitHub release publishing from version tags and in-app update notifications.
- Installer progress reporting and persistent Java stdout/stderr logs.

## Accounts

Offline mode does not authenticate a Microsoft account or bypass premium authentication. Online-mode servers may require a legitimate Microsoft account that owns Minecraft: Java Edition. Microsoft sign-in uses a public-client application ID configured in Settings.

## Data locations

Launcher settings are stored under the operating system application-data directory in `TensuraLauncher/settings.json`. Instance game data is stored under `TensuraLauncher/instances/<instance name>`, with separate `minecraft` and `game` directories.

## Build on Windows

Install the stable Rust toolchain and Inno Setup, then run:

    cargo fmt --all -- --check
    cargo clippy --release -- -D warnings
    cargo test --release
    cargo build --release
    iscc installer/TensuraLauncher.iss

GitHub Actions bundles the Windows Java 21 runtime into the installer. On first launch, the selected Minecraft version, loader, libraries, assets and default Fabric performance mods are prepared before Play becomes available.

Mod and shader downloads use the public Modrinth API. Minecraft files are installed from Mojang and loader metadata services. Tensura Launcher is an independent community project and is not affiliated with Mojang or Microsoft.