use clap::Command;

fn main() {
    let cli_app = Command::new("t-chain")
        .version("0.0.1")
        .author("Your Name")
        .about("T-chain CLI application used to start and interact with t-chain")
        .subcommand(
            Command::new("start")
                .about("Starts a t-chain daemon")
                .subcommand(Command::new("node").about("Starts a t-chain node process")),
        );

    let matches = cli_app.get_matches();

    match matches.subcommand() {
        Some(("start", start_matches)) => match start_matches.subcommand() {
            Some(("node", _)) => {
                println!("Starting node");
            }
            _ => unreachable!("Invariance violation detected"),
        },
        _ => unreachable!("Invariance violation detected"),
    }
}
