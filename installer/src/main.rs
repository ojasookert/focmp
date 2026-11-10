#![cfg_attr(windows, windows_subsystem = "windows")]
mod art;
mod gui;
mod install;
mod manifest;
mod steam;
mod vdf;
type Result<T> = std::result::Result<T, String>;
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() {
        if let Err(e) = gui::run() {
            eprintln!("{e}");
            std::process::exit(1)
        }
        return;
    }
    if let Err(e) = cli(&args) {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
fn cli(args: &[String]) -> Result<()> {
    let mut m = manifest::parse(manifest::EMBEDDED_MANIFEST)?;
    if args == ["--license"] {
        println!("{}", include_str!("../../LICENSE"));
        return Ok(());
    }
    if args == ["--version"] {
        println!("FOCMP installer {}", m.version);
        return Ok(());
    }
    if args == ["--help"] || args == ["-h"] {
        println!(
            "FOCMP installer {}\n\nUsage: focmp-installer [status|install|uninstall|launch|check-updates]\n  --game-dir PATH       Game directory (otherwise detected through Steam)\n  --steam-root PATH     Use a specific Steam installation\n  --no-proton-config    Skip the Proton DLL override (manual setup)\n  --manifest PATH       Use a local release manifest\n  --package PATH        Use its matching local ZIP\n\nNo arguments opens the graphical installer. Close the game before installation.\nOn Linux, start the game once first so Proton creates its prefix.",
            m.version
        );
        return Ok(());
    }
    let command = &args[0];
    let mut game = None;
    let mut root = None;
    let mut package = None;
    let mut configure = !cfg!(windows);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--no-proton-config" | "--no-steam-config" => configure = false,
            "--game-dir" | "--steam-root" | "--manifest" | "--package" => {
                let opt = &args[i];
                i += 1;
                let value = args.get(i).ok_or(format!("Missing value for {opt}"))?;
                let p = std::path::PathBuf::from(value);
                match opt.as_str() {
                    "--game-dir" => game = Some(p),
                    "--steam-root" => root = Some(p),
                    "--package" => package = Some(p),
                    _ => m = manifest::parse(&std::fs::read(p).map_err(|e| e.to_string())?)?,
                }
            }
            other => return Err(format!("Unknown option: {other}")),
        }
        i += 1;
    }
    if command == "check-updates" {
        let latest = manifest::latest()?;
        println!(
            "Latest public release: {} (bundled: {})",
            latest.version, m.version
        );
        return Ok(());
    }
    if command == "launch" {
        return steam::launch();
    }
    let steam = steam::Steam::find(root.as_deref());
    let game = game
        .or_else(|| steam.as_ref()?.game())
        .ok_or("Game not found; specify --game-dir")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    match command.as_str() {
        "status" => {
            let r = install::record(&game)?;
            println!(
                "Game: {}\nBundled/selected: {}\nInstalled: {}",
                game.display(),
                m.version,
                r.map(|r| r.version)
                    .unwrap_or("not managed by installer".into())
            );
            Ok(())
        }
        "install" => {
            if configure {
                steam::prepare(steam.as_ref(), &game)?;
            }
            let message = install::install(&game, &m, package.as_deref())?;
            if configure {
                steam::configure(steam.as_ref(), &game).map_err(|e| {
                    format!("Mod files installed, but the Proton setup needs attention: {e}")
                })?;
            }
            println!("{message}");
            Ok(())
        }
        "uninstall" => {
            println!("{}", install::uninstall(&game)?);
            Ok(())
        }
        _ => Err(format!("Unknown command: {command}. Use --help.")),
    }
}
