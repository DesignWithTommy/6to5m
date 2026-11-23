# TODO: GTA V / FiveM conversion

Done: Rust writers (`6ixpack convert`) and `6ixpack fivem build` (terrain, buildings incl. fragments, interiors and LOD chains, water, shared txds, incremental, self-check, --report); items 1-3, 7, 8 and most of 5 below.

Open gaps found:
- Fragment props convert as dynamic yft (fivem/frags.rs: one piece, a box per 6m collider with its mass; capsule/sphere/hull types 1, 2, 9, 14 as their bound box until their layouts are decoded; no breakage into parts yet). Textures missing from the ttdb pools come from the TTPF stores (re-encoded DXT1/DXT5); 3 in the test district are in neither.
- Interiors convert to MLOs by default (`src/gta5/fivem/interiors.rs`; checked with CodeWalker, not yet in game). Portals are decoded from the 6m archetype and entities sit in their rooms; entity sets are written off by default. Interior collision linked by sixng (exact, high or medium) is written per instance as `t6m_intcol_<hash>_<n>.ybn` with each polygon's room id picked by room bbox (6m bounds carry none; checked in CodeWalker, not in game). Interiors without linked collision keep only the world collision cells, so the player may count as outside there (`T6M_MLO_LIMBO=1` is the fallback). Interior lights are decoded from the interiors' own meta files during the build (archetypes defined in other meta files get no lights; `T6M_NO_LIGHTS=1` skips); intensity (1:1, `T6M_LIGHT_SCALE`) needs an in-game check. `--interiors` keeps the old world-space flattening. LOD parents that are prop batches (`*_batchlod`) are ignored.
- Terrain tile numbering differs from the Python run (153 vs 158 tiles).
- Nothing is checked in game yet; quaternion convention unverified (`--conjugate-rot`).

Next, from the FiveM port's requests:

1. `6ixpack fivem build --out <res> [--bbox ..] [--name ..]`: writes stream/ (ydr/ybn/ytd/ytyp/ymap), fxmanifest.lua (this_is_a_map, DLC_ITYP_REQUEST, WATER_FILE) and water.xml. Incremental: skip outputs that are up to date.
2. Reload every written file in Rust and report counts (verts, polys, textures); fail loudly on a bad file.
3. Shared txds: dedupe textures by key, group them per area/name family, cap each ytd at ~48 MB, set the ytyp textureDictionary, use a placeholder for keys that don't resolve, keep full mip chains.
4. Materials: map GTA VI shader params to GTA V shaders (normal_spec, cutout/decal, terrain_cb_w_4lyr with vertex-colour weights), driven by the `texture-gaps` and `shaders` census.
5. Placements: put interior- and group-local entities in world space (or emit MLO ytyp), keep the LOD chain from lod-links (parentIndex/numChildren), split ymaps spatially with correct extents, verify the quaternion convention (GTA V may store the inverse) against a known placement.
6. Collision: sixng collision → ybn with GTA V material ids from surface categories, one ybn per map cell.
7. Limits: names < 64 chars (hash fallback), ≤ 65k verts per geometry, drawable size caps, archetype bounds from the mesh.
8. `--report` JSON: converted, skipped and why (unnamed hashes, missing textures, fragments/skinned meshes).
9. Later: roads/paths → ynd, navmesh → ynv, lights → LODLights.

## fivem (open)
- BCPack textures: flat decodes rejected (placeholder) until the pure-Rust BCPack decoder lands in sixng-core ttpf.
- Collision materials come from inferred `sixng surfaces` categories (geometry-only unless paths/batches/navmeshes are exported first); verify in game, refine with road and plant cues.
- gtxd.meta shared txd chain unverified in game.
- Multi-archive models (pack/core_restofworld) not converted.
- LOD/SLOD drawables without a model link (unnamed hash) not converted.
- Airport: roads missing, far-apron floating poles, canopy cutout look — uninvestigated.

## fivem weapon (`src/gta5/fivem/weapons.rs`, `yft::build_ydr_skel`)

- [ ] Test in game (give WEAPON_W_PI_SCORPIO_01): grip/muzzle sockets, hand IK, animations.
- [x] Weapon textures: 6m weapon shaders (e68a4a98) carry only normal + packed material (739e3d0d, R = material id); the colour map is synthesized per id. Traced: every one of the 2,733 `weapon` (e68a4a98) shaders in data.rpf has params speculartex, 20245319, bumptex, speculartex2, materialid; 20245319 is never bound in any asset (null texture pointer), so the per-id finish palette is a runtime global the engine binds, not asset data. `materialid` (8adb8480, BC4, ids at R = 2 + 4n) is present on only 54 shaders; it sits in a type-0 texture record that sixng drawable::find_shaders skips. Not in data.rpf PSOs, 0x7a meta (find-u32 on weapon name hashes: 0 hits in data/common/packs/script/story) or named txds. Next: find the global in the executable or shaders.rpf.
- [ ] Real stats from 6m weapon data instead of GTA V class defaults; component variants (mags, sights) as GTA V components.

## fivem vehicle (`src/gta5/fivem/vehicles.rs`, `src/gta5/yft.rs`)
Loads in CodeWalker; not yet tested in game.
- Physics: one box child per exterior 6m physics group (doors, bonnet, boot, bumpers, windows, wings, mirrors, plates) with 6m collider masses, composite BVH past 5 children; joint limits are left to GTA V (6m vehicle fragments have no limits block). Unverified in game whether doors open; boxes are AABBs of each part, not the 6m hull colliders.
- Wheels: rim + tyre drawables from `sixng vehicles` (scale 1.625 x wheelScale is fitted, not decoded); right-side rim orientation unverified.
- Paint is a solid texture of the configuration's body colour (stored ARGB read as linear); add vehicle_paint shaders so carcols colours apply, plus glass, lights, dirt and damage.
- Modular parts (bumpers, intakes, roof attachments in part_groups) are not added; handling PSO fields are unnamed, so handling stays default.
- Handling, seats, layout and meta are sedan defaults; map 6m vehicle data when it is decoded.

## fivem ped (`src/gta5/fivem/ped.rs`, `src/gta5/ped.rs`, `src/gta5/ped_bones.rs`)
Loads in CodeWalker; not yet tested in game.
- Textures: diffuse only, the one on the largest geometry; normal and spec maps are flat. Some clothing textures are not in TTPF ("not found"), so those drawables get grey.
- Some clothing vertices name bones in neither the 6m ped skeleton nor the drawable (e.g. cs_lucia tshirtcrop: 6681 influences dropped). Find those palette bones.
- Part selection is SixNG's default look (one part per outfit slot; `--all` for every part). Characters without their own head (cs_jason) use the legacy head and whole-body copies. No props, no texture variations, no base body under clothing.
- Facial bones keep GTA V reference positions (only 78 of 101 bones are placed from 6m).
- Hair and beards: `--hair` / `--beard <groom id>` draw a strand groom as crossed cards (flat `--hair-color`, rigid on SKEL_Head), and `--grooms` lists the grooms that fit the head. Nothing in the game files links a ped to its grooms, so you pick one by eye. Still to do: brows and lashes, an alpha strand texture, card UVs from the root UVs (A2), and skinning beards to the jaw.
- Head faces: heads bind no textures; albedo/normal come from a "heads" material preset (formats/07 §7.5). cs_lucia's head is in no preset, so it stays grey. Wire presets for tagged heads.
- Head skin variant 0x8c (slot 9 as 4 x u32 joints) is not decoded by sixng-core; cs_lucia's head uses the ordinary 4+4 x u8 layout.

## fivem features (from the FiveM migration)
- Road paths to .ynd (AI traffic), from `sixng paths`
- Navmesh to .ynv (ped/AI pathing)
- LODLights / DistantLODLights (`src/gta5/fivem/lodlights.rs`) are written but unverified: check in CodeWalker and at night in game (intensity byte = intensity x5.3125, clamped 255)
- Instanced batches (grass, vegetation) to ymap grass instance lists
- Minimap texture and zone names
- Region split (`--split`) done; verify in game, and split the LOD/SLOD ymaps and terrain per region if common grows too big
- Coordinate offset option: GTA VI's map reaches y 9474, past GTA V's ~±8000 limit
- Los Santos removal in Rust: stub ymaps/ybns for vanilla _citye/_cityw/_hills, packing the ~11k ybn stubs into fewer files

## Standard test region
Airport spawn, 1 km: `--bbox -2921,-1312,-1921,-312` (centre -2421,-812). Use it for all timings, file counts and in-game checks.
