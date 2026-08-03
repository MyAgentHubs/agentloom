use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::Result;
use crate::events::EventRecorder;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub artifact_id: String,
    pub kind: String,
    pub path: PathBuf,
    pub title: String,
    pub mime_type: String,
}

#[derive(Debug, Clone)]
pub struct ArtifactStore {
    artifacts_dir: PathBuf,
}

impl ArtifactStore {
    pub fn new(artifacts_dir: impl Into<PathBuf>) -> Self {
        Self {
            artifacts_dir: artifacts_dir.into(),
        }
    }

    pub fn write_text(
        &self,
        recorder: &mut EventRecorder,
        file_name: &str,
        title: &str,
        content: &str,
    ) -> Result<Artifact> {
        std::fs::create_dir_all(&self.artifacts_dir)?;
        let path = self.artifacts_dir.join(file_name);
        std::fs::write(&path, content)?;
        let artifact = Artifact {
            artifact_id: format!("art_{}", recorder.run_id()),
            kind: "markdown".to_string(),
            path,
            title: title.to_string(),
            mime_type: "text/markdown".to_string(),
        };
        recorder.emit(
            "artifact.created",
            json!({
                "artifact_id": artifact.artifact_id,
                "kind": artifact.kind,
                "path": display_path(&artifact.path),
                "title": artifact.title,
                "mime_type": artifact.mime_type,
            }),
        )?;
        Ok(artifact)
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
