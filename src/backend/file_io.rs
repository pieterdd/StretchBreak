use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
};

use chrono::{DateTime, Duration, Utc};
use dirs::config_dir;
use serde::{Deserialize, Serialize};

use crate::backend::idle_monitoring::PresenceMode;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistableState {
    pub progress_towards_break: Duration,
    pub progress_towards_reset: Duration,
    pub last_checked: DateTime<Utc>,
    pub presence_mode: PresenceMode,
    pub reading_mode: bool,
}

impl PersistableState {
    fn get_state_filename() -> Result<PathBuf, ()> {
        let parent_folder = config_dir()
            .expect("Could not construct config dir path")
            .join("stretch-break");
        std::fs::create_dir_all(&parent_folder).map_err(|_| ())?;
        Ok(parent_folder.join("state.json"))
    }

    pub fn load_from_disk() -> Result<Self, ()> {
        let file_from_disk = fs::read_to_string(Self::get_state_filename()?).map_err(|_| ())?;
        let persistable_state: PersistableState =
            serde_json::from_str(&file_from_disk).map_err(|_| ())?;
        Ok(persistable_state)
    }

    pub fn save_to_disk(&self) -> Result<(), ()> {
        let raw_contents = serde_json::to_string(self).map_err(|_| ())?;
        let mut file = File::create(Self::get_state_filename()?).map_err(|_| ())?;
        file.write_all(&raw_contents.as_bytes()).map_err(|_| ())
    }
}
