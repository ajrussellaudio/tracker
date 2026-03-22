use anyhow::Result;

/// The action to take on startup, derived from CLI arguments.
#[derive(Debug, PartialEq)]
pub enum CliAction {
    /// Show the interactive startup screen (no arguments given).
    ShowStartup,
    /// Open the given `.trk` file directly, skipping the startup screen.
    OpenFile(std::path::PathBuf),
}

/// Parse command-line arguments into a [`CliAction`].
///
/// Returns `Err` for unknown flags (including the removed `--sample`).
pub fn parse_args(args: &[String]) -> Result<CliAction> {
    // Skip argv[0] (program name).
    let args = match args.first() {
        // If the first element looks like a program path, skip it.
        Some(first) if first.contains('/') || first.contains('\\') || !first.ends_with(".trk") => {
            &args[1..]
        }
        _ => args,
    };
    if args.is_empty() {
        return Ok(CliAction::ShowStartup);
    }
    for arg in args {
        if arg.starts_with('-') {
            if arg == "--sample" {
                anyhow::bail!(
                    "`--sample` has been removed.\n\
                     Launch with `vitakt path/to/project.trk` to open a project directly,\n\
                     or `vitakt` with no arguments to show the startup screen."
                );
            }
            anyhow::bail!("Unknown flag: {arg}\nUsage: vitakt [path.trk]");
        }
    }
    Ok(CliAction::OpenFile(std::path::PathBuf::from(&args[0])))
}
