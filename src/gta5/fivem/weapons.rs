//! `6ixpack fivem weapon <name|all>`: GTA VI (6m) weapons -> a streamable
//! FiveM add-on weapon resource.
//!
//! 6m weapons are modular (data.rpf): a tiny base drawable `w_pi_scorpio_01`
//! whose `WAP_<Slot>` bones are sockets, and one model per part
//! (`w_pi_scorpio_01_frame_01`, `_slide_01`, `_mag_01`, `_barrel_01`, ...,
//! each with `_mid` / `_hi`). A part's root bone `AAP_<Slot>` sits on its
//! parent's `WAP_<Slot>`, and parts carry further sockets (the frame holds
//! the slide, barrel, magazine; the slide holds the sights). The default
//! weapon is assembled from the first variant of every slot reachable from
//! the base, as one rigid model in weapon space (+X forward, +Z up, as in
//! GTA V), with a GTA V weapon skeleton: `Gun_Root`, `Gun_Muzzle` /
//! `Gun_Nuzzle`, `Gun_GripR`, `Gun_GripL`, `Gun_VFX_Eject` and the component
//! sockets `WAPClip`, `WAPSupp`, `WAPScop`, `WAPFlshLasr`, `WAPGrip`.
//!
//! Output (<out> is the resource):
//!   <out>/stream/<model>.ydr, <model>.ytd   (model = w_..., the 6m name)
//!   <out>/data/weaponarchetypes.meta, weapons.meta, weaponanimations.meta
//!   <out>/fxmanifest.lua
//!
//! Weapon stats, animation sets and audio are GTA V defaults per class
//! (pistol, SMG, rifle, MG, shotgun, sniper), not 6m data; melee, thrown and
//! kit items are not converted (no gun class). Untested in game.

use super::vehicles::{load_source, solid, Pools, Source, MISSING};
use super::write_atomic;
use crate::gta5::{
    yft::{self, Bone, Model, Q, V3},
    ydr::Geom,
    ytd,
};
use sixng_core::scan::Entry;
use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    fs,
    path::PathBuf,
};

pub const USAGE: &str = "\
usage: 6ixpack fivem weapon <name|all> --game <dir> --out <resource dir> [options]
  <name>               6m weapon base model (data.rpf), e.g. w_pi_scorpio_01; `all` converts every gun
  --game <dir>         GTA VI install (data.rpf, data/ttdb)
  --out <dir>          FiveM resource directory (stream/, data/, fxmanifest.lua)
  --work <dir>         sixng work dir with index/data.json (default $SIXNG_WORK or ./work)
  --max-tex <px>       drop top mips so textures are at most this size
  --list               list 6m guns in the index and exit";

struct Opts {
    names: Vec<String>,
    game: PathBuf,
    out: PathBuf,
    work: PathBuf,
    max_tex: Option<u16>,
    list: bool,
}

/// GTA V class of a 6m weapon prefix (w_<prefix>_...).
struct Class {
    group: &'static str,
    ammo: &'static str,
    clip: u32,
    damage: f32,
    between: f32,
    batch: u32,
    range: f32,
    audio: &'static str,
    aiming: &'static str,
    clipset: &'static str,
    cover: &'static str,
    two_handed: bool,
}

fn class_of(name: &str) -> Option<Class> {
    let p = name.strip_prefix("w_")?.split('_').next()?;
    let c = |group, ammo, clip, damage, between, batch, range, audio, aiming, clipset, cover, two_handed| Class { group, ammo, clip, damage, between, batch, range, audio, aiming, clipset, cover, two_handed };
    Some(match p {
        "pi" | "rv" => c("GROUP_PISTOL", "AMMO_PISTOL", if p == "rv" { 6 } else { 12 }, if p == "rv" { 60.0 } else { 26.0 }, if p == "rv" { 0.5 } else { 0.18 }, 1, 120.0, "AUDIO_ITEM_PISTOL", "PISTOL_2H_BASE_STRAFE", "weapons@pistol@pistol", "Cover_Wpn_Pistol", false),
        "smg" => c("GROUP_SMG", "AMMO_SMG", 30, 22.0, 0.08, 1, 120.0, "AUDIO_ITEM_SMG", "SMG_2H_BASE_STRAFE", "weapons@rifle@smg", "Cover_Wpn_Rifle", true),
        "ar" | "tr" | "lr" => c("GROUP_RIFLE", "AMMO_RIFLE", 30, 30.0, 0.13, 1, 500.0, "AUDIO_ITEM_ASSAULTRIFLE", "RIFLE_LO_BASE_STRAFE", "weapons@rifle@hi@carbine", "Cover_Wpn_Rifle", true),
        "mg" => c("GROUP_MG", "AMMO_MG", 100, 32.0, 0.11, 1, 500.0, "AUDIO_ITEM_COMBATMG", "MG_LOW_BASE_STRAFE", "weapons@machinegun@combat_mg", "Cover_Wpn_Rifle", true),
        "sg" => c("GROUP_SHOTGUN", "AMMO_SHOTGUN", 8, 29.0, 1.0, 8, 40.0, "AUDIO_ITEM_PUMPSHOTGUN", "SHOTGUN_LO_BASE_STRAFE", "weapons@rifle@lo@pump", "Cover_Wpn_Rifle", true),
        "sr" => c("GROUP_SNIPER", "AMMO_SNIPER", 10, 101.0, 1.0, 1, 1500.0, "AUDIO_ITEM_SNIPERRIFLE", "SNIPER_BASE_STRAFE", "weapons@rifle@hi@sniper_rifle", "Cover_Wpn_Rifle", true),
        _ => return None,
    })
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { names: Vec::new(), game: PathBuf::new(), out: PathBuf::new(), work: std::env::var_os("SIXNG_WORK").map(PathBuf::from).unwrap_or_else(|| "work".into()), max_tex: None, list: false };
    let mut it = args.iter();
    let val = |it: &mut std::slice::Iter<String>, k: &str| it.next().cloned().ok_or(format!("{k} needs a value"));
    while let Some(a) = it.next() {
        match a.as_str() {
            "--game" => o.game = val(&mut it, a)?.into(),
            "--out" => o.out = val(&mut it, a)?.into(),
            "--work" => o.work = val(&mut it, a)?.into(),
            "--max-tex" => o.max_tex = Some(val(&mut it, a)?.parse().map_err(|e| format!("{a}: {e}"))?),
            "--list" => o.list = true,
            x if !x.starts_with("--") => o.names.push(x.to_lowercase()),
            _ => return Err(format!("unknown argument {a}\n{USAGE}")),
        }
    }
    if o.game.as_os_str().is_empty() || (!o.list && (o.names.is_empty() || o.out.as_os_str().is_empty())) {
        return Err(USAGE.into());
    }
    Ok(o)
}

/// Base weapon names: `w_<class>_<maker>_<nn>` with a gun class.
fn guns<'a>(by_name: &HashMap<&'a str, &'a Entry>) -> Vec<&'a str> {
    let mut v: Vec<&str> = by_name
        .keys()
        .copied()
        .filter(|n| {
            let p: Vec<&str> = n.split('_').collect();
            p.len() == 4 && p[3].len() == 2 && p[3].bytes().all(|c| c.is_ascii_digit()) && class_of(n).is_some()
        })
        .collect();
    v.sort();
    v
}

pub fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    if args.is_empty() || args[0] == "--help" {
        println!("{USAGE}");
        return Ok(());
    }
    let o = parse(args)?;
    let t0 = std::time::Instant::now();
    let ip = sixng_core::scan::index_path(&o.work.join("index"), "data.rpf");
    let index = sixng_core::scan::load_index(&ip).map_err(|e| format!("{}: {e:#} (run `6ixpack sixng index data.rpf --work ...` first)", ip.display()))?;
    let by_name: HashMap<&str, &Entry> = index.entries.iter().filter(|e| matches!(e.rtype.as_deref(), Some("dr" | "ft"))).filter_map(|e| Some((e.name.as_deref()?, e))).collect();
    let all = guns(&by_name);
    if o.list {
        for n in &all {
            println!("{n}");
        }
        eprintln!("{} guns", all.len());
        return Ok(());
    }
    let names: Vec<String> = if o.names.iter().any(|n| n == "all") { all.iter().map(|s| s.to_string()).collect() } else { o.names.clone() };
    let rpf = o.game.join("data.rpf");
    let mut pools = Pools::new(&o.game, &o.work)?;
    let stream = o.out.join("stream");
    let data = o.out.join("data");
    fs::create_dir_all(&stream)?;
    fs::create_dir_all(&data)?;
    let mut done: Vec<(String, Class)> = Vec::new();
    for name in &names {
        let Some(class) = class_of(name) else {
            eprintln!("weapon: {name}: not a gun class (pi, rv, smg, ar, tr, lr, mg, sg, sr), skipped");
            continue;
        };
        if !by_name.contains_key(name.as_str()) {
            eprintln!("weapon: {name}: not in {} (try --list)", ip.display());
            continue;
        }
        match convert(&o, &rpf, &by_name, &mut pools, name, &stream) {
            Ok(msg) => {
                println!("weapon: {name}: {msg}");
                done.push((name.clone(), class));
            }
            Err(e) => eprintln!("weapon: {name}: {e}"),
        }
    }
    if done.is_empty() {
        return Err("no weapon converted".into());
    }
    write_atomic(&data.join("weaponarchetypes.meta"), archetypes_meta(&done).as_bytes())?;
    write_atomic(&data.join("weapons.meta"), weapons_meta(&done).as_bytes())?;
    write_atomic(&data.join("weaponanimations.meta"), animations_meta(&done).as_bytes())?;
    write_atomic(&o.out.join("fxmanifest.lua"), FXMANIFEST.as_bytes())?;
    println!("weapon: {} of {} converted in {:.1}s -> {} (give with WEAPON_{})", done.len(), names.len(), t0.elapsed().as_secs_f32(), o.out.display(), done[0].0.to_uppercase());
    Ok(())
}

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn qmul(a: Q, b: Q) -> Q {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

/// (T, R) applied to a part-space transform.
fn compose(t: (V3, Q), g: (V3, Q)) -> (V3, Q) {
    (add(t.0, yft::qrot(t.1, g.0)), qmul(t.1, g.1))
}

/// Assembled weapon: geometry per LOD in weapon space, named sockets.
#[derive(Default)]
struct Asm {
    lods: [Vec<Geom>; 3],
    sockets: BTreeMap<String, (V3, Q)>,
    parts: Vec<String>,
}

/// First variant of a part: `<w>_<slot>_01`, `_00`, else the lowest number.
fn variant<'a>(by_name: &HashMap<&'a str, &'a Entry>, w: &str, slot: &str) -> Option<String> {
    let pre = format!("{w}_{slot}_");
    let mut v: Vec<&str> = by_name.keys().copied().filter(|n| n.strip_prefix(pre.as_str()).is_some_and(|r| r.len() == 2 && r.bytes().all(|c| c.is_ascii_digit()))).collect();
    v.sort_by_key(|n| (!n.ends_with("_01"), *n));
    v.first().map(|s| s.to_string())
}

/// Decode `<n>_hi` (LOD0), `<n>_mid` (LOD1, or LOD0 when there is no _hi)
/// and `<n>` (LOD2).
fn load_lods(rpf: &std::path::Path, by_name: &HashMap<&str, &Entry>, pools: &mut Pools, n: &str, texs: &mut Vec<String>, missing: &mut Vec<String>) -> Result<(Source, [Vec<Geom>; 3]), String> {
    let get = |s: &str| by_name.get(format!("{n}{s}").as_str()).copied();
    let mut load = |e: Option<&Entry>| e.map(|e| load_source(rpf, e, pools, texs, missing)).transpose();
    let (hi, mid, base) = (load(get("_hi"))?, load(get("_mid"))?, load(get(""))?);
    let geoms = |s: &Option<Source>, l: usize| -> Vec<Geom> { s.as_ref().and_then(|s| s.lods.get(l)).map(|v| v.iter().map(|(_, g)| g.clone()).collect()).unwrap_or_default() };
    let l0 = if hi.is_some() { geoms(&hi, 0) } else { geoms(&mid, 0) };
    let l1 = if hi.is_some() { geoms(&mid, 0) } else { geoms(&mid, 1) };
    let l2 = geoms(&base, 0);
    let src = hi.or(mid).or(base).ok_or("no model")?;
    Ok((src, [l0, l1, l2]))
}

/// Place part `n` with its root at `at`, then recurse into its sockets.
#[allow(clippy::too_many_arguments)]
fn attach(rpf: &std::path::Path, by_name: &HashMap<&str, &Entry>, pools: &mut Pools, w: &str, n: &str, at: (V3, Q), asm: &mut Asm, texs: &mut Vec<String>, missing: &mut Vec<String>, depth: usize) -> Result<(), String> {
    if depth > 6 {
        return Ok(());
    }
    let (src, lods) = load_lods(rpf, by_name, pools, n, texs, missing)?;
    asm.parts.push(n.to_string());
    for (l, gs) in lods.into_iter().enumerate() {
        for mut g in gs {
            for p in g.positions.iter_mut() {
                *p = add(at.0, yft::qrot(at.1, *p));
            }
            for nn in g.normals.iter_mut() {
                *nn = yft::qrot(at.1, *nn);
            }
            asm.lods[l].push(g);
        }
    }
    let bones: Vec<Bone> = src.bones.iter().map(|b| Bone { name: b.name.clone(), parent: b.parent.map(|p| p as usize), t: b.translation, r: b.rotation }).collect();
    let g = yft::globals(&bones);
    let mut kids = Vec::new();
    for (i, b) in bones.iter().enumerate() {
        let world = compose(at, g[i]);
        asm.sockets.entry(b.name.clone()).or_insert(world);
        if let Some(slot) = b.name.strip_prefix("WAP_") {
            let slot = slot.to_lowercase();
            if let Some(p) = variant(by_name, w, &slot) {
                if !asm.parts.contains(&p) {
                    kids.push((p, world));
                }
            }
        }
    }
    for (p, world) in kids {
        if asm.parts.contains(&p) {
            continue;
        }
        if let Err(e) = attach(rpf, by_name, pools, w, &p, world, asm, texs, missing, depth + 1) {
            eprintln!("weapon: {w}: part {p}: {e}");
        }
    }
    Ok(())
}

fn convert(o: &Opts, rpf: &std::path::Path, by_name: &HashMap<&str, &Entry>, pools: &mut Pools, w: &str, stream: &std::path::Path) -> Result<String, String> {
    let (mut texs, mut missing) = (Vec::new(), Vec::new());
    let mut asm = Asm::default();
    let ident: (V3, Q) = ([0.0; 3], [0.0, 0.0, 0.0, 1.0]);
    attach(rpf, by_name, pools, w, w, ident, &mut asm, &mut texs, &mut missing, 0)?;
    if asm.lods[0].is_empty() {
        return Err("no geometry".into());
    }
    // GTA V weapon skeleton: root + sockets, all children of the root
    let sock = |names: &[&str]| names.iter().find_map(|n| asm.sockets.get(*n).copied());
    let mut bones = vec![Bone { name: "Gun_Root".into(), parent: None, t: [0.0; 3], r: [0.0, 0.0, 0.0, 1.0] }];
    let muzzle = sock(&["Wpn_VFX_Barrel", "WAP_Muzzle", "Wpn_Muzzle"]);
    let map: [(&str, Option<(V3, Q)>); 10] = [
        ("Gun_Muzzle", muzzle),
        ("Gun_Nuzzle", muzzle),
        ("Gun_GripR", sock(&["Wpn_LOC_GripPrimary", "WAP_PGrip"])),
        ("Gun_GripL", sock(&["Wpn_LOC_GripSecondary", "WAP_FGrip", "WAP_Handguard"])),
        ("Gun_VFX_Eject", sock(&["Wpn_VFX_Eject"])),
        ("WAPClip", sock(&["WAP_Mag"])),
        ("WAPSupp", muzzle),
        ("WAPScop", sock(&["WAP_Optic", "WAP_Sight_R", "WAP_Sight"])),
        ("WAPFlshLasr", sock(&["WAP_Flashlight", "WAP_Laser"])),
        ("WAPGrip", sock(&["WAP_FGrip", "WAP_Grip", "WAP_Frontrail"])),
    ];
    for (n, s) in map {
        if let Some((t, r)) = s {
            bones.push(Bone { name: n.into(), parent: Some(0), t, r });
        }
    }
    let mut lods: [Vec<Model>; 4] = Default::default();
    for (l, gs) in asm.lods.iter().enumerate() {
        if !gs.is_empty() {
            lods[l] = vec![Model { bone: 0, geoms: gs.clone() }];
        }
    }
    if lods[1].is_empty() {
        lods[1] = lods[0].clone();
    }
    let ydr = yft::build_ydr_skel(w, &bones, &lods, [10.0, 40.0, 150.0, 500.0])?;
    write_atomic(&stream.join(format!("{w}.ydr")), &ydr).map_err(|e| e.to_string())?;
    let mut tx = Vec::new();
    for t in &texs {
        if t == MISSING || !t.starts_with("r6m_") || t.len() != 12 {
            continue;
        }
        let p = pools.dir.join(format!("{}.dds", &t[4..]));
        let Ok(bytes) = fs::read(&p) else { continue };
        let mut x = ytd::from_dds(t, &bytes)?;
        if let Some(m) = o.max_tex {
            x.limit_size(m);
        }
        tx.push(x);
    }
    if asm.lods.iter().flatten().any(|g| g.texture == MISSING || !texs.contains(&g.texture)) || tx.is_empty() {
        tx.push(solid(MISSING, [0x40, 0x40, 0x40, 0xFF], 16));
    }
    // paint-slot shaders (rare on weapons) fall back to a dark grey
    if asm.lods.iter().flatten().any(|g| g.texture == "r6m_paint") {
        tx.push(solid("r6m_paint", [0x30, 0x30, 0x30, 0xFF], 16));
    }
    let ytd = ytd::build_ytd(&tx)?;
    write_atomic(&stream.join(format!("{w}.ytd")), &ytd).map_err(|e| e.to_string())?;
    let tris: usize = asm.lods[0].iter().map(|g| g.indices.len() / 3).sum();
    Ok(format!("{} parts, {tris} tris, {} textures{}, bones {:?}", asm.parts.len(), tx.len(), if missing.is_empty() { String::new() } else { format!(" ({} missing)", missing.len()) }, bones.iter().map(|b| b.name.as_str()).collect::<Vec<_>>()))
}

// ------------------------------------------------------------------ meta

const FXMANIFEST: &str = "fx_version 'cerulean'
game 'gta5'

files {
  'data/weaponarchetypes.meta',
  'data/weapons.meta',
  'data/weaponanimations.meta',
}

data_file 'WEAPON_METADATA_FILE' 'data/weaponarchetypes.meta'
data_file 'WEAPONINFO_FILE' 'data/weapons.meta'
data_file 'WEAPON_ANIMATIONS_FILE' 'data/weaponanimations.meta'
";

fn archetypes_meta(done: &[(String, Class)]) -> String {
    let items: String = done
        .iter()
        .map(|(n, _)| format!("    <Item>\n      <modelName>{n}</modelName>\n      <txdName>{n}</txdName>\n      <ptfxAssetName>NULL</ptfxAssetName>\n      <lodDist value=\"500.000000\" />\n    </Item>\n"))
        .collect();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CWeaponModelInfo__InitDataList>\n  <InitDatas>\n{items}  </InitDatas>\n</CWeaponModelInfo__InitDataList>\n")
}

fn weapons_meta(done: &[(String, Class)]) -> String {
    let mut items = String::new();
    for (n, c) in done {
        let id = format!("WEAPON_{}", n.to_uppercase());
        let flags = if c.two_handed { "CarriedInHand Gun CanLockonOnFoot CanLockonInVehicle CanFreeAim TwoHanded AnimReload AnimCrouchFire UsableOnFoot UsableClimbing DoesRevivableDamage HasLowCoverReloads HasLowCoverSwaps LongWeapon UseFPSAimIK UseFPSSecondaryMotion" } else { "CarriedInHand Gun CanLockonOnFoot CanLockonInVehicle CanFreeAim AnimReload AnimCrouchFire UsableOnFoot UsableClimbing UsableInCover AllowEarlyExitFromFireAnimAfterBulletFired DoesRevivableDamage QuitTransitionToIdleIntroOnWeaponChange DisableLeftHandIkWhenOnFoot UseFPSAimIK UseFPSSecondaryMotion" };
        items += &format!(
            r#"        <Item type="CWeaponInfo">
          <Name>{id}</Name>
          <Model>{n}</Model>
          <Audio>{audio}</Audio>
          <Slot>SLOT_{up}</Slot>
          <DamageType>BULLET</DamageType>
          <Explosion>
            <Default>DONTCARE</Default>
            <HitCar>DONTCARE</HitCar>
            <HitTruck>DONTCARE</HitTruck>
            <HitBike>DONTCARE</HitBike>
            <HitBoat>DONTCARE</HitBoat>
            <HitPlane>DONTCARE</HitPlane>
          </Explosion>
          <FireType>INSTANT_HIT</FireType>
          <WheelSlot>WHEEL_{wheel}</WheelSlot>
          <Group>{group}</Group>
          <AmmoInfo ref="{ammo}" />
          <AimingInfo ref="{aiming}" />
          <ClipSize value="{clip}" />
          <AccuracySpread value="2.000000" />
          <AccurateModeAccuracyModifier value="0.500000" />
          <RunAndGunAccuracyModifier value="2.000000" />
          <RunAndGunAccuracyMaxModifier value="1.000000" />
          <RecoilAccuracyMax value="0.500000" />
          <RecoilErrorTime value="3.000000" />
          <RecoilRecoveryRate value="1.000000" />
          <RecoilAccuracyToAllowHeadShotAI value="1000.000000" />
          <MinHeadShotDistanceAI value="1000.000000" />
          <MaxHeadShotDistanceAI value="1000.000000" />
          <HeadShotDamageModifierAI value="1000.000000" />
          <RecoilAccuracyToAllowHeadShotPlayer value="0.175000" />
          <MinHeadShotDistancePlayer value="5.000000" />
          <MaxHeadShotDistancePlayer value="40.000000" />
          <HeadShotDamageModifierPlayer value="18.000000" />
          <Damage value="{damage:.6}" />
          <DamageTime value="0.000000" />
          <DamageTimeInVehicle value="0.000000" />
          <DamageTimeInVehicleHeadShot value="0.000000" />
          <HitLimbsDamageModifier value="0.500000" />
          <NetworkHitLimbsDamageModifier value="0.800000" />
          <LightlyArmouredDamageModifier value="0.750000" />
          <Force value="40.000000" />
          <ForceHitPed value="140.000000" />
          <ForceHitVehicle value="750.000000" />
          <ForceHitFlyingHeli value="750.000000" />
          <ForceMaxStrengthMult value="1.000000" />
          <ForceFalloffRangeStart value="0.000000" />
          <ForceFalloffRangeEnd value="50.000000" />
          <ForceFalloffMin value="1.000000" />
          <ProjectileForce value="0.000000" />
          <FragImpulse value="600.000000" />
          <Penetration value="0.100000" />
          <VerticalLaunchAdjustment value="0.000000" />
          <DropForwardVelocity value="0.000000" />
          <Speed value="2000.000000" />
          <BulletsInBatch value="{batch}" />
          <BatchSpread value="{spread:.6}" />
          <ReloadTimeMP value="-1.000000" />
          <ReloadTimeSP value="-1.000000" />
          <VehicleReloadTime value="1.000000" />
          <AnimReloadRate value="1.000000" />
          <BulletsPerAnimLoop value="1" />
          <TimeBetweenShots value="{between:.6}" />
          <TimeLeftBetweenShotsWhereShouldFireIsCached value="-1.000000" />
          <SpinUpTime value="0.000000" />
          <SpinTime value="0.000000" />
          <SpinDownTime value="0.000000" />
          <AlternateWaitTime value="-1.000000" />
          <BulletBendingNearRadius value="0.000000" />
          <BulletBendingFarRadius value="0.750000" />
          <BulletBendingZoomedRadius value="0.375000" />
          <FirstPersonBulletBendingNearRadius value="0.000000" />
          <FirstPersonBulletBendingFarRadius value="0.750000" />
          <FirstPersonBulletBendingZoomedRadius value="0.375000" />
          <Fx>
            <EffectGroup>WEAPON_EFFECT_GROUP_{fx}</EffectGroup>
            <FlashFx>muz_{muz}</FlashFx>
            <FlashFxAlt />
            <FlashFxFP />
            <FlashFxAltFP />
            <MuzzleSmokeFx>muz_smoking_barrel</MuzzleSmokeFx>
            <MuzzleSmokeFxFP />
            <MuzzleSmokeFxMinLevel value="0.300000" />
            <MuzzleSmokeFxIncPerShot value="0.100000" />
            <MuzzleSmokeFxDecPerSec value="0.250000" />
            <ShellFx>eject_{eject}</ShellFx>
            <ShellFxFP />
            <TracerFx>bullet_tracer</TracerFx>
            <PedDamageHash>BulletLarge</PedDamageHash>
            <TracerFxChanceSP value="0.150000" />
            <TracerFxChanceMP value="0.750000" />
            <FlashFxChanceSP value="1.000000" />
            <FlashFxChanceMP value="1.000000" />
            <FlashFxAltChance value="0.000000" />
            <FlashFxScale value="1.000000" />
            <FlashFxLightEnabled value="true" />
            <FlashFxLightCastsShadows value="false" />
            <FlashFxLightOffsetDist value="0.000000" />
            <FlashFxLightRGBAMin x="255.000000" y="93.000000" z="25.000000" />
            <FlashFxLightRGBAMax x="255.000000" y="100.000000" z="50.000000" />
            <FlashFxLightIntensityMinMax x="1.000000" y="2.000000" />
            <FlashFxLightRangeMinMax x="2.000000" y="2.500000" />
            <FlashFxLightFalloffMinMax x="1024.000000" y="1536.000000" />
            <GroundDisturbFxEnabled value="false" />
            <GroundDisturbFxDist value="5.000000" />
            <GroundDisturbFxNameDefault />
            <GroundDisturbFxNameSand />
            <GroundDisturbFxNameDirt />
            <GroundDisturbFxNameWater />
            <GroundDisturbFxNameFoliage />
          </Fx>
          <InitialRumbleDuration value="90" />
          <InitialRumbleIntensity value="0.100000" />
          <InitialRumbleIntensityTrigger value="0.950000" />
          <RumbleDuration value="90" />
          <RumbleIntensity value="0.100000" />
          <RumbleIntensityTrigger value="0.800000" />
          <RumbleDamageIntensity value="1.000000" />
          <NetworkPlayerDamageModifier value="1.000000" />
          <NetworkPedDamageModifier value="1.000000" />
          <NetworkHeadShotPlayerDamageModifier value="1.000000" />
          <LockOnRange value="{lock:.6}" />
          <WeaponRange value="{range:.6}" />
          <BulletDirectionOffsetInDegrees value="0.000000" />
          <AiSoundRange value="-1.000000" />
          <AiPotentialBlastEventRange value="-1.000000" />
          <DamageFallOffRangeMin value="{fmin:.6}" />
          <DamageFallOffRangeMax value="{range:.6}" />
          <DamageFallOffModifier value="0.300000" />
          <VehicleWeaponHash />
          <DefaultCameraHash>DEFAULT_THIRD_PERSON_PED_AIM_CAMERA</DefaultCameraHash>
          <CoverCameraHash>DEFAULT_THIRD_PERSON_PED_AIM_IN_COVER_CAMERA</CoverCameraHash>
          <CoverReadyToFireCameraHash />
          <RunAndGunCameraHash>DEFAULT_THIRD_PERSON_PED_RUN_AND_GUN_CAMERA</RunAndGunCameraHash>
          <CinematicShootingCameraHash>DEFAULT_THIRD_PERSON_PED_CINEMATIC_SHOOTING_CAMERA</CinematicShootingCameraHash>
          <AlternativeOrScopedCameraHash />
          <RunAndGunAlternativeOrScopedCameraHash />
          <CinematicShootingAlternativeOrScopedCameraHash />
          <CameraFov value="45.000000" />
          <FirstPersonScopeFov value="20.00000" />
          <FirstPersonScopeAttachmentFov value="20.00000" />
          <ReticuleHudPosition x="0.000000" y="0.000000" />
          <AimOffsetMin x="0.250000" y="0.200000" z="0.600000" />
          <AimProbeLengthMin value="0.430000" />
          <AimOffsetMax x="0.165000" y="-0.180000" z="0.470000" />
          <AimProbeLengthMax value="0.340000" />
          <TorsoAimOffset x="-1.000000" y="0.550000" />
          <TorsoCrouchedAimOffset x="0.100000" y="0.120000" />
          <LeftHandIkOffset x="0.015000" y="0.095000" z="-0.008000" />
          <ReticuleMinSizeStanding value="0.600000" />
          <ReticuleMinSizeCrouched value="0.500000" />
          <ReticuleScale value="0.300000" />
          <ReticuleStyleHash>WEAPON_{reticule}</ReticuleStyleHash>
          <FirstPersonReticuleStyleHash />
          <PickupHash>PICKUP_{pickup}</PickupHash>
          <MPPickupHash>PICKUP_AMMO_BULLET_MP</MPPickupHash>
          <HumanNameHash>{id}</HumanNameHash>
          <MovementModeConditionalIdle>MMI_{mmi}</MovementModeConditionalIdle>
          <StatName>{stat}</StatName>
          <KnockdownCount value="-1" />
          <KillshotImpulseScale value="1.000000" />
          <NmShotTuningSet>Normal</NmShotTuningSet>
          <AttachPoints />
          <GunFeedBone />
          <TargetSequenceGroup />
          <WeaponFlags>{flags}</WeaponFlags>
          <TintSpecValues ref="TINT_DEFAULT" />
          <FiringPatternAliases ref="FIRING_PATTERN_{fp}" />
          <ReloadUpperBodyFixupExpressionData ref="default" />
          <AmmoDiminishingRate value="3" />
          <AimingBreathingAdditiveWeight value="1.000000" />
          <FiringBreathingAdditiveWeight value="1.000000" />
          <StealthAimingBreathingAdditiveWeight value="1.000000" />
          <StealthFiringBreathingAdditiveWeight value="1.000000" />
          <AimingLeanAdditiveWeight value="1.000000" />
          <FiringLeanAdditiveWeight value="1.000000" />
          <StealthAimingLeanAdditiveWeight value="1.000000" />
          <StealthFiringLeanAdditiveWeight value="1.000000" />
          <ExpandPedCapsuleRadius value="0.000000" />
          <AudioCollisionHash />
          <HudDamage value="40" />
          <HudSpeed value="40" />
          <HudCapacity value="20" />
          <HudAccuracy value="40" />
          <HudRange value="25" />
        </Item>
"#,
            audio = c.audio,
            up = n.to_uppercase(),
            wheel = match c.group {
                "GROUP_PISTOL" => "PISTOL",
                "GROUP_SMG" => "SMG",
                "GROUP_SHOTGUN" => "SHOTGUN",
                "GROUP_SNIPER" => "SNIPER",
                "GROUP_MG" => "SMG",
                _ => "RIFLE",
            },
            group = c.group,
            ammo = c.ammo,
            aiming = c.aiming,
            clip = c.clip,
            damage = c.damage,
            batch = c.batch,
            spread = if c.batch > 1 { 0.1 } else { 0.0 },
            between = c.between,
            fx = match c.group {
                "GROUP_PISTOL" => "PISTOL_SMALL",
                "GROUP_SMG" => "SMG",
                "GROUP_SHOTGUN" => "SHOTGUN",
                "GROUP_SNIPER" => "RIFLE_SNIPER",
                _ => "RIFLE_ASSAULT",
            },
            muz = match c.group {
                "GROUP_PISTOL" => "pistol",
                "GROUP_SHOTGUN" => "shotgun",
                "GROUP_SMG" => "smg",
                "GROUP_MG" => "mg",
                _ => "assault_rifle",
            },
            eject = match c.group {
                "GROUP_PISTOL" | "GROUP_SMG" => "pistol",
                "GROUP_SHOTGUN" => "shotgun",
                _ => "auto",
            },
            lock = c.range.min(120.0),
            range = c.range,
            fmin = c.range * 0.4,
            reticule = match c.group {
                "GROUP_PISTOL" => "PISTOL",
                "GROUP_SHOTGUN" => "SHOTGUN",
                "GROUP_SNIPER" => "SNIPER",
                _ => "RIFLE",
            },
            pickup = match c.group {
                "GROUP_PISTOL" => "WEAPON_PISTOL",
                "GROUP_SMG" => "WEAPON_SMG",
                "GROUP_SHOTGUN" => "WEAPON_PUMPSHOTGUN",
                "GROUP_SNIPER" => "WEAPON_SNIPERRIFLE",
                "GROUP_MG" => "WEAPON_COMBATMG",
                _ => "WEAPON_CARBINERIFLE",
            },
            mmi = if c.two_handed { "LONG" } else { "SHORT" },
            stat = match c.group {
                "GROUP_PISTOL" => "PISTOL",
                "GROUP_SMG" => "SMG",
                "GROUP_SHOTGUN" => "PUMP",
                "GROUP_SNIPER" => "SNIPERRFL",
                "GROUP_MG" => "CMBTMG",
                _ => "CRBNRIFLE",
            },
            fp = match c.group {
                "GROUP_PISTOL" | "GROUP_SHOTGUN" | "GROUP_SNIPER" => "PISTOL",
                _ => "RIFLE",
            },
        );
    }
    let slots: String = done.iter().map(|(n, _)| format!("    <Item>SLOT_{}</Item>\n", n.to_uppercase())).collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CWeaponInfoBlob>\n  <SlotNavigateOrder>\n    <Item>\n      <WeaponSlots>\n{s1}      </WeaponSlots>\n    </Item>\n    <Item>\n      <WeaponSlots>\n{s1}      </WeaponSlots>\n    </Item>\n  </SlotNavigateOrder>\n  <SlotBestOrder>\n    <WeaponSlots>\n{s2}    </WeaponSlots>\n  </SlotBestOrder>\n  <TintSpecValues />\n  <FiringPatternAliases />\n  <UpperBodyFixupExpressionData />\n  <AimingInfos />\n  <Infos>\n    <Item>\n      <Infos>\n{items}      </Infos>\n    </Item>\n    <Item>\n      <Infos />\n    </Item>\n    <Item>\n      <Infos />\n    </Item>\n  </Infos>\n  <VehicleWeaponInfos />\n  <Name>r6m weapons</Name>\n</CWeaponInfoBlob>\n",
        s1 = done.iter().enumerate().map(|(i, (n, _))| format!("        <Item>\n          <OrderNumber value=\"{}\" />\n          <Entry>SLOT_{}</Entry>\n        </Item>\n", 900 + i, n.to_uppercase())).collect::<String>(),
        s2 = slots,
    )
}

fn animations_meta(done: &[(String, Class)]) -> String {
    let items: String = done
        .iter()
        .map(|(n, c)| {
            format!(
                "          <Item key=\"WEAPON_{}\">\n            <CoverMovementClipSetHash />\n            <CoverMovementExtraClipSetHash />\n            <CoverAlternateMovementClipSetHash />\n            <CoverWeaponClipSetHash>{}</CoverWeaponClipSetHash>\n            <MotionClipSetHash>{cs}</MotionClipSetHash>\n            <MotionFilterHash />\n            <MotionCrouchClipSetHash />\n            <MotionStrafingClipSetHash />\n            <MotionStrafingStealthClipSetHash />\n            <MotionStrafingUpperBodyClipSetHash />\n            <WeaponClipSetHash>{cs}</WeaponClipSetHash>\n            <WeaponClipSetStreamedHash />\n            <WeaponClipSetHashInjured />\n            <WeaponClipSetHashStealth />\n            <WeaponClipSetHashHiCover />\n            <AlternativeClipSetWhenBlocked />\n            <ScopeWeaponClipSet />\n            <AlternateAimingStandingClipSetHash />\n            <AlternateAimingCrouchingClipSetHash />\n            <FiringVariationsStandingClipSetHash />\n            <FiringVariationsCrouchingClipSetHash />\n            <AimTurnStandingClipSetHash />\n            <AimTurnCrouchingClipSetHash />\n            <MeleeClipSetHash />\n            <MeleeVariationClipSetHash />\n            <MeleeTauntClipSetHash />\n            <MeleeSupportTauntClipSetHash />\n            <MeleeStealthClipSetHash />\n            <ShellShockedClipSetHash />\n            <JumpUpperbodyClipSetHash />\n            <FallUpperbodyClipSetHash />\n            <FromStrafeTransitionUpperBodyClipSetHash />\n            <SwapWeaponClipSetHash />\n            <AnimFireRateModifier value=\"1.000000\" />\n            <AnimBlindFireRateModifier value=\"1.000000\" />\n            <AnimWantingToShootFireRateModifier value=\"-1.000000\" />\n            <UseFromStrafeUpperBodyAimNetwork value=\"true\" />\n            <AimingDownTheBarrel value=\"true\" />\n          </Item>\n",
                n.to_uppercase(),
                c.cover,
                cs = c.clipset,
            )
        })
        .collect();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CWeaponAnimationsSets>\n  <WeaponAnimationsSets>\n    <Item key=\"Default\">\n      <Fallback />\n      <WeaponAnimations>\n{items}      </WeaponAnimations>\n    </Item>\n  </WeaponAnimationsSets>\n</CWeaponAnimationsSets>\n")
}
