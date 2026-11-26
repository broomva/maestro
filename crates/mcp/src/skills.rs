use rmcp::ErrorData as McpError;
use rmcp::model::{CustomRequest, CustomResult, ErrorCode};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

pub(crate) const EXTENSION: &str = "io.modelcontextprotocol/skills";
pub(crate) const CACHE_MS: u64 = 300_000;
const MAX_FILE_BYTES: u64 = 100_000;
const MAX_SKILL_BYTES: usize = 400_000;

#[derive(Debug)]
pub struct Catalog {
    current: Bundle,
    previous: Option<Bundle>,
}

#[derive(Debug)]
struct Bundle {
    entries: BTreeMap<String, Value>,
    files: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ListParams {
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct GetParams {
    uri: String,
}

impl Catalog {
    /// Read and freeze the release directories before advertising them to clients.
    pub fn load(current: &Path, previous: Option<&Path>) -> io::Result<Self> {
        Ok(Self { current: Bundle::load(current)?, previous: previous.map(Bundle::load).transpose()? })
    }

    pub(crate) fn read(&self, uri: &str) -> Option<&str> {
        self.current.files.get(uri).or_else(|| self.previous.as_ref()?.files.get(uri)).map(String::as_str)
    }

    pub(crate) fn handle(&self, request: CustomRequest) -> Result<CustomResult, McpError> {
        let invalid = |error: serde_json::Error| McpError::invalid_params(error.to_string(), None);
        let mut result = match request.method.as_str() {
            "skills/list" => {
                if request.params_as::<ListParams>().map_err(invalid)?.and_then(|p| p.cursor).is_some() {
                    return Err(McpError::invalid_params("unknown skills cursor", None));
                }
                let entries = self.current.entries.values().collect::<Vec<_>>();
                json!({"skills": entries})
            }
            "skills/get" => {
                let GetParams { uri } = request
                    .params_as()
                    .map_err(invalid)?
                    .ok_or_else(|| McpError::invalid_params("missing skill URI", None))?;
                let entry = self
                    .current
                    .entries
                    .get(&uri)
                    .or_else(|| self.previous.as_ref()?.entries.get(&uri))
                    .ok_or_else(|| McpError::invalid_params("unknown skill URI", None))?;
                json!({ "skill": entry })
            }
            _ => return Err(unsupported(request.method)),
        };
        result["resultType"] = json!("complete");
        result["cacheScope"] = json!("public");
        result["ttlMs"] = json!(CACHE_MS);
        Ok(CustomResult(result))
    }
}

pub(crate) fn unsupported(method: String) -> McpError {
    McpError::new(ErrorCode::METHOD_NOT_FOUND, method, None)
}

impl Bundle {
    fn load(root: &Path) -> io::Result<Self> {
        if !root.symlink_metadata()?.is_dir() {
            return Err(invalid("skill bundle root must be a real directory"));
        }
        let mut skills = BTreeMap::new();
        let mut digest = Sha256::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| invalid("non-UTF-8 skill name"))?;
            if !entry.file_type()?.is_dir() || !skill_name(&name) || skills.len() >= 100 {
                return Err(invalid("skill bundle must contain 1 to 100 valid skill directories"));
            }
            let mut files = BTreeMap::new();
            let mut bytes = 0;
            collect_files(&entry.path(), "", &mut files, &mut bytes)?;
            let instructions = files.get("SKILL.md").ok_or_else(|| invalid("missing SKILL.md"))?;
            let frontmatter = read_frontmatter(instructions, &name)?;
            skills.insert(name, (frontmatter, files));
        }
        if skills.is_empty() {
            return Err(invalid("empty skill bundle"));
        }
        for (name, (_, files)) in &skills {
            for (file, text) in files {
                digest.update(format!("{name}/{file}\n{}\n", text.len()));
                digest.update(text.as_bytes());
            }
        }
        let prefix = format!("skill://maestro/{:x}", digest.finalize());
        let mut bundle = Self { entries: BTreeMap::new(), files: BTreeMap::new() };
        for (name, (frontmatter, files)) in skills {
            let uri = format!("{prefix}/{name}/SKILL.md");
            let resources = files
                .into_iter()
                .map(|(file, text)| {
                    let file_uri = format!("{prefix}/{name}/{file}");
                    let metadata = json!({
                        "uri": file_uri,
                        "digest": format!("sha256:{:x}", Sha256::digest(text.as_bytes())),
                        "size": text.len(),
                    });
                    bundle.files.insert(file_uri, text);
                    metadata
                })
                .collect::<Vec<_>>();
            let entry = json!({ "uri": uri, "frontmatter": frontmatter, "resources": resources });
            bundle.entries.insert(uri, entry);
        }
        Ok(bundle)
    }
}

fn read_frontmatter(text: &str, name: &str) -> io::Result<Value> {
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return Err(invalid("SKILL.md must start with YAML frontmatter"));
    }
    let remaining = lines.by_ref();
    let fields = remaining.by_ref().take_while(|line| *line != "---").collect::<Vec<_>>();
    if remaining.next().is_none() {
        return Err(invalid("SKILL.md is missing instructions after its frontmatter"));
    }
    let value: Value = serde_yaml::from_str(&fields.join("\n")).map_err(invalid)?;
    let description = value.get("description").and_then(Value::as_str).unwrap_or_default();
    if value.get("name").and_then(Value::as_str) != Some(name)
        || description.trim().is_empty()
        || description.chars().count() > 1024
    {
        return Err(invalid("skill name must match its directory and description must be 1 to 1024 characters"));
    }
    Ok(value)
}

fn skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name.bytes().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
}

fn collect_files(root: &Path, prefix: &str, files: &mut BTreeMap<String, String>, bytes: &mut usize) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| invalid("non-UTF-8 skill path"))?;
        if name.starts_with('.') || !name.bytes().all(|ch| ch.is_ascii_alphanumeric() || b"._-".contains(&ch)) {
            return Err(invalid("skill path must contain only letters, digits, dot, underscore and hyphen"));
        }
        let path = format!("{prefix}{name}");
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if prefix.bytes().filter(|ch| *ch == b'/').count() >= 8 {
                return Err(invalid("skill directory exceeds depth limit"));
            }
            collect_files(&entry.path(), &format!("{path}/"), files, bytes)?;
        } else if kind.is_file() {
            if entry.metadata()?.len() > MAX_FILE_BYTES || files.len() >= 512 {
                return Err(invalid("skill file or manifest exceeds size limit"));
            }
            let text = fs::read_to_string(entry.path())?;
            *bytes += text.len();
            if text.len() as u64 > MAX_FILE_BYTES || *bytes > MAX_SKILL_BYTES {
                return Err(invalid("skill exceeds size limit"));
            }
            files.insert(path, text);
        } else {
            return Err(invalid("skill bundles accept only regular files and directories"));
        }
    }
    Ok(())
}

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}
