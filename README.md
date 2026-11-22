# 6to5m

GTA VI (6m) to GTA V / FiveM (5m) toolkit: extract RPF9 assets and convert map areas into ready FiveM resources. The binary is `6ixpack`.

## Status

What works today:

- **Extraction**: RPF9 archive scanning and carving, names, placements, interiors, lights, paths, navmesh, collision, audio (Opus/Ogg/WAV), Bink movies, PSO/meta to JSON, vehicles and handling data.
- **Textures**: RAGE pools and TTPF stores to DDS/PNG; BCPack (Xbox hardware compression) decoded in pure Rust for BC1, BC3, BC5 and BC7.
- **Models**: drawables and fragments to OBJ/GLB.
- **GTA V / FiveM** (`6ixpack fivem build`): a ready resource for any map area.
  - Terrain tiles, water and buildings/props as ydr, with ytd textures re-encoded to DXT1/DXT5.
  - Texture dictionaries share common textures through a gtxd.meta parent chain.
  - Correct render buckets (opaque, alpha, decal, cutout).
  - LOD/SLOD chains (or `--no-lod`).
  - Static collision as ybn.
  - Every output is checked against a CodeWalker loader.

## Roadmap

- [ ] BCPack BC4 and BC6H history presets (the last undecodable textures)
- [ ] Missing road and asphalt surfaces (unlinked LOD drawables, models in other archives)
- [ ] Models outside `levels.rpf` (`pack/*` archives)
- [ ] Collision surface materials (currently all default)
- [ ] Interior (MLO) shells, so interior contents stop needing `--interiors`
- [ ] Tint palettes and vertex-colour materials
- [ ] Fragments with physics (currently static)
- [ ] Peds, vehicles and weapons to GTA V formats
- [ ] Verify the gtxd.meta parent chain in game
- [ ] Road paths to .ynd (AI traffic), from `sixng paths`
- [ ] Navmesh to .ynv (ped/AI pathing)
- [ ] Lights to ymap LODLights / DistantLODLights (night lighting)
- [ ] Instanced batches (grass, vegetation) to ymap grass instance lists
- [ ] Minimap texture and zone names
- [ ] Split output into several resources by region/size (the 2 km airport alone is 5.2 GB)
- [ ] Coordinate offset option: 6m reaches y 9474, past GTA V's ~±8000 limit
- [ ] Los Santos removal in Rust: stub ymaps/ybns for vanilla _citye/_cityw/_hills, packing the ~11k ybn stubs into fewer files

## Contributing

Fork it, make it better, send a PR. Humans and AI agents both welcome.

Agents: start from `TODO.md`, stay 100% Rust, never commit game files, and credit any code you borrow.

## Build

Pure Rust, no C code, DLLs or external tools. `cargo build --release` → `target/release/6ixpack.exe`.

## Use

```
6ixpack [-n] [-v] <file.rpf> [outdir]   scan and extract payloads (cached scan index)
6ixpack audio   <args...>               AWC/ADAT audio: list, Opus, Ogg, WAV
6ixpack texture <args...>               RAGE textures and TTPF stores -> DDS/PNG (BCPack BC1 decodes)
6ixpack model   <args...>               drawables -> OBJ (cached model walk)
6ixpack sixng   <args...>               placements, vehicles, GLB, names, interiors, movies, ...
6ixpack convert <kind> ...              GTA V files: ytd, ydr, ybn, ymap, ytyp
6ixpack fivem build --game .. --out ..   ready FiveM map resource (terrain, buildings, water)
6ixpack mcp                             stdio MCP server: extract, audio, texture, model, sixng tools
```

Add `--help` to any command. `--threads N` works everywhere.

## Credits

Built on the work of:

- **offs** — [rpf9-extract](https://github.com/offs/rpf9-extract) (no license published): the archive scanner, carver and report code this tool started from.
- **breadbbbb** — [rpf9-explorer](https://github.com/breadbbbb/rpf9-explorer) (no license published): model, texture and audio decoding references.
- **holdmysocks** — [SixNG](https://github.com/holdmysocks/SixNG) (GPL-3.0): vendored in `vendor/sixng-core` and the `sixng` command.
- **ShrewBrew** — [OpenRPF9](https://github.com/shrewbrewsh/OpenRPF9) (MIT): stream-carving approach.
- **rpf9-audio contributors** — [rpf9-audio](https://github.com/jaylikesbunda/rpf9-audio) (MIT): AWC/ADAT and Ogg Opus handling.
- **opus-decoder** (BSD-3-Clause, port of libopus): built-in Opus decoding.
- **dexyfex** — [CodeWalker](https://github.com/dexyfex/CodeWalker) (MIT): GTA V resource formats (RSC7, ytd, ydr, ybn, ymap/ytyp), ported to Rust.
- **Ahmet Dedeler** — [bcpack-rs](https://github.com/Ahmet-Dedeler/bcpack-rs) (MIT/Apache-2.0): BCPack BC1 decoder (git dependency).
- **ItsNotRexed** — [Leonida](https://github.com/ItsNotRexed/Leonida) (GPL-3.0): clean-room BCPack BC3/BC4/BC5/BC6H/BC7 rebuilders and tables, ported in `vendor/sixng-core/src/bcpack.rs`.

If you are one of these authors and want your credit, link or code changed or removed, open an issue.

## License

GPL-3.0 (see `LICENSE`), as required by the vendored SixNG code. Third-party code keeps its own license as listed above.

Not affiliated with Rockstar Games or Take-Two Interactive. No game files are included; you need your own copy.
