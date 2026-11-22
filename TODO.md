# TODO: GTA V / FiveM conversion

Done: Rust writers (`6ixpack convert`) and `6ixpack fivem build` (terrain, buildings incl. fragments, interiors and LOD chains, water, shared txds, incremental, self-check, --report); items 1-3, 7, 8 and most of 5 below.

Open gaps found:
- Fragments convert as static ydr (no breakage). Textures missing from the ttdb pools come from the TTPF stores (re-encoded DXT1/DXT5); 3 in the test district are in neither.
- Interiors convert to MLOs by default (`src/gta5/fivem/interiors.rs`; checked with CodeWalker, not yet in game). Portals are not decoded and interior collision has no room ids, so all entities are attached to limbo and the decoded rooms are empty; entity sets are written off by default. Decode 6m portals, then attach entities to their rooms. `--interiors` keeps the old world-space flattening. LOD parents that are prop batches (`*_batchlod`) are ignored.
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
- Collision material ids not mapped (all material 0).
- gtxd.meta shared txd chain unverified in game.
- Multi-archive models (pack/core_restofworld) not converted.
- LOD/SLOD drawables without a model link (unnamed hash) not converted.
- Airport: roads missing, far-apron floating poles, canopy cutout look — uninvestigated.

## fivem vehicle (`src/gta5/fivem/vehicles.rs`, `src/gta5/yft.rs`)
Loads in CodeWalker; not yet tested in game.
- Doors, bonnet and boot are drawn on their bones but are static: physics is at most 5 box children (chassis + 4 wheels) because more needs a BVH. Add the BVH and per-part children, groups and joint limits.
- Wheels: 6m wheel meshes were not found, so each wheel gets a generated tyre cylinder. Find the 6m wheel drawables.
- Shaders are default/alpha only: add vehicle_paint shaders so carcols colours apply, plus glass, lights, dirt and damage.
- Handling, seats, layout and meta are sedan defaults; map 6m vehicle data when it is decoded.

## fivem ped (`src/gta5/fivem/ped.rs`, `src/gta5/ped.rs`, `src/gta5/ped_bones.rs`)
Loads in CodeWalker; not yet tested in game.
- Textures: diffuse only, the one on the largest geometry; normal and spec maps are flat. Some clothing textures are not in TTPF ("not found"), so those drawables get grey.
- Some clothing vertices name bones in neither the 6m ped skeleton nor the drawable (e.g. cs_lucia tshirtcrop: 6681 influences dropped). Find those palette bones.
- Slots come from name keywords; 6m pieces with no GTA V slot go to accs. No props, no texture variations, no base body under clothing.
- Facial bones keep GTA V reference positions (only 78 of 101 bones are placed from 6m).

## fivem features (from the FiveM migration)
- Road paths to .ynd (AI traffic), from `sixng paths`
- Navmesh to .ynv (ped/AI pathing)
- Lights to ymap LODLights / DistantLODLights (night lighting)
- Instanced batches (grass, vegetation) to ymap grass instance lists
- Minimap texture and zone names
- Region split (`--split`) done; verify in game, and split the LOD/SLOD ymaps and terrain per region if common grows too big
- Coordinate offset option: GTA VI's map reaches y 9474, past GTA V's ~±8000 limit
- Los Santos removal in Rust: stub ymaps/ybns for vanilla _citye/_cityw/_hills, packing the ~11k ybn stubs into fewer files
