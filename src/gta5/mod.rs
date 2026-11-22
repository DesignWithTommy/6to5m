//! GTA V (legacy PC) resource writers: RSC7 container, .ytd, .ydr, .ybn,
//! .ymap/.ytyp. Formats ported from CodeWalker (dexyfex, MIT).

pub mod fivem;
pub mod meta;
pub mod ped;
pub mod ped_bones;
pub mod rsc;
pub mod ybn;
pub mod ydr;
pub mod yft;
pub mod ytd;

pub const USAGE: &str = "\
usage: 6ixpack convert <kind> <args...>
  ytd  <dds-dir> <out.ytd>                 DDS files -> texture dictionary
  ydr  <in.obj> <out.ydr> [--ytd <embed>]  OBJ -> drawable
  ybn  <in.obj> <out.ybn>                  OBJ -> collision bounds
  ymap <in.json> <out.ymap>                map entities -> ymap
  ytyp <in.json> <out.ytyp>                archetypes -> ytyp";

pub fn run(args: &[std::ffi::OsString]) -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    let r = match args.first().map(String::as_str) {
        Some("ytd") => ytd::cli(rest),
        Some("ydr") => ydr::cli(rest),
        Some("ybn") => ybn::cli(rest),
        Some("ymap") | Some("ytyp") => meta::cli(&args),
        _ => {
            println!("{USAGE}");
            return Ok(());
        }
    };
    r.map_err(Into::into)
}
