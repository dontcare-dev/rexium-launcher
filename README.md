# Tensura Launcher

Tensura Launcher is a Minecraft launcher built in Rust.

## Tensura Client

Tensura Client installs as the default Fabric profile with a performance mod stack and Complementary Reimagined shaders when compatible releases are available. Mods, resource packs, shader packs, saves, screenshots, and configs are stored in the managed instance game directory.

## Accounts

Microsoft sign-in is optional. Offline mode is available without a Microsoft account. Offline play is intended for single-player and compatible offline/LAN use; online servers may require a legitimate Microsoft account that owns Minecraft: Java Edition. Microsoft sign-in requires a public-client application ID configured in Settings.

## Runtime and paths

The launcher installs a Java 21 runtime on Windows when a compatible runtime is not found. The managed instance is stored under the TensuraLauncher application data directory in `instances/Tensura Client`; the Minecraft libraries and version metadata are stored in that instance's `minecraft` directory, and game data is stored in its `game` directory.

## Windows installation

Download `TensuraLauncherSetup.exe` from the `TensuraLauncher-Windows` GitHub Actions artifact and run it. The installer creates Start Menu and optional desktop shortcuts. The Tensura profile is installed on first launch.

## Build

    cargo build --release

Mod and shader downloads use the public Modrinth API. Minecraft files are installed from Mojang version metadata. Tensura Launcher is an independent community project and is not affiliated with Mojang or Microsoft.
