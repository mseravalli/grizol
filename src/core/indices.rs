use crate::{
    device_id::DeviceId,
    syncthing::{FileInfo, Index},
};
use std::collections::{HashMap};

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct FolderDevice {
    pub folder: String,
    pub device_id: DeviceId,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct GrizolIndices {
    indices: HashMap<FolderDevice, GrizolIndex>,
}

#[derive(Clone, Debug, PartialEq, Default)]
struct GrizolIndex {
    folder: String,
    files: HashMap<String, FileInfo>,
}

impl From<Index> for GrizolIndex {
    fn from(value: Index) -> Self {
        let mut grizol_index = GrizolIndex::default();
        grizol_index.folder = value.folder;
        for f in value.files.into_iter() {
            grizol_index.files.insert(f.name.clone(), f);
        }
        grizol_index
    }
}

impl From<GrizolIndex> for Index {
    fn from(value: GrizolIndex) -> Self {
        Index {
            folder: value.folder,
            files: value.files.into_values().collect(),
        }
    }
}

impl From<&GrizolIndex> for Index {
    fn from(value: &GrizolIndex) -> Self {
        Index {
            folder: value.folder.clone(),
            files: value.files.clone().into_values().collect(),
        }
    }
}

impl GrizolIndices {
    pub fn insert(&mut self, device_id: DeviceId, index: Index) -> Option<Index> {
        let key = FolderDevice {
            device_id,
            folder: index.folder.clone(),
        };
        self.indices.insert(key, index.into()).map(|x| x.into())
    }

    // TODO: return a reference to the index, this requires to use GrizolIndex all over the place
    // and have the conversion at the boundaries.
    pub fn index(&self, folder_device: &FolderDevice) -> Option<Index> {
        debug!("getting index for {:?}", &folder_device);
        self.indices.get(folder_device).map(|x| x.into())
    }

    pub fn insert_file_info(
        &mut self,
        folder: &str,
        device_id: &DeviceId,
        file_info: Vec<FileInfo>,
    ) {
        let folder_device = FolderDevice {
            folder: folder.to_string(),
            device_id: *device_id,
        };

        let index_files = &mut self
            .indices
            .entry(folder_device)
            .or_insert(Default::default())
            .files;

        for f in file_info.into_iter() {
            index_files.insert(f.name.clone(), f);
        }
    }

    pub fn remove_file_info(
        &mut self,
        folder: &str,
        device_id: &DeviceId,
        file_name: &str,
    ) -> Option<FileInfo> {
        let folder_device = FolderDevice {
            folder: folder.to_string(),
            device_id: *device_id,
        };
        self.indices
            .entry(folder_device)
            .or_insert(Default::default())
            .files
            .remove(file_name)
    }
}
