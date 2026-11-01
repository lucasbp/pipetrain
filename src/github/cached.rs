use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value, json};

use crate::detect::keeps_patch;
use crate::model::ChangedFile;

pub(super) fn storable(files: &[ChangedFile]) -> bool {
    !files.iter().any(|f| keeps_patch(&f.path))
}

pub(super) fn files_value(files: &[ChangedFile], complete: bool) -> Value {
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    json!({ "paths": paths, "complete": complete })
}

pub(super) fn files_from(value: &Value) -> Option<(Vec<ChangedFile>, bool)> {
    let paths = strings(value.get("paths")?)?;
    let complete = value.get("complete")?.as_bool()?;
    let files: Vec<ChangedFile> = paths
        .into_iter()
        .map(|path| ChangedFile { path, patch: None })
        .collect();
    storable(&files).then_some((files, complete))
}

pub(super) fn tree_value(paths: &[String], truncated: bool) -> Value {
    json!({ "paths": paths, "truncated": truncated })
}

pub(super) fn tree_from(value: &Value) -> Option<(Vec<String>, bool)> {
    let paths = strings(value.get("paths")?)?;
    let truncated = value.get("truncated")?.as_bool()?;
    Some((paths, truncated))
}

pub(super) fn blob_map(entry: Option<Value>) -> Map<String, Value> {
    entry
        .as_ref()
        .and_then(|value| value.get("files"))
        .and_then(|files| files.as_object())
        .cloned()
        .unwrap_or_default()
}

pub(super) fn missing(known: &Map<String, Value>, paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| !known.contains_key(path.as_str()))
        .cloned()
        .collect()
}

pub(super) fn merge(
    known: &mut Map<String, Value>,
    asked: &[String],
    found: Vec<(String, String)>,
) {
    let texts: HashMap<String, String> = found.into_iter().collect();
    for path in asked {
        let value = texts.get(path).map_or(Value::Null, |t| json!(t));
        known.insert(path.clone(), value);
    }
}

pub(super) fn answer(known: &Map<String, Value>, paths: &[String]) -> Vec<(String, String)> {
    paths
        .iter()
        .filter_map(|path| {
            let text = known.get(path.as_str())?.as_str()?;
            Some((path.clone(), text.to_string()))
        })
        .collect()
}

pub(super) fn blobs_value(known: &Map<String, Value>) -> Value {
    json!({ "files": known })
}

pub(super) fn oid_map(entry: Option<Value>) -> BTreeMap<String, Option<String>> {
    entry
        .as_ref()
        .and_then(|value| value.get("oids"))
        .and_then(Value::as_object)
        .and_then(|map| {
            map.iter()
                .map(|(path, oid)| match oid {
                    Value::Null => Some((path.clone(), None)),
                    Value::String(text) => Some((path.clone(), Some(text.clone()))),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn oids_value(oids: &BTreeMap<String, Option<String>>) -> Value {
    json!({ "oids": oids })
}

fn strings(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect()
}
