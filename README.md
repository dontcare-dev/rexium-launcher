# Rexium Launcher

Rexium Launcher is a Minecraft launcher built in Rust.

## Included client

Tensura Client is installed into its own managed instance on first launch. It uses Fabric and a compatible performance stack including Sodium, Lithium, FerriteCore, ImmediatelyFast, Entity Culling, ModernFix, Iris, Dynamic FPS, More Culling, and Noisium where releases are available for the selected Minecraft version.

Complementary Reimagined is installed as the default shader pack when a compatible release is available. Additional mods, resource packs, and shader packs can be searched through Modrinth and installed into the Tensura instance folders.

## Minecraft account

Microsoft sign-in uses the device-code flow. Add a Microsoft Entra public-client application ID in Settings before signing in. The Microsoft account must own Minecraft: Java Edition. Account tokens are held in memory for the current launcher session.

## Runtime and paths

The launcher installs a Java 21 runtime on Windows when a suitable Java runtime is not found. Minecraft libraries and version metadata are stored under the Tensura instance's managed Minecraft directory. Mods, resource packs, shader packs, saves, screenshots, and configs are stored in the instance game directory.

## Windows installation

Download RexiumLauncherSetup.exe from the Windows Actions artifact and run it. The installer creates Start Menu and optional desktop shortcuts. The client profile downloads on first launch.

## Build

    cargo build --release

Mod and shader downloads are provided by the public Modrinth API. Minecraft files are installed through mc-launcher-core from Mojang version metadata. Rexium Launcher is an independent community project and is not affiliated with Mojang or Microsoft.
