# Rexium Launcher

A clean Minecraft Java launcher foundation built with Rust and egui.

## Current scope

- Modrinth search for mods, modpacks, and resource packs
- One-click downloads to the standard Minecraft folders
- Modpack archive downloads to Rexium's Downloads folder
- Animated launcher background and editable accent color
- Custom client branding

## Build

```bash
cargo build --release
```

## Run

```bash
cargo run --release
```

Modrinth content is fetched from its public API. Modrinth projects retain their own licenses and attribution.
