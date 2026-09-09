# Windows flag pack

251 existing NORY country/region PNGs, fitted inside 28 × 19 logical pixels.
`src/flag_assets.rs` embeds them directly with `include_bytes!`; that module
and its UI consumer are both gated by `cfg(target_os = "windows")`.

Windows does not need a country emoji font, a network download, or an external
`flags` directory. The location globe is drawn by the native icon renderer.
Linux continues to use system emoji fonts for both flags and the globe.

Preserve each flag's national aspect ratio. Do not add a generic all-platform
fallback that silently replaces Linux's system icons with this pack.
