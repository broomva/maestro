//! Read-only machine-local fleet telemetry. Each input has an independent read state.
use crate::Store;
use maestro_core::{Section, SourceError};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct Census {
    pub host_load: Value,
    pub host_caps: Value,
    pub connections: Value,
    #[serde(default)]
    pub active_signals: Vec<Value>,
}

#[derive(Debug, Deserialize)]
pub struct Capacity {
    pub host: Value,
    pub lanes: Vec<Value>,
    pub routing: Vec<Value>,
}

#[derive(Debug, Deserialize)]
pub struct LoopHealth {
    pub loops: Value,
}

#[derive(Debug, Deserialize)]
pub struct Sample {
    pub at: String,
    pub lane: String,
    pub used: Option<f64>,
    pub readable: bool,
}

impl Store {
    fn health_json<T: for<'de> Deserialize<'de>>(&self, file: &str) -> Section<T> {
        let path = self.paths.fleet_meta.join(file);
        let source = format!("fs:{}", path.display());
        match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        {
            Ok(value) => Section::ok(source, value),
            Err(error) => Section::unavailable(source, error),
        }
    }

    pub fn health_census(&self) -> Section<Census> {
        self.health_json("fleet-snapshot.json")
    }
    pub fn health_capacity(&self) -> Section<Capacity> {
        self.health_json("capacity.json")
    }
    pub fn health_loops(&self) -> Section<LoopHealth> {
        self.health_json("loop-health.json")
    }

    pub fn health_history(&self) -> Section<Vec<Sample>> {
        let path = self.paths.fleet_meta.join("capacity-history.jsonl");
        let source = format!("fs:{}", path.display());
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => return Section::unavailable(source, e.to_string()),
        };
        let mut samples = Vec::new();
        let mut errors = Vec::new();
        for (index, line) in text.lines().enumerate() {
            match serde_json::from_str(line) {
                Ok(sample) => samples.push(sample),
                Err(e) => errors.push(SourceError { at: format!("{}:{}", source, index + 1), reason: e.to_string() }),
            }
        }
        Section::partial(source, samples, errors)
    }

    pub fn health_log(&self) -> Section<String> {
        let path = self.paths.fleet_meta.join("capacity.log");
        let source = format!("fs:{}", path.display());
        match std::fs::read_to_string(path) {
            Ok(text) => Section::ok(source, text),
            Err(e) => Section::unavailable(source, e.to_string()),
        }
    }
}
