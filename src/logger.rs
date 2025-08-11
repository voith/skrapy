// src/logger.rs
use chrono::Local;
use fern::colors::{Color, ColoredLevelConfig};
use log::LevelFilter;
use std::sync::OnceLock;

static LOGGER_STARTED: OnceLock<()> = OnceLock::new();

pub fn init_logger(level: LevelFilter) -> Result<(), fern::InitError> {
    // If already initialized, just update the max level and return.
    if LOGGER_STARTED.get().is_some() {
        log::set_max_level(level);
        return Ok(());
    }

    let colors = ColoredLevelConfig::new()
        .info(Color::Green)
        .debug(Color::Blue)
        .warn(Color::Yellow)
        .error(Color::Red);

    let dispatch = fern::Dispatch::new()
        .level(level)
        .level_for("hyper", LevelFilter::Warn)
        .level_for("reqwest", LevelFilter::Warn)
        .chain(
            fern::Dispatch::new()
                .format(move |out, message, record| {
                    out.finish(format_args!(
                        "{} {:<5} {}",
                        Local::now().format("%Y-%m-%dT%H:%M:%S"),
                        colors.color(record.level()),
                        message
                    ))
                })
                .chain(std::io::stdout()),
        );

    match dispatch.apply() {
        Ok(()) => {
            let _ = LOGGER_STARTED.set(());
            Ok(())
        }
        Err(_e) => {
            // Someone else already set a global logger. Respect the requested level and continue.
            log::set_max_level(level);
            Ok(())
        }
    }
}
