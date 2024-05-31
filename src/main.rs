use crate::mapping::*;
use crate::remapper::*;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Duration;
use clap::Parser;

mod deviceinfo;
mod mapping;
mod remapper;

/// Remap libinput evdev keyboard inputs
#[derive(Debug, Parser)]
#[command(
    name = "evremap",
    about,
    author = "Wez Furlong"
)]
enum Opt {
    /// Rather than running the remapper, list currently available devices.
    /// This is helpful to check their names when setting up the initial
    /// configuration
    ListDevices,

    /// Show a list of possible KEY_XXX values
    ListKeys,

    /// Load a remapper config and run the remapper.
    /// This usually requires running as root to obtain exclusive access
    /// to the input devices.
    Remap {
        /// Specify the configuration file to be loaded
        #[arg(name = "CONFIG-FILE")]
        config_file: PathBuf,

        /// Number of seconds for user to release keys on startup
        #[arg(short, long, default_value = "2")]
        delay: f64,

        /// Wait forever for the device to be found
        #[arg(short, long, action)]
        wait: bool,
    },
}

pub fn list_keys() -> Result<()> {
    let mut keys: Vec<String> = EventCode::EV_KEY(KeyCode::KEY_RESERVED)
        .iter()
        .filter_map(|code| match code {
            EventCode::EV_KEY(_) => Some(format!("{}", code)),
            _ => None,
        })
        .collect();
    keys.sort();
    for key in keys {
        println!("{}", key);
    }
    Ok(())
}

fn setup_logger() {
    let mut builder = pretty_env_logger::formatted_timed_builder();
    if let Ok(s) = std::env::var("EVREMAP_LOG") {
        builder.parse_filters(&s);
    } else {
        builder.filter(None, log::LevelFilter::Info);
    }
    builder.init();
}

fn main() -> Result<()> {
    setup_logger();
    let opt = Opt::parse();

    match opt {
        Opt::ListDevices => deviceinfo::list_devices(),
        Opt::ListKeys => list_keys(),
        Opt::Remap { config_file, delay, wait } => {
            let mapping_config = MappingConfig::from_file(&config_file).context(format!(
                "loading MappingConfig from {}",
                config_file.display()
            ))?;

            log::warn!("Short delay: release any keys now!");
            std::thread::sleep(Duration::from_secs_f64(delay));

            let mut device_info;
            let mut i = 0f64;
            loop {
                device_info = deviceinfo::DeviceInfo::with_name(
                    &mapping_config.device_name,
                    mapping_config.phys.as_deref(),
                );
                if !wait || device_info.is_ok() {
                    break;
                }
                let msg = device_info.err();
                if msg.is_some() {
                    log::info!("{}", msg.unwrap());
                }
                std::thread::sleep(Duration::from_secs_f64(delay * i));
                log::info!("Retrying..");
                if i < 30f64 {i += 1f64;}
            }

            let mut mapper = InputMapper::create_mapper(device_info.unwrap().path, mapping_config.mappings)?;
            mapper.run_mapper()
        }
    }
}
