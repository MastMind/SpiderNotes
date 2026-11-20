use crate::note::NoteSummary;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceMeta {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    pub version: u32,
    pub device: Option<DeviceMeta>,
    /// uuid -> summary
    #[serde(default)]
    pub notes: BTreeMap<Uuid, NoteSummary>,
    /// Last local clock value, so same-millisecond events stay ordered.
    #[serde(default)]
    pub device_clock: Option<crate::hlc::Hlc>,
}

impl Index {
    pub const FILE: &'static str = "index.json";
    pub const DIR: &'static str = ".spidernotes";

    pub fn path(root: &Path) -> std::path::PathBuf {
        root.join(Self::DIR).join(Self::FILE)
    }

    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let p = Self::path(root);
        if !p.exists() {
            return Ok(Self {
                version: 1,
                device: None,
                notes: Default::default(),
                device_clock: None,
            });
        }
        let raw = std::fs::read_to_string(&p)?;
        let idx: Index = serde_json::from_str(&raw)?;
        Ok(idx)
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let p = Self::path(root);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        let tmp = dir_tmp_name(&p);
        std::fs::write(&tmp, raw)?;
        std::fs::rename(&tmp, p)?;
        Ok(())
    }
}

fn dir_tmp_name(p: &Path) -> std::path::PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(".tmp");
    std::path::PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hlc::Hlc;

    struct TestDir(std::path::PathBuf);
    impl TestDir {
        fn new() -> Self {
            let d = std::env::temp_dir().join(format!(
                "spidernotes-test-{}-{}",
                std::process::id(),
                Uuid::new_v4()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let t = TestDir::new();
        let mut idx = Index {
            version: 1,
            device: Some(DeviceMeta {
                id: "dev".into(),
                name: "laptop".into(),
            }),
            notes: Default::default(),
            device_clock: None,
        };
        let u = Uuid::new_v4();
        idx.notes.insert(
            u,
            NoteSummary {
                uuid: u,
                path: "x.md".into(),
                hlc: Hlc::from_parts(5, 1, "dev".into()),
                hash: "abc".into(),
                deleted: false,
            },
        );
        idx.save(&t.0).unwrap();
        let loaded = Index::load(&t.0).unwrap();
        assert_eq!(loaded.notes.len(), 1);
        assert_eq!(loaded.device.as_ref().unwrap().name, "laptop");
    }
}
