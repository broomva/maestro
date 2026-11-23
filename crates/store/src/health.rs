//! Read-only machine-local fleet telemetry. Producer timestamps, never file mtimes, establish age.
use crate::{Store, Tree, TreeSource};
use maestro_core::{Section, SourceError};
use serde::Deserialize;
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Deserialize)]
pub struct Census {
    pub host_load: Value,
    pub host_caps: Value,
    pub connections: Value,
    #[serde(default)]
    pub observed_at: Option<String>,
    #[serde(default)]
    pub host_last_observed: Value,
    #[serde(default)]
    pub reachable_hosts: Vec<String>,
    #[serde(default)]
    pub active_signals: Vec<Value>,
    #[serde(default)]
    pub coverage_gaps: Vec<Value>,
    #[serde(default)]
    pub sensor_errors: Value,
    #[serde(default)]
    pub errors: Value,
}

#[derive(Debug, Deserialize)]
pub struct Capacity {
    pub host: Value,
    pub lanes: Vec<Value>,
    pub routing: Vec<Value>,
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    #[serde(default)]
    pub host_holds: Value,
    #[serde(default)]
    pub errors: Value,
}

#[derive(Debug, Deserialize)]
pub struct LoopHealth {
    pub loops: Value,
    #[serde(default)]
    pub evaluated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Sample {
    pub at: String,
    pub lane: String,
    pub used: Option<f64>,
    pub readable: bool,
}

#[derive(Debug, Deserialize)]
pub struct Signal {
    pub at: String,
    pub host: String,
    pub instance: String,
    pub to: String,
}

#[derive(Debug, Deserialize)]
pub struct CensusSample {
    pub at: String,
    #[serde(default)]
    pub observed_at: Option<String>,
    #[serde(default)]
    pub read_at: Value,
    pub complete: bool,
    pub host_load: Value,
    #[serde(default)]
    pub errors: Value,
}

/// No telemetry input can grow the request's allocation without bound.
const TAIL_BYTES: u64 = 1024 * 1024;

impl Store {
    fn health_json<T: for<'de> Deserialize<'de>>(&self, file: &str) -> Section<T> {
        let path = self.paths.fleet_meta.join(file);
        let source = format!("fs:{}", path.display());
        let tree = Tree::new(self.paths.fleet_meta.clone(), TreeSource::WorkingTree);
        match tree
            .read(file)
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

    fn health_tail(&self, file: &str) -> Section<String> {
        let path = self.paths.fleet_meta.join(file);
        let mut source = format!("fs:{}", path.display());
        let read = || -> Result<(String, bool), String> {
            let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let size = f.metadata().map_err(|e| e.to_string())?.len();
            let start = size.saturating_sub(TAIL_BYTES);
            f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            f.take(TAIL_BYTES).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            if start > 0 {
                let after = bytes.iter().position(|b| *b == b'\n').map_or(bytes.len(), |i| i + 1);
                bytes.drain(..after);
            }
            Ok((String::from_utf8(bytes).map_err(|e| e.to_string())?, start > 0))
        };
        match read() {
            Ok((text, truncated)) => {
                if truncated {
                    source.push_str(" (last 1 MiB; earlier records not read)");
                }
                Section::ok(source, text)
            }
            Err(error) => Section::unavailable(source, error),
        }
    }

    fn health_rows<T: for<'de> Deserialize<'de>>(&self, file: &str) -> Section<Vec<T>> {
        let text = self.health_tail(file);
        let Some(data) = text.data else {
            return Section { source: text.source, data: None, errors: text.errors };
        };
        let mut rows = Vec::new();
        let mut errors = Vec::new();
        for (index, line) in data.lines().enumerate() {
            match serde_json::from_str::<Value>(line).map_err(|e| e.to_string()).and_then(|value| {
                let at = value.get("at").and_then(Value::as_str).ok_or("missing sample timestamp")?;
                at.parse::<jiff::Timestamp>().map_err(|_| "invalid sample timestamp")?;
                serde_json::from_value(value).map_err(|e| e.to_string())
            }) {
                Ok(sample) => rows.push(sample),
                Err(reason) => {
                    errors.push(SourceError { at: format!("{}:record {} in tail", text.source, index + 1), reason })
                }
            }
        }
        Section::partial(text.source, rows, errors)
    }

    pub fn health_history(&self) -> Section<Vec<Sample>> {
        self.health_rows("capacity-history.jsonl")
    }
    pub fn health_signals(&self) -> Section<Vec<Signal>> {
        self.health_rows("signals.jsonl")
    }
    pub fn health_samples(&self) -> Section<Vec<CensusSample>> {
        self.health_rows("usage-watch.jsonl")
    }
    pub fn health_log(&self) -> Section<String> {
        self.health_tail("capacity.log")
    }
}
