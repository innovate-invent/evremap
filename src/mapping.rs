use anyhow::Context;
pub use evdev_rs::enums::{EventCode, EventCode as KeyCode, EventType};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use thiserror::Error;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone)]
pub struct MappingConfig {
    pub device_name: String,
    pub phys: Option<String>,
    pub mappings: Vec<Mapping>,
}

impl MappingConfig {
    pub fn from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let toml_data = std::fs::read_to_string(path)
            .context(format!("reading toml from {}", path.display()))?;
        let config_file: ConfigFile =
            toml::from_str(&toml_data).context(format!("parsing toml from {}", path.display()))?;
        let mut mappings = vec![];
        for dual in config_file.dual_role {
            mappings.push(dual.into());
        }
        for remap in config_file.remap {
            mappings.push(remap.into());
        }
        Ok(Self {
            device_name: config_file.device_name,
            phys: config_file.phys,
            mappings,
        })
    }
}

#[derive(Debug, Clone)]
pub enum Mapping {
    DualRole {
        input: KeyCode,
        hold: Vec<KeyCode>,
        tap: Vec<KeyCode>,
    },
    Remap {
        input: HashSet<KeyCodeWrapper>,
        output: HashSet<KeyCodeWrapper>,
    },
}

#[derive(Debug, Deserialize, Clone)]
#[serde(try_from = "String")]
pub struct KeyCodeWrapper {
    pub code: EventCode,
    pub scale: i32,
}

impl PartialEq for KeyCodeWrapper {
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code && self.scale.is_negative() == other.scale.is_negative()
    }
}

impl Eq for KeyCodeWrapper {}

impl PartialEq<EventCode> for KeyCodeWrapper {
    fn eq(&self, other: &EventCode) -> bool {
        self.code == *other
    }
}

impl PartialEq<KeyCodeWrapper> for EventCode {
    fn eq(&self, other: &KeyCodeWrapper) -> bool {
        *self == other.code
    }
}

impl Hash for KeyCodeWrapper {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.code.hash(state);
        // Hash the direction filter only but not the magnitude
        (if self.scale.is_negative() {-1} else {1}).hash(state);
    }
}

impl From<KeyCodeWrapper> for KeyCode {
    fn from(value: KeyCodeWrapper) -> Self {
        value.code
    }
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Invalid key `{0}`.  Use `evremap list-keys` to see possible keys.")]
    InvalidKey(String),
    #[error("Impossible: parsed KEY_XXX but not into an EV_KEY")]
    ImpossibleParseKey,
}

impl std::convert::TryFrom<String> for KeyCodeWrapper {
    type Error = ConfigError;
    fn try_from(s: String) -> Result<KeyCodeWrapper, Self::Error> {
        let mut scale: i32 = 1;
        let name: &str;
        match s.rmatch_indices(&['+', '-']).next() {
            None => {
                name = &s;
                scale = 0;
            },
            Some(m) => {
                let _scale;
                (name, _scale) = s.split_at(m.0);
                if _scale.len() > 1 {
                    scale = _scale.parse::<i32>().unwrap();
                } else if _scale == "-" {
                    scale = -1;
                }
            },
        };
        let mut prefix = name.split_once("_").unwrap().0;
        if prefix == "BTN" {
            prefix = "KEY";
        }
        if prefix == "KEY" && scale == 0 {
            scale = 1;
        }
        match EventType::from_str(&*("EV_".to_string() + prefix)) {
            Some(event_type) => match EventCode::from_str(&event_type, &name) {
                Some(code) => Ok(KeyCodeWrapper { code, scale }),
                None => Err(ConfigError::InvalidKey(name.to_string())),
            },
            None => Err(ConfigError::InvalidKey(name.to_string())),
        }
    }
}

#[derive(Debug, Deserialize)]
struct DualRoleConfig {
    input: KeyCodeWrapper,
    hold: Vec<KeyCodeWrapper>,
    tap: Vec<KeyCodeWrapper>,
}

impl Into<Mapping> for DualRoleConfig {
    fn into(self) -> Mapping {
        Mapping::DualRole {
            input: self.input.into(),
            hold: self.hold.into_iter().map(Into::into).collect(),
            tap: self.tap.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RemapConfig {
    input: Vec<KeyCodeWrapper>,
    output: Vec<KeyCodeWrapper>,
}

impl Into<Mapping> for RemapConfig {
    fn into(self) -> Mapping {
        Mapping::Remap {
            input: self.input.into_iter().map(Into::into).collect(),
            output: self.output.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    device_name: String,
    #[serde(default)]
    phys: Option<String>,

    #[serde(default)]
    dual_role: Vec<DualRoleConfig>,

    #[serde(default)]
    remap: Vec<RemapConfig>,
}
