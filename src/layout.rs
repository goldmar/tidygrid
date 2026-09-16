//! Deterministic, machine-readable layout inspection and validation.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct Location {
    pub container: String,
    pub home_page: usize,
    pub home_slot: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_page: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_slot: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Placement {
    /// A stable icon-instance identifier. iOS can place the same app more than
    /// once; in that case displayIdentifier distinguishes the copies.
    pub key: String,
    pub bundle_identifier: String,
    pub display_name: String,
    pub location: Location,
    #[serde(skip_serializing)]
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
    pub severity: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ValidationReport {
    pub ok: bool,
    pub app_count: usize,
    pub missing: Vec<String>,
    pub unknown: Vec<String>,
    pub duplicated: Vec<String>,
    pub changed: Vec<String>,
    pub issues: Vec<ValidationIssue>,
}

fn is_folder(item: &Value) -> bool {
    item.get("listType").and_then(Value::as_str) == Some("folder")
}

fn key_of(item: &Value) -> Option<&str> {
    item.get("displayIdentifier")
        .or_else(|| item.get("bundleIdentifier"))
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
}

fn bundle_of(item: &Value, key: &str) -> String {
    item.get("bundleIdentifier")
        .and_then(Value::as_str)
        .filter(|bundle| !bundle.is_empty())
        .unwrap_or(key)
        .to_owned()
}

fn name_of(item: &Value, key: &str) -> String {
    item.get("displayName")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or(key)
        .to_owned()
}

pub fn placements(state: &Value) -> Result<Vec<Placement>, String> {
    let pages = state
        .as_array()
        .ok_or_else(|| "icon state must be an array of dock plus home-screen pages".to_string())?;
    let mut out = Vec::new();

    for (page_index, page) in pages.iter().enumerate() {
        let items = page
            .as_array()
            .ok_or_else(|| format!("layout page {page_index} is not an array"))?;
        for (slot_index, item) in items.iter().enumerate() {
            if is_folder(item) {
                let folder_name = item
                    .get("displayName")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| {
                        format!("folder at page {page_index}, slot {slot_index} has no name")
                    })?;
                let folder_pages = item
                    .get("iconLists")
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("folder '{folder_name}' has no iconLists array"))?;
                for (folder_page_index, folder_page) in folder_pages.iter().enumerate() {
                    let children = folder_page.as_array().ok_or_else(|| {
                        format!("folder '{folder_name}' page {folder_page_index} is not an array")
                    })?;
                    for (folder_slot_index, child) in children.iter().enumerate() {
                        if is_folder(child) {
                            return Err(format!(
                                "folder '{folder_name}' contains a nested folder on page {}, slot {}",
                                folder_page_index + 1,
                                folder_slot_index + 1
                            ));
                        }
                        let key = key_of(child).ok_or_else(|| {
                            format!(
                                "folder '{folder_name}' page {folder_page_index}, slot {folder_slot_index} has no identifier"
                            )
                        })?;
                        out.push(Placement {
                            key: key.to_owned(),
                            bundle_identifier: bundle_of(child, key),
                            display_name: name_of(child, key),
                            payload: child.clone(),
                            location: Location {
                                container: folder_name.to_owned(),
                                home_page: page_index,
                                home_slot: slot_index,
                                folder_page: Some(folder_page_index),
                                folder_slot: Some(folder_slot_index),
                            },
                        });
                    }
                }
            } else {
                let key = key_of(item).ok_or_else(|| {
                    format!("item at page {page_index}, slot {slot_index} has no identifier")
                })?;
                out.push(Placement {
                    key: key.to_owned(),
                    bundle_identifier: bundle_of(item, key),
                    display_name: name_of(item, key),
                    payload: item.clone(),
                    location: Location {
                        container: if page_index == 0 {
                            "Dock".to_string()
                        } else {
                            format!("Page {page_index}")
                        },
                        home_page: page_index,
                        home_slot: slot_index,
                        folder_page: None,
                        folder_slot: None,
                    },
                });
            }
        }
    }
    Ok(out)
}

fn metric(metrics: &Value, name: &str, fallback: usize) -> usize {
    metrics
        .get(name)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(fallback)
}

fn issue(code: &str, message: String, severity: &str) -> ValidationIssue {
    ValidationIssue {
        code: code.to_owned(),
        message,
        severity: severity.to_owned(),
    }
}

pub fn validate(state: &Value, metrics: &Value, inventory: Option<&Value>) -> ValidationReport {
    let mut issues = Vec::new();
    let found = match placements(state) {
        Ok(found) => found,
        Err(error) => {
            issues.push(issue("malformed-layout", error, "error"));
            return ValidationReport {
                ok: false,
                app_count: 0,
                missing: Vec::new(),
                unknown: Vec::new(),
                duplicated: Vec::new(),
                changed: Vec::new(),
                issues,
            };
        }
    };

    let mut counts = BTreeMap::<String, usize>::new();
    for app in &found {
        *counts.entry(app.key.clone()).or_default() += 1;
    }
    let duplicated: Vec<String> = counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(key, _)| key.clone())
        .collect();
    for key in &duplicated {
        issues.push(issue(
            "duplicate-app",
            format!("{key} is placed {} times", counts[key]),
            "error",
        ));
    }

    for app in &found {
        if app
            .payload
            .get("iconType")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != "app")
        {
            issues.push(issue(
                "unsupported-icon-type",
                format!("{} has an unsupported iconType", app.key),
                "error",
            ));
        }
    }

    let mut missing = Vec::new();
    let mut unknown = Vec::new();
    let mut changed = Vec::new();
    if let Some(inventory) = inventory {
        match placements(inventory) {
            Ok(available) => {
                let available: BTreeMap<_, _> = available
                    .into_iter()
                    .map(|app| (app.key, app.payload))
                    .collect();
                let placed: BTreeMap<_, _> = found
                    .iter()
                    .map(|app| (app.key.clone(), app.payload.clone()))
                    .collect();
                let available_keys: BTreeSet<_> = available.keys().cloned().collect();
                let placed_keys: BTreeSet<_> = placed.keys().cloned().collect();
                missing = available_keys.difference(&placed_keys).cloned().collect();
                unknown = placed_keys.difference(&available_keys).cloned().collect();
                changed = available_keys
                    .intersection(&placed_keys)
                    .filter(|key| available.get(*key) != placed.get(*key))
                    .cloned()
                    .collect();
                for key in &missing {
                    issues.push(issue(
                        "missing-app",
                        format!("{key} is visible on the phone but absent from the plan"),
                        "error",
                    ));
                }
                for key in &unknown {
                    issues.push(issue(
                        "unknown-app",
                        format!("{key} is in the plan but absent from the current layout"),
                        "error",
                    ));
                }
                for key in &changed {
                    issues.push(issue(
                        "changed-icon",
                        format!("{key} has different icon data than the current phone"),
                        "error",
                    ));
                }
            }
            Err(error) => issues.push(issue("malformed-inventory", error, "error")),
        }
    }

    let dock_limit = metric(metrics, "homeScreenIconDockMaxCount", 4);
    let columns = metric(metrics, "homeScreenIconColumns", 4);
    let rows = metric(metrics, "homeScreenIconRows", 6);
    let page_limit = columns.saturating_mul(rows);
    let folder_columns = metric(metrics, "homeScreenIconFolderColumns", 3);
    let folder_rows = metric(metrics, "homeScreenIconFolderRows", 3);
    let folder_limit = folder_columns.saturating_mul(folder_rows);
    let folder_page_count_limit = metric(metrics, "homeScreenIconFolderMaxPages", 15);
    let page_count_limit = metric(metrics, "homeScreenIconMaxPages", 15);

    if let Some(pages) = state.as_array() {
        if let Some(dock) = pages.first().and_then(Value::as_array) {
            if dock.len() > dock_limit {
                issues.push(issue(
                    "dock-overflow",
                    format!(
                        "dock contains {} items; maximum is {dock_limit}",
                        dock.len()
                    ),
                    "error",
                ));
            }
        }
        if pages.len().saturating_sub(1) > page_count_limit {
            issues.push(issue(
                "too-many-pages",
                format!(
                    "layout contains {} pages; maximum is {page_count_limit}",
                    pages.len().saturating_sub(1)
                ),
                "error",
            ));
        }
        for (page_index, page) in pages.iter().enumerate() {
            let Some(items) = page.as_array() else {
                continue;
            };
            if page_index > 0 && items.len() > page_limit {
                issues.push(issue(
                    "page-overflow",
                    format!(
                        "page {page_index} contains {} items; maximum is {page_limit}",
                        items.len()
                    ),
                    "error",
                ));
            }
            for item in items {
                if !is_folder(item) {
                    continue;
                }
                let folder_name = item
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or("<unnamed folder>");
                if page_index == 0 {
                    issues.push(issue(
                        "dock-folder",
                        format!("folder '{folder_name}' is in the dock, which is unreliable"),
                        "warning",
                    ));
                }
                let Some(folder_pages) = item.get("iconLists").and_then(Value::as_array) else {
                    continue;
                };
                if folder_pages.len() > folder_page_count_limit {
                    issues.push(issue(
                        "too-many-folder-pages",
                        format!(
                            "folder '{folder_name}' contains {} pages; maximum is {folder_page_count_limit}",
                            folder_pages.len()
                        ),
                        "error",
                    ));
                }
                let total = folder_pages
                    .iter()
                    .filter_map(Value::as_array)
                    .map(Vec::len)
                    .sum::<usize>();
                if total == 0 {
                    issues.push(issue(
                        "empty-folder",
                        format!("folder '{folder_name}' is empty"),
                        "error",
                    ));
                }
                for (folder_page_index, folder_page) in folder_pages.iter().enumerate() {
                    if let Some(children) = folder_page.as_array() {
                        if children.len() > folder_limit {
                            issues.push(issue(
                                "folder-page-overflow",
                                format!(
                                    "folder '{folder_name}' page {} contains {} items; maximum is {folder_limit}",
                                    folder_page_index + 1,
                                    children.len()
                                ),
                                "error",
                            ));
                        }
                    }
                }
            }
        }
    }

    ValidationReport {
        ok: !issues.iter().any(|item| item.severity == "error"),
        app_count: found.len(),
        missing,
        unknown,
        duplicated,
        changed,
        issues,
    }
}

pub fn validate_live_metrics(metrics: &Value) -> Result<(), String> {
    let required = [
        "homeScreenIconDockMaxCount",
        "homeScreenIconColumns",
        "homeScreenIconRows",
        "homeScreenIconFolderColumns",
        "homeScreenIconFolderRows",
        "homeScreenIconMaxPages",
        "homeScreenIconFolderMaxPages",
    ];
    let mut values = BTreeMap::new();
    for name in required {
        let value = metrics
            .get(name)
            .and_then(Value::as_u64)
            .filter(|value| (1..=1000).contains(value))
            .ok_or_else(|| format!("the iPhone returned an invalid or missing {name} metric"))?;
        values.insert(name, value as usize);
    }
    values["homeScreenIconColumns"]
        .checked_mul(values["homeScreenIconRows"])
        .ok_or_else(|| "the iPhone returned overflowing Home Screen dimensions".to_string())?;
    values["homeScreenIconFolderColumns"]
        .checked_mul(values["homeScreenIconFolderRows"])
        .ok_or_else(|| "the iPhone returned overflowing folder dimensions".to_string())?;
    Ok(())
}

fn identity_map(state: &Value) -> Result<BTreeMap<String, (String, Option<String>)>, String> {
    let icons = placements(state)?;
    let mut identities = BTreeMap::new();
    for icon in icons {
        if identities
            .insert(
                icon.key.clone(),
                (
                    icon.bundle_identifier,
                    icon.payload
                        .get("iconType")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                ),
            )
            .is_some()
        {
            return Err(format!("{} appears more than once", icon.key));
        }
    }
    Ok(identities)
}

pub fn verify_settled(expected: &Value, observed: &Value, metrics: &Value) -> Result<(), String> {
    let report = validate(observed, metrics, None);
    if !report.ok {
        return Err(format!(
            "the read-back layout is invalid: {}",
            serde_json::to_string(&report).unwrap_or_default()
        ));
    }
    if identity_map(expected)? != identity_map(observed)? {
        return Err("the read-back contains different icon identities".to_string());
    }
    if !equivalent_placement(expected, observed)? {
        return Err("the read-back does not match the requested placements".to_string());
    }
    Ok(())
}

pub fn refresh_icon_payloads(saved: &Value, current: &Value) -> Result<Value, String> {
    if identity_map(saved)? != identity_map(current)? {
        return Err("the saved layout does not match the phone's current icon identities".into());
    }
    let payloads: BTreeMap<_, _> = placements(current)?
        .into_iter()
        .map(|icon| (icon.key, icon.payload))
        .collect();
    let mut refreshed = saved.clone();
    let pages = refreshed
        .as_array_mut()
        .ok_or_else(|| "icon state must be an array of dock plus home-screen pages".to_string())?;
    for page in pages {
        let items = page
            .as_array_mut()
            .ok_or_else(|| "layout contains a page that is not an array".to_string())?;
        for item in items {
            if is_folder(item) {
                let folder_pages = item
                    .get_mut("iconLists")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| "saved folder has no iconLists array".to_string())?;
                for folder_page in folder_pages {
                    let children = folder_page
                        .as_array_mut()
                        .ok_or_else(|| "saved folder page is not an array".to_string())?;
                    for child in children {
                        let key = key_of(child)
                            .ok_or_else(|| "saved folder icon has no identifier".to_string())?;
                        *child = payloads
                            .get(key)
                            .cloned()
                            .ok_or_else(|| format!("{key} is not on the current phone"))?;
                    }
                }
            } else {
                let key = key_of(item)
                    .ok_or_else(|| "saved Home Screen icon has no identifier".to_string())?;
                *item = payloads
                    .get(key)
                    .cloned()
                    .ok_or_else(|| format!("{key} is not on the current phone"))?;
            }
        }
    }
    if !equivalent_placement(saved, &refreshed)? {
        return Err("refreshing icon metadata changed the saved placements".to_string());
    }
    Ok(refreshed)
}

pub fn diff(before: &Value, after: &Value) -> Result<Value, String> {
    let before_placements = placements(before)?;
    let after_placements = placements(after)?;
    let before_map: BTreeMap<_, _> = before_placements
        .iter()
        .map(|app| (app.key.clone(), app))
        .collect();
    let after_map: BTreeMap<_, _> = after_placements
        .iter()
        .map(|app| (app.key.clone(), app))
        .collect();

    let before_keys: BTreeSet<_> = before_map.keys().cloned().collect();
    let after_keys: BTreeSet<_> = after_map.keys().cloned().collect();
    let added: Vec<_> = after_keys.difference(&before_keys).cloned().collect();
    let removed: Vec<_> = before_keys.difference(&after_keys).cloned().collect();
    let mut moved = Vec::new();
    for key in before_keys.intersection(&after_keys) {
        let old = before_map[key];
        let new = after_map[key];
        if old.location != new.location {
            moved.push(json!({
                "key": key,
                "display_name": new.display_name,
                "from": old.location,
                "to": new.location,
            }));
        }
    }

    let folders = |state: &Value| -> BTreeSet<String> {
        state
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_array)
            .flatten()
            .filter(|item| is_folder(item))
            .filter_map(|item| item.get("displayName").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
    };
    let old_folders = folders(before);
    let new_folders = folders(after);
    let added_folders: Vec<_> = new_folders.difference(&old_folders).cloned().collect();
    let removed_folders: Vec<_> = old_folders.difference(&new_folders).cloned().collect();

    Ok(json!({
        "empty": added.is_empty() && removed.is_empty() && moved.is_empty()
            && added_folders.is_empty() && removed_folders.is_empty(),
        "app_count_before": before_placements.len(),
        "app_count_after": after_placements.len(),
        "added": added,
        "removed": removed,
        "moved": moved,
        "folders_added": added_folders,
        "folders_removed": removed_folders,
    }))
}

pub fn equivalent_placement(left: &Value, right: &Value) -> Result<bool, String> {
    let left: BTreeMap<_, _> = placements(left)?
        .into_iter()
        .map(|app| (app.key, app.location))
        .collect();
    let right: BTreeMap<_, _> = placements(right)?
        .into_iter()
        .map(|app| (app.key, app.location))
        .collect();
    Ok(left == right)
}

#[cfg(test)]
mod tests {
    use super::{
        diff, equivalent_placement, refresh_icon_payloads, validate, validate_live_metrics,
        verify_settled,
    };
    use serde_json::json;

    fn metrics() -> serde_json::Value {
        json!({
            "homeScreenIconDockMaxCount": 4,
            "homeScreenIconColumns": 4,
            "homeScreenIconRows": 6,
            "homeScreenIconFolderColumns": 3,
            "homeScreenIconFolderRows": 3,
            "homeScreenIconMaxPages": 15,
            "homeScreenIconFolderMaxPages": 15
        })
    }

    #[test]
    fn system_app_alias_and_regular_icon_are_distinct_instances() {
        let state = json!([[], [
            {
                "bundleIdentifier": "com.apple.DocumentsApp",
                "displayIdentifier": "system-icon-uuid",
                "iconLists": [],
                "iconType": "app"
            },
            {
                "bundleIdentifier": "com.apple.DocumentsApp",
                "displayIdentifier": "com.apple.DocumentsApp",
                "displayName": "Files"
            }
        ]]);
        let report = validate(&state, &metrics(), Some(&state));
        assert!(report.ok, "{:?}", report.issues);
        assert_eq!(report.app_count, 2);
        assert!(report.duplicated.is_empty());
    }

    #[test]
    fn validation_rejects_loss_duplication_and_overflow() {
        let inventory = json!([[], [
            {"bundleIdentifier": "a", "displayIdentifier": "a", "displayName": "A"},
            {"bundleIdentifier": "b", "displayIdentifier": "b", "displayName": "B"}
        ]]);
        let plan = json!([[], [
            {"bundleIdentifier": "a", "displayIdentifier": "a", "displayName": "A"},
            {"bundleIdentifier": "a", "displayIdentifier": "a", "displayName": "A"},
            {"bundleIdentifier": "c", "displayIdentifier": "c", "displayName": "C"}
        ]]);
        let report = validate(&plan, &metrics(), Some(&inventory));
        assert!(!report.ok);
        assert_eq!(report.missing, vec!["b"]);
        assert_eq!(report.unknown, vec!["c"]);
        assert_eq!(report.duplicated, vec!["a"]);
    }

    #[test]
    fn diff_and_equivalence_include_exact_slots() {
        let before = json!([[], [
            {"bundleIdentifier": "a", "displayIdentifier": "a", "displayName": "A"},
            {"bundleIdentifier": "b", "displayIdentifier": "b", "displayName": "B"}
        ]]);
        let after = json!([[], [
            {"bundleIdentifier": "b", "displayIdentifier": "b", "displayName": "B"},
            {"bundleIdentifier": "a", "displayIdentifier": "a", "displayName": "A"}
        ]]);
        assert!(!equivalent_placement(&before, &after).unwrap());
        assert_eq!(
            diff(&before, &after).unwrap()["moved"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn validation_rejects_changed_identity_and_unsupported_structures() {
        let inventory = json!([[], [
            {"bundleIdentifier": "a", "displayIdentifier": "a", "displayName": "A"}
        ]]);
        let changed = json!([[], [
            {"bundleIdentifier": "different", "displayIdentifier": "a", "displayName": "A"}
        ]]);
        let report = validate(&changed, &metrics(), Some(&inventory));
        assert!(!report.ok);
        assert_eq!(report.changed, vec!["a"]);

        let widget = json!([[], [
            {"bundleIdentifier": "a", "displayIdentifier": "a", "iconType": "widget"}
        ]]);
        assert!(!validate(&widget, &metrics(), None).ok);

        let nested = json!([[], [{
            "displayName": "Outer",
            "listType": "folder",
            "iconLists": [[{
                "displayName": "Inner",
                "listType": "folder",
                "iconLists": [[{"displayIdentifier": "a"}]]
            }]]
        }]]);
        assert!(!validate(&nested, &metrics(), None).ok);
    }

    #[test]
    fn settled_verification_allows_metadata_refresh_but_not_identity_changes() {
        let expected = json!([[], [
            {
                "bundleIdentifier": "a",
                "displayIdentifier": "a",
                "displayName": "A",
                "bundleVersion": "1"
            }
        ]]);
        let refreshed = json!([[], [
            {
                "bundleIdentifier": "a",
                "displayIdentifier": "a",
                "displayName": "A refreshed",
                "bundleVersion": "2"
            }
        ]]);
        verify_settled(&expected, &refreshed, &metrics()).unwrap();

        let changed = json!([[], [
            {"bundleIdentifier": "different", "displayIdentifier": "a"}
        ]]);
        assert!(verify_settled(&expected, &changed, &metrics()).is_err());

        let duplicated = json!([[], [
            {"bundleIdentifier": "a", "displayIdentifier": "a"},
            {"bundleIdentifier": "a", "displayIdentifier": "a"}
        ]]);
        assert!(verify_settled(&expected, &duplicated, &metrics()).is_err());
    }

    #[test]
    fn live_metrics_must_be_complete_and_sane() {
        assert!(validate_live_metrics(&metrics()).is_ok());
        let mut missing = metrics();
        missing
            .as_object_mut()
            .unwrap()
            .remove("homeScreenIconRows");
        assert!(validate_live_metrics(&missing).is_err());
        let mut zero = metrics();
        zero["homeScreenIconRows"] = json!(0);
        assert!(validate_live_metrics(&zero).is_err());
    }

    #[test]
    fn folder_page_count_is_enforced() {
        let state = json!([[], [{
            "displayName": "Full",
            "listType": "folder",
            "iconLists": [
                [{"displayIdentifier": "a"}],
                [{"displayIdentifier": "b"}]
            ]
        }]]);
        let mut one_page = metrics();
        one_page["homeScreenIconFolderMaxPages"] = json!(1);
        let report = validate(&state, &one_page, None);
        assert!(!report.ok);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "too-many-folder-pages"));
    }

    #[test]
    fn restore_refreshes_metadata_without_changing_saved_positions() {
        let saved = json!([[], [
            {
                "bundleIdentifier": "a",
                "displayIdentifier": "a",
                "displayName": "Old A",
                "bundleVersion": "1"
            },
            {
                "bundleIdentifier": "b",
                "displayIdentifier": "b",
                "displayName": "Old B"
            }
        ]]);
        let current = json!([[], [
            {
                "bundleIdentifier": "b",
                "displayIdentifier": "b",
                "displayName": "New B"
            },
            {
                "bundleIdentifier": "a",
                "displayIdentifier": "a",
                "displayName": "New A",
                "bundleVersion": "2"
            }
        ]]);
        let refreshed = refresh_icon_payloads(&saved, &current).unwrap();
        assert_eq!(refreshed[1][0]["displayName"], "New A");
        assert_eq!(refreshed[1][0]["bundleVersion"], "2");
        assert!(equivalent_placement(&saved, &refreshed).unwrap());
        assert!(validate(&refreshed, &metrics(), Some(&current)).ok);
    }
}
