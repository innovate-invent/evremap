use crate::mapping::*;
use crate::remapper::*;
use anyhow::{bail, Context, Result};
use evdev_rs::{Device, ReadFlag};
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

    /// Listen to a device for events
    Listen {
        /// Name of device to listen to. Same as device_name in config.
        device_name: String,

        /// Optional PCI device path. Same as phys in config.
        phys: Option<String>,
    },

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

pub fn listen(name: String, phys: Option<String>) -> Result<()> {
    let device_info = deviceinfo::DeviceInfo::with_name(
        &name,
        phys.as_deref(),
    )?;
    let path = device_info.path.as_path();
    let f = std::fs::File::open(path).context(format!("opening {}", path.display()))?;
    let input_device = Device::new_from_file(f)
        .with_context(|| format!("failed to create new Device from file {}", path.display()))?;
    log::info!("Going into read loop");
    loop {
        let (status, event) = input_device.next_event(ReadFlag::NORMAL | ReadFlag::BLOCKING)?;
        match status {
            evdev_rs::ReadStatus::Success => {
                match event.event_code {
                    EventCode::EV_KEY(_) |
                    EventCode::EV_REL(_) |
                    EventCode::EV_ABS(_) |
                    EventCode::EV_SW(_)  |
                    EventCode::EV_LED(_) |
                    EventCode::EV_SND(_) |
                    EventCode::EV_REP(_) |
                    EventCode::EV_FF(_)  |
                    EventCode::EV_PWR    |
                    EventCode::EV_FF_STATUS(_) |
                    EventCode::EV_UNK { .. } |
                    EventCode::EV_MAX =>
                        log::info!("IN code: {:?} value: {:?}", event.event_code, event.value),
                    _ => {}
                }
            },
            evdev_rs::ReadStatus::Sync => bail!("ReadStatus::Sync!"),
        }
    }
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
        Opt::Listen { device_name, phys} => listen(device_name, phys),
        Opt::Remap { config_file, delay } => {
            let mapping_config = MappingConfig::from_file(&config_file).context(format!(
                "loading MappingConfig from {}",
                config_file.display()
            ))?;

            log::warn!("Short delay: release any keys now!");
            std::thread::sleep(Duration::from_secs_f64(delay));

            let device_info = deviceinfo::DeviceInfo::with_name(
                &mapping_config.device_name,
                mapping_config.phys.as_deref(),
            )?;

            let mut mapper = InputMapper::create_mapper(device_info.path, mapping_config.mappings)?;
            mapper.run_mapper()
        }
    }
}
