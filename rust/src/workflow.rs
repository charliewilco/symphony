use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use serde_yaml::{Mapping, Value};

#[derive(Clone, Debug)]
pub struct LoadedWorkflow {
    pub prompt_template: String,
    pub prompt: String,
}

pub fn workflow_file_path(explicit: Option<&Path>) -> Result<PathBuf> {
    match explicit {
        Some(path) => Ok(path.canonicalize().unwrap_or_else(|_| path.to_path_buf())),
        None => Ok(std::env::current_dir()?.join("WORKFLOW.md")),
    }
}

pub fn load(path: &Path) -> Result<LoadedWorkflow> {
    let content = fs::read_to_string(path)
        .map_err(|error| anyhow!("missing_workflow_file: {}: {error}", path.display()))?;
    parse(&content)
}

pub fn parse(content: &str) -> Result<LoadedWorkflow> {
    let (_, prompt_lines) = split_front_matter(content);
    let prompt = prompt_lines.join("\n").trim().to_string();
    Ok(LoadedWorkflow {
        prompt_template: prompt.clone(),
        prompt,
    })
}

pub fn load_legacy_front_matter(path: &Path) -> Result<Value> {
    let content = fs::read_to_string(path)
        .map_err(|error| anyhow!("missing_workflow_file: {}: {error}", path.display()))?;
    parse_legacy_front_matter(&content)
}

pub fn parse_legacy_front_matter(content: &str) -> Result<Value> {
    let (front_matter_lines, _) = split_front_matter(content);
    front_matter_to_value(&front_matter_lines)
}

pub fn has_front_matter(path: &Path) -> Result<bool> {
    let content = fs::read_to_string(path)
        .map_err(|error| anyhow!("missing_workflow_file: {}: {error}", path.display()))?;
    Ok(matches!(content.lines().next(), Some("---")))
}

fn split_front_matter(content: &str) -> (Vec<String>, Vec<String>) {
    let lines = content
        .split('\n')
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();

    if !matches!(lines.first(), Some(first) if first == "---") {
        return (Vec::new(), lines);
    }

    let mut front = Vec::new();
    let mut rest_start = None;
    for (index, line) in lines.iter().enumerate().skip(1) {
        if line == "---" {
            rest_start = Some(index + 1);
            break;
        }
        front.push(line.clone());
    }

    let prompt_lines = rest_start
        .map(|index| lines[index..].to_vec())
        .unwrap_or_default();
    (front, prompt_lines)
}

fn front_matter_to_value(lines: &[String]) -> Result<Value> {
    let yaml = lines.join("\n");
    if yaml.trim().is_empty() {
        return Ok(Value::Mapping(Mapping::new()));
    }

    let value: Value =
        serde_yaml::from_str(&yaml).map_err(|error| anyhow!("workflow_parse_error: {error}"))?;
    match value {
        Value::Mapping(_) => Ok(value),
        _ => Err(anyhow!("workflow_front_matter_not_a_map")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prompt_only_workflow() {
        let workflow = parse("Prompt only\n").unwrap();
        assert_eq!(workflow.prompt, "Prompt only");
    }

    #[test]
    fn strips_front_matter_without_parsing_it() {
        let workflow = parse("---\nnot: [valid\n---\nPrompt body\n").unwrap();
        assert_eq!(workflow.prompt, "Prompt body");
    }

    #[test]
    fn parses_legacy_front_matter_when_requested() {
        let value =
            parse_legacy_front_matter("---\ntracker:\n  kind: linear\n---\nPrompt\n").unwrap();
        assert_eq!(value["tracker"]["kind"], "linear");
    }

    #[test]
    fn detects_front_matter_from_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("WORKFLOW.md");
        std::fs::write(&path, "---\ntracker:\n  kind: memory\n---\nPrompt\n").unwrap();
        assert!(has_front_matter(&path).unwrap());
    }
}
