# OmniTool

A desktop modding toolkit for **Ratchet & Clank: Rift Apart (PC)**.  
Built with Tauri + React + Rust.

---

## Tools

**Model Converter** — Export `.model` mesh data to `.ascii` for editing, then inject the modified mesh back into the original file.

**Material Remapper** — Remap material path references inside `.model` files.

**Asset Browser** — Browse the game's TOC archive, filter by path, and extract assets to a staging project.

**Config Editor** — Convert `.config` files to JSON. Edit them with the inline editor that supports formatting and syntax highlighting.

**Stager** — Manage mod projects. Collect extracted and modified assets, then export them as a `.stage` package ready for installation.

---

## Getting Started

1. Open **Settings** and point the archives folder at the game directory containing the `toc` file.
2. Click **Fetch from GitHub** to download the asset hash map — this gives the Asset Browser readable paths instead of raw IDs.
3. Load the Asset Browser and start extracting.

---

## Acknowledgements

**[ALERT](https://github.com/Tkachov/ALERT)** — this toolkit is an extension of Tkachov's ALERT. The binary format research, section layouts, and codec logic is based on that project.

**[NVIDIA/libdeflate (gdeflate)](https://github.com/NVIDIA/libdeflate/tree/gdeflate)** — GDeflate decompression support relies on NVIDIA's fork of libdeflate.
