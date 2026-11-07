//! Viewer: open any file.
//!
//! Startup is ordered for speed (PLAN.md §7): the file is mapped and decoded on a
//! background thread that starts before the window and GPU are initialised.

#[cfg(unix)]
mod alloc;

#[cfg(unix)]
#[global_allocator]
static ALLOCATOR: alloc::Allocator = alloc::Allocator;

mod formats;
mod loading;
mod startup;

mod lg;

use std::path::PathBuf;

const HELP: &str = "\
viewer — open any file

USAGE:
    viewer [OPTIONS] [FILE]

OPTIONS:
    --view-as <ID>          Force a loader (e.g. hex, text, json, image), or show a
                            facet of the file by name (e.g. Stage, Source, Raw)
    --trace-startup         Print startup timings to stderr
    --benchmark             Trace startup and exit after the first frame showing the file
    --screenshot <PNG>      Save a screenshot once the file is shown, then exit
    --info                  Open with the info panel shown
    --size <WxH>            Window size in points for --screenshot (default 1100x760)
    --list-loaders          Print the available loaders
    -h, --help              Show this help
    -V, --version           Show version
";

#[derive(Default)]
pub struct Args {
    pub path: Option<PathBuf>,
    pub view_as: Option<String>,
    pub benchmark: bool,
    pub screenshot: Option<PathBuf>,
    pub size: Option<(f32, f32)>,
    pub info: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut trace = false;
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                print!("{HELP}");
                std::process::exit(0);
            }
            Some("-V" | "--version") => {
                println!("viewer {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            Some("--list-loaders") => {
                for l in formats::registry().loaders() {
                    println!("{:<10} {}", l.id(), l.name());
                }
                std::process::exit(0);
            }
            Some("--trace-startup") => trace = true,
            Some("--benchmark") => {
                args.benchmark = true;
                trace = true;
            }
            Some("--view-as") => {
                let id = it.next().ok_or("--view-as needs a loader id")?;
                args.view_as = Some(id.to_string_lossy().into_owned());
            }
            Some("--screenshot") => {
                args.screenshot = Some(it.next().ok_or("--screenshot needs a path")?.into());
            }
            Some("--info") => args.info = true,
            Some("--size") => {
                let v = it.next().ok_or("--size needs WxH")?;
                let v = v.to_string_lossy();
                let parsed = v.split_once('x').and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)));
                match parsed {
                    Some((w, h)) if w >= 1.0 && h >= 1.0 => args.size = Some((w, h)),
                    _ => return Err(format!("--size wants WxH, like 390x844, not {v}")),
                }
            }
            Some(s) if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ if args.path.is_none() => args.path = Some(arg.into()),
            _ => return Err("only one file can be opened at a time".into()),
        }
    }
    if args.size.is_some() && args.screenshot.is_none() {
        return Err("--size is for --screenshot".into());
    }
    if args.view_as.is_some() && args.path.is_none() {
        return Err("--view-as needs a file to open".into());
    }
    // Not a loader: a facet name, checked once the file is open.
    startup::set_tracing(trace);
    Ok(args)
}

fn main() {
    startup::init();
    let args = parse_args().unwrap_or_else(|e| {
        eprintln!("viewer: {e}\n\n{HELP}");
        std::process::exit(2);
    });

    // The font list, wanted only once a character Inter lacks is drawn.
    lg::scan_system_fonts();
    // Kick off decoding before the window exists; it overlaps GPU initialisation.
    let job = args
        .path
        .clone()
        .map(|p| loading::Job::open_path(p, args.view_as.clone()));

    if let Err(e) = run(args, job) {
        eprintln!("viewer: {e}");
        std::process::exit(1);
    }
}

fn window_title(args: &Args) -> String {
    args.path
        .as_ref()
        .and_then(|p| p.file_name())
        .map_or("Viewer".into(), |n| n.to_string_lossy().into_owned())
}

fn run(args: Args, job: Option<loading::Job>) -> anyhow::Result<()> {
    lg::run(window_title(&args), args, job)
}

/// Writes `--screenshot` output.
pub fn save_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) {
    match image::save_buffer(path, rgba, width, height, image::ExtendedColorType::Rgba8) {
        Ok(()) => eprintln!("screenshot saved to {}", path.display()),
        Err(e) => eprintln!("viewer: saving screenshot: {e}"),
    }
}
