use clap::{Arg, ArgAction, ArgMatches, Command};
use log::{info, LevelFilter};
use std::sync::atomic::{AtomicBool, Ordering};
use syslog::{BasicLogger, Facility, Formatter3164};

static DAEMON: AtomicBool = AtomicBool::new(true);

pub fn add_args(app_args: Command) -> Command {
    app_args.arg(
        Arg::new("NO_SYSLOG")
            .long("no-syslog")
            .action(ArgAction::SetTrue)
            .help("Don't use syslog for logging"),
    )
}

pub fn start(args: &ArgMatches) {
    DAEMON.store(
        !args.get_one::<bool>("NO_SYSLOG").unwrap_or(&false),
        Ordering::Relaxed,
    );
    if DAEMON.load(Ordering::Relaxed) {
        let formatter = Formatter3164 {
            facility: Facility::LOG_USER,
            hostname: None,
            process: "tcp2serial".into(),
            pid: 0,
        };
        let logger = match syslog::unix(formatter) {
            Err(e) => {
                eprintln!("Failed to start logging: {}", e);
                return;
            }
            Ok(l) => l,
        };
        log::set_boxed_logger(Box::new(BasicLogger::new(logger)))
            .map(|()| log::set_max_level(LevelFilter::Info)).unwrap();
    } else {
        tracing_subscriber::fmt::init();
    }
    info!("Server starting");
}
pub fn ready() {
    info!("Server ready");
}

pub fn exiting() {
    info!("Server exiting");
    log::logger().flush()
}
