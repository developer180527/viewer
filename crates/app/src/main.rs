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
    --data-dir <DIR>        Keep the recent files and settings in DIR (with
                            --screenshot, which otherwise uses none)
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
    pub data_dir: Option<PathBuf>,
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
            Some("--data-dir") => {
                args.data_dir = Some(it.next().ok_or("--data-dir needs a folder")?.into());
            }
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
    // The session wakes the window through WAKE, which exists once the event loop
    // does: a load that finishes first is seen at the first poll.
    let hooks = viewer_core::Hooks::new(std::sync::Arc::new(|| {
        if let Some(wake) = lg::WAKE.get() {
            wake();
        }
    }))
    .with_trace(startup::mark);
    let mut session = viewer_core::Session::new(hooks);
    // The recent list and the settings; for --screenshot only when named, since its
    // pictures must not depend on what was opened before.
    let dir = args.data_dir.clone().or_else(|| if args.screenshot.is_none() { data_dir() } else { None });
    if let Some(dir) = &dir {
        session.use_data_dir(dir);
    }
    // Kick off decoding before the window exists; it overlaps GPU initialisation.
    if let Some(p) = args.path.clone() {
        session.open_path_as(p, args.view_as.clone());
    } else if dir.is_some() {
        session.open_last();
    }

    if let Err(e) = run(args, session) {
        eprintln!("viewer: {e}");
        std::process::exit(1);
    }
}

/// Where the recent list and the settings are kept: the platform's place for an app's
/// own files.
fn data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        home.map(|h| h.join("Library/Application Support/Viewer"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Viewer"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| home.map(|h| h.join(".config"))).map(|d| d.join("viewer"))
    }
}

fn window_title(args: &Args) -> String {
    args.path
        .as_ref()
        .and_then(|p| p.file_name())
        .map_or("Viewer".into(), |n| n.to_string_lossy().into_owned())
}

fn run(args: Args, session: viewer_core::Session) -> anyhow::Result<()> {
    lg::run(window_title(&args), args, session)
}

/// Writes `--screenshot` output.
pub fn save_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) {
    match image::save_buffer(path, rgba, width, height, image::ExtendedColorType::Rgba8) {
        Ok(()) => eprintln!("screenshot saved to {}", path.display()),
        Err(e) => eprintln!("viewer: saving screenshot: {e}"),
    }
}
