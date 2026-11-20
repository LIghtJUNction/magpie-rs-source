//! Compatibility spelling for the active Usage plugin's registered command.
pub fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let args: Vec<_> = args.collect();
    if args == ["--help"] || args == ["-h"] {
        println!("{}", magpie_plugin_usage::command::HELP);
        return Ok(());
    }
    let result = crate::plugins::call(magpie_plugin_usage::PLUGIN_ID, "report", &args)?;
    // Preserve the established report shape for existing callers.
    println!("{result}");
    Ok(())
}
