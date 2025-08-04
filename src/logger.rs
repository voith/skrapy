// src/logger.rs
use chrono::Local;
use fern::colors::{Color, ColoredLevelConfig};
use log::LevelFilter;

pub fn init_logger(level: LevelFilter) -> Result<(), fern::InitError> {
    let colors = ColoredLevelConfig::new()
        .info(Color::Green)
        .debug(Color::Blue)
        .warn(Color::Yellow)
        .error(Color::Red);

    fern::Dispatch::new()
        // use the passed‐in filter here
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
        )
        .apply()?;

    Ok(())
}
