# 6to5m: GTA VI to GTA V / FiveM converter (CLI + MCP server)

**6to5m** is a pure-Rust **command-line tool and MCP server** that extracts **GTA VI (Xbox Series X|S version)** RPF9 archives and converts maps, textures, models and collision into **GTA V / FiveM** formats (ydr, ytd, ybn, ymap, ytyp, water.xml) as ready-to-stream FiveM resources. The binary is `6ixpack`.

Keywords: GTA 6, GTA VI, RPF9, RAGE, BCPack, Xbox Series X, GTA V, FiveM, CodeWalker, ydr, ytd, ybn, ymap, MLO, map conversion, modding, MCP, Model Context Protocol, AI agents, Rust.

## For AI agents: MCP server

Run `6ixpack mcp` as a stdio MCP server, and every CLI command becomes an MCP tool: `extract`, `audio`, `texture`, `model`, `convert`, `sixng`, `fivem`. Each tool takes the same args as the CLI; pass `["--help"]` to discover options.

```json
{ "mcpServers": { "6to5m": { "command": "C:/path/to/6ixpack.exe", "args": ["mcp"] } } }
```

Claude Code: `claude mcp add 6to5m -- C:/path/to/6ixpack.exe mcp`

## CLI quick start

```
cargo build --release                       # -> target/release/6ixpack.exe (pure Rust, no DLLs)
6ixpack levels.rpf out/                     # extract an RPF9 archive
6ixpack fivem build --game <GTA VI dir> --out <resource> --bbox x0,y0,x1,y1   # map area -> FiveM resource
6ixpack fivem vehicle chino --game <GTA VI dir> --out <resource>              # one vehicle -> FiveM vehicle resource
6ixpack fivem ped cs_lucia --game <GTA VI dir> --out <resource>               # one ped -> FiveM ped resource (--list to preview)
```

## Status

What works today:

- **Extraction**: RPF9 archive scanning and carving, names, placements, interiors, lights, paths, navmesh, collision, audio (Opus/Ogg/WAV), Bink movies, PSO/meta to JSON, vehicles and handling data.
- **Textures**: RAGE pools and TTPF stores to DDS/PNG; BCPack (Xbox hardware compression) decoded in pure Rust for BC1, BC3, BC4, BC5 and BC7 (BC6H partly).
- **Models**: drawables and fragments to OBJ/GLB.
- **GTA V / FiveM** (`6ixpack fivem build`): a ready resource for any map area.
  - `6ixpack fivem vehicle <name>`: a 6m vehicle (`<name>`, `_mid`, `_hi` fragments) to `<model>.yft`, `<model>_hi.yft` and `<model>.ytd` (GTA V bone names, skeleton pruned to 255 bones, rigid parts per bone, box physics for chassis and 4 wheels), plus vehicles/handling/carvariations/carcols meta and fxmanifest. Doors stay shut, wheels are generated tyres, no paint colours.
  - `6ixpack fivem ped <name>`: every packs.rpf drawable named `<name>_*` becomes a component (`head_000_u`, `uppr_000_u`, `jbib_000_u`, ...) of `<ped>.ydd`, skinned onto `<ped>.yft`, the standard GTA V ped skeleton (SKEL_ROOT, GTA V bone names and tags) with the 6m joint positions; plus `<ped>.ytd`, `<ped>.ymt`, peds.meta and fxmanifest. Diffuse only (flat normal/spec maps), one texture per drawable.
  - Terrain tiles, water and buildings/props as ydr, with ytd textures re-encoded to DXT1/DXT5.
  - Textures picked from every material slot until one is a real colour map (normal, mask and flat maps rejected); texture size capped by prop size (128 px for small clutter).
  - Road and apron tiles: one opaque base layer plus alpha-blended decal layers, with the terrain sunk underneath.
  - Draw distances: big buildings never vanish before the props on them; small clutter streams only within 40-100 m.
  - Dense meshes simplified (meshopt-rs, 0.5% error, UV seams kept); textures capped by prop size (128 to 1024 px; `--max-tex 512` cuts building ytds by about 70%).
  - BC7 textures used only by opaque geometry are re-encoded to DXT1 (half size, cached in the texture cache).
  - Texture dictionaries share common textures through a gtxd.meta parent chain.
  - Correct render buckets (opaque, alpha, decal, cutout).
  - LOD/SLOD chains (or `--no-lod`).
  - Static collision as ybn (welded, deduplicated, weapon-only shapes dropped unless `--weapon-collision`).
  - Water only over low or empty ground (no ocean ambience under the land); stale outputs pruned on a full build.
  - Every output is checked against a CodeWalker loader.

## Roadmap

- [x] BCPack BC4
- [ ] BCPack BC6H (modes m10/m14) and the remaining inconsistent BC7 blocks (in progress: mode 14 bad blocks 75% to 14%)
- [x] Road and asphalt surfaces
- [x] LOD/SLOD drawables with no linked model (unresolved archetypes 84,216 to 37; `#member` model ids resolved, so every airport placement converts)
- [ ] Models outside `levels.rpf` (`pack/*` archives)
- [x] Interiors as GTA V MLOs: `t6m_int.ytyp` (CMloArchetypeDef with decoded rooms, entity sets and the interior entities) and `t6m_int_X_Y.ymap` (CMloInstanceDef); `--interiors` still flattens them into world space instead
- [ ] MLO portals (not decoded yet: every entity sits in limbo, entity sets start off)
- [x] Tint palettes: the embedded palette entry (vertex colour blue, row = the placement's `tintValue`) is baked into tinted copies of the colour texture (32-step colours, max 512 px); the palette is no longer picked as a colour map
- [x] Per-placement tint rows: placements with `tintValue` > 0 use an archetype variant `<name>__tint<row>`, shared by every placement with that row
- [ ] Prop batch per-instance tint (`ba049770` +0x34 RGBA)
- [ ] Fragments with physics (currently static)
- [x] Peds to GTA V: `6ixpack fivem ped <name>` (skinned ydd components, yft skeleton retargeted to the GTA V ped skeleton, ytd, ymt, peds.meta; loads in CodeWalker, in-game test pending)
- [x] Vehicles to GTA V: `6ixpack fivem vehicle <name>` (yft + vehicles/handling/carvariations/carcols metas; in-game test pending)
- [ ] Weapons to GTA V formats
- [ ] Verify the gtxd.meta parent chain in game
- [ ] Road paths to .ynd (AI traffic), from `sixng paths`
- [ ] Navmesh to .ynv (ped/AI pathing)
- [ ] Lights to ymap LODLights / DistantLODLights (night lighting)
- [ ] Instanced batches (grass, vegetation) to ymap grass instance lists
- [ ] Minimap texture and zone names
- [x] Split output into several resources by region: `--split <metres>` writes `<out>/[<name>]/` with one resource per region plus `<name>_common` (ytyps, LOD/SLOD ymaps, shared txds, terrain, water) and an `ensure.cfg`; regions over `--split-max <MB>` (default 1024) are quartered. Not yet tested in game
- [x] Collision surface materials: 6m surface ids map to GTA V materials (concrete, tarmac, gravel, dirt, sand, marsh, soil, grass, rock) through `sixng surfaces` categories; needs `<work>/collision/levels/surface_categories.json`, else DEFAULT. Not yet tested in game
- [ ] Coordinate offset option: GTA VI's map reaches y 9474, past GTA V's ~±8000 limit
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
6ixpack texture <args...>               RAGE textures and TTPF stores -> DDS/PNG (BCPack decodes)
6ixpack model   <args...>               drawables -> OBJ (cached model walk)
6ixpack sixng   <args...>               placements, vehicles, GLB, names, interiors, movies, ...
6ixpack convert <kind> ...              GTA V files: ytd, ydr, ybn, ymap, ytyp
6ixpack fivem build --game .. --out ..   ready FiveM map resource (terrain, buildings, water)
6ixpack fivem vehicle <name> ..          FiveM vehicle resource from one 6m vehicle
6ixpack fivem ped <name> ..              FiveM ped resource from one 6m ped (cs_lucia, NPCs)
6ixpack mcp                             stdio MCP server: extract, audio, texture, model, convert, sixng, fivem tools
```

Add `--help` to any command. `--threads N` works everywhere.

## Credits

Built on the work of:

- **offs** — [rpf9-extract](https://github.com/offs/rpf9-extract) (no license published): the archive scanner, carver and report code this tool started from.
- **breadbbbb** — [rpf9-explorer](https://github.com/breadbbbb/rpf9-explorer) (no license published): model, texture and audio decoding references; the 1.2 model recovery (zero-radius drawables, empty-bounds geometry fallback, optional fragment header pointers, 16-bit bone palettes) is ported in `src/model/decode.rs`.
- **holdmysocks** — [SixNG](https://github.com/holdmysocks/SixNG) (GPL-3.0): vendored in `vendor/sixng-core` and the `sixng` command; synced to 0.5.0 for drawable-dictionary members (per-member model names, bounds and exports).
- **ShrewBrew** — [OpenRPF9](https://github.com/shrewbrewsh/OpenRPF9) (MIT): stream-carving approach.
- **rpf9-audio contributors** — [rpf9-audio](https://github.com/jaylikesbunda/rpf9-audio) (MIT): AWC/ADAT and Ogg Opus handling.
- **opus-decoder** (BSD-3-Clause, port of libopus): built-in Opus decoding.
- **dexyfex** — [CodeWalker](https://github.com/dexyfex/CodeWalker) (MIT): GTA V resource formats (RSC7, ytd, ydr, ybn, ymap/ytyp), ported to Rust.
- **Ahmet Dedeler** — [bcpack-rs](https://github.com/Ahmet-Dedeler/bcpack-rs) (MIT/Apache-2.0): BCPack rANS lanes and BC1 decoder (git dependency).
- **ItsNotRexed** — [Leonida](https://github.com/ItsNotRexed/Leonida) (GPL-3.0): clean-room BCPack BC3/BC4/BC5/BC6H/BC7 rebuilders and tables, and the pool self-calibrator, ported in `vendor/sixng-core/src/bcpack*.rs`.

If you are one of these authors and want your credit, link or code changed or removed, open an issue.

## License

GPL-3.0 (see `LICENSE`), as required by the vendored SixNG code. Third-party code keeps its own license as listed above.

Not affiliated with Rockstar Games or Take-Two Interactive. No game files are included; you need your own copy.
