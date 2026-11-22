# TODO: GTA V / FiveM conversion

Done: Rust writers (`6ixpack convert`) and `6ixpack fivem build` (terrain, buildings incl. fragments, interiors and LOD chains, water, shared txds, incremental, self-check, --report); items 1-3, 7, 8 and most of 5 below.

Open gaps found:
- Fragments convert as static ydr (no breakage). Textures missing from the ttdb pools come from the TTPF stores (re-encoded DXT1/DXT5); 3 in the test district are in neither.
- Interior contents are opt-in (`--interiors`): the MLO shell model is not found, so multi-floor contents float. LOD parents that are prop batches (`*_batchlod`) are ignored.
- Terrain tile numbering differs from the Python run (153 vs 158 tiles).
- Nothing is checked in game yet; quaternion convention unverified (`--conjugate-rot`).

Next, from the 6m-port session's requests:

1. `6ixpack fivem build --out <res> [--bbox ..] [--name ..]`: writes stream/ (ydr/ybn/ytd/ytyp/ymap), fxmanifest.lua (this_is_a_map, DLC_ITYP_REQUEST, WATER_FILE) and water.xml. Incremental: skip outputs that are up to date.
2. Reload every written file in Rust and report counts (verts, polys, textures); fail loudly on a bad file.
3. Shared txds: dedupe textures by key, group them per area/name family, cap each ytd at ~48 MB, set the ytyp textureDictionary, use a placeholder for keys that don't resolve, keep full mip chains.
4. Materials: map 6m shader params to GTA V shaders (normal_spec, cutout/decal, terrain_cb_w_4lyr with vertex-colour weights), driven by the `texture-gaps` and `shaders` census.
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

## fivem features (from the 5m migration)
- Road paths to .ynd (AI traffic), from `sixng paths`
- Navmesh to .ynv (ped/AI pathing)
- Lights to ymap LODLights / DistantLODLights (night lighting)
- Instanced batches (grass, vegetation) to ymap grass instance lists
- Minimap texture and zone names
- Split output into several resources by region/size (the 2 km airport alone is 5.2 GB)
- Coordinate offset option: 6m reaches y 9474, past GTA V's ~±8000 limit
- Los Santos removal in Rust: stub ymaps/ybns for vanilla _citye/_cityw/_hills, packing the ~11k ybn stubs into fewer files
