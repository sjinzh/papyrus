//! Utils for serialization and deserialization of nested config fields into simple types.
//! These conversions let the command line updater (which supports only numbers strings and
//! booleans) handle these fields.
//!
//! # example
//!
//! ```
//! use std::collections::BTreeMap;
//! use std::time::Duration;
//!
//! use papyrus_config::converters::deserialize_milliseconds_to_duration;
//! use papyrus_config::dumping::{ser_param, SerializeConfig};
//! use papyrus_config::loading::load;
//! use papyrus_config::{ParamPath, SerializedParam};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
//! struct DurationConfig {
//!     #[serde(deserialize_with = "deserialize_milliseconds_to_duration")]
//!     dur: Duration,
//! }
//!
//! impl SerializeConfig for DurationConfig {
//!     fn dump(&self) -> BTreeMap<ParamPath, SerializedParam> {
//!         BTreeMap::from([ser_param("dur", &self.dur.as_millis(), "Dur as milliseconds.")])
//!     }
//! }
//!
//! let dumped_config = DurationConfig { dur: Duration::from_secs(1) }.dump();
//! let loaded_config = load::<DurationConfig>(&dumped_config).unwrap();
//! assert_eq!(loaded_config.dur.as_secs(), 1);
//! ```

use std::collections::HashMap;
use std::time::Duration;

use serde::de::Error;
use serde::{Deserialize, Deserializer};

/// Deserializes milliseconds to duration object.
pub fn deserialize_milliseconds_to_duration<'de, D>(de: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let secs: u64 = Deserialize::deserialize(de)?;
    Ok(Duration::from_millis(secs))
}

/// Serializes a map to "k1:v1 k2:v2" string structure.
pub fn serialize_optional_map(optional_map: &Option<HashMap<String, String>>) -> String {
    match optional_map {
        None => "".to_owned(),
        Some(map) => map.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<String>>().join(" "),
    }
}

/// Deserializes a map from "k1:v1 k2:v2" string structure.
pub fn deserialize_optional_map<'de, D>(de: D) -> Result<Option<HashMap<String, String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw_str: String = Deserialize::deserialize(de)?;
    if raw_str.is_empty() {
        return Ok(None);
    }

    let mut map = HashMap::new();
    for raw_pair in raw_str.split(' ') {
        let split: Vec<&str> = raw_pair.split(':').collect();
        if split.len() != 2 {
            return Err(D::Error::custom(format!(
                "pair \"{}\" is not valid. The Expected format is name:value",
                raw_pair
            )));
        }
        map.insert(split[0].to_string(), split[1].to_string());
    }
    Ok(Some(map))
}
