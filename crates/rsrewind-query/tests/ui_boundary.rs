//! CLAUDE.md: `rsrewind-ui` depends only on `rsrewind-core` and `rsrewind-query` and must never
//! reach capture, storage, OCR or the daemon, *even transitively*. That is what makes "the UI
//! crashing cannot stop recording" true by construction. This test walks the real resolved
//! dependency graph (`cargo metadata`, every target platform, normal and build edges; dev-only
//! edges do not ship) and fails if any forbidden crate is reachable from the UI.
//!
//! It lives here rather than in the UI crate so it also runs on hosts that cannot build the UI.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Command;

const FORBIDDEN: &[&str] = &[
    "rsrewind-storage",
    "rsrewind-capture",
    "rsrewind-ocr",
    "rsrewind-daemon",
    // Not named in CLAUDE.md, but a viewer has no business with the replication format either:
    // the history facade hides that sources arrive as segments (docs/design/distributed.md §4).
    "rsrewind-segment",
    "rsrewind-cli",
];

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn metadata() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    // No --filter-platform: a Windows-only edge must be caught on a Linux host too.
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(&manifest)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Package names reachable from `root` through non-dev edges, and the root's direct deps.
fn reachable(
    meta: &serde_json::Value,
    root: &str,
) -> Result<(BTreeSet<String>, BTreeSet<String>), Box<dyn std::error::Error>> {
    let packages = meta["packages"].as_array().ok_or("no packages")?;
    let names: HashMap<&str, &str> = packages
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p["name"].as_str()?)))
        .collect();
    let root_id = packages
        .iter()
        .find(|p| p["name"] == root)
        .and_then(|p| p["id"].as_str())
        .ok_or("root package not in workspace")?;
    let nodes: HashMap<&str, &serde_json::Value> = meta["resolve"]["nodes"]
        .as_array()
        .ok_or("no resolve graph")?
        .iter()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();

    let shipped_deps = |id: &str| -> Vec<String> {
        nodes
            .get(id)
            .and_then(|n| n["deps"].as_array())
            .map(|deps| {
                deps.iter()
                    .filter(|d| {
                        d["dep_kinds"].as_array().is_some_and(|kinds| {
                            kinds.iter().any(|k| k["kind"].as_str() != Some("dev"))
                        })
                    })
                    .filter_map(|d| d["pkg"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };

    let direct: BTreeSet<String> = shipped_deps(root_id)
        .iter()
        .filter_map(|id| names.get(id.as_str()).map(|n| (*n).to_owned()))
        .collect();
    let mut seen = BTreeSet::new();
    let mut found = BTreeSet::new();
    let mut queue = VecDeque::from([root_id.to_owned()]);
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        for dep in shipped_deps(&id) {
            if let Some(name) = names.get(dep.as_str()) {
                found.insert((*name).to_owned());
            }
            queue.push_back(dep);
        }
    }
    Ok((found, direct))
}

#[test]
fn the_ui_cannot_reach_capture_storage_ocr_or_the_daemon() -> TestResult {
    let meta = metadata()?;
    let (all, direct) = reachable(&meta, "rsrewind-ui")?;

    // Not vacuous: the UI really reads history through the query layer.
    assert!(all.contains("rsrewind-query"), "{all:?}");
    assert!(all.contains("rsrewind-core"), "{all:?}");

    let leaked: Vec<&str> = FORBIDDEN
        .iter()
        .copied()
        .filter(|f| all.contains(*f))
        .collect();
    assert!(
        leaked.is_empty(),
        "rsrewind-ui reaches {leaked:?}; only rsrewind-core and rsrewind-query are allowed \
         (CLAUDE.md, \"UI never owns capture, storage, or OCR\")"
    );

    // No SQL in the UI: SQLite may only arrive through rsrewind-query.
    assert!(!direct.contains("rusqlite"), "{direct:?}");
    let first_party: Vec<&String> = direct
        .iter()
        .filter(|d| d.starts_with("rsrewind-"))
        .collect();
    assert_eq!(
        first_party,
        ["rsrewind-core", "rsrewind-query"],
        "rsrewind-ui may depend directly on rsrewind-core and rsrewind-query only"
    );
    Ok(())
}

/// The tray acts only through the `rsrewind` command line (it runs the executable); like the UI it
/// must not be able to reach capture, storage, OCR, the daemon, segments, or even the query layer.
#[test]
fn the_tray_reaches_nothing_but_core() -> TestResult {
    let meta = metadata()?;
    let (all, direct) = reachable(&meta, "rsrewind-tray")?;
    assert!(all.contains("rsrewind-core"), "{all:?}");
    let leaked: Vec<&str> = FORBIDDEN
        .iter()
        .chain(&["rsrewind-query", "rsrewind-ui"])
        .copied()
        .filter(|f| all.contains(*f))
        .collect();
    assert!(leaked.is_empty(), "rsrewind-tray reaches {leaked:?}");
    assert!(!all.contains("rusqlite"), "{all:?}");
    let first_party: Vec<&String> = direct
        .iter()
        .filter(|d| d.starts_with("rsrewind-"))
        .collect();
    assert_eq!(first_party, ["rsrewind-core"]);
    Ok(())
}

#[test]
fn the_query_layer_itself_does_not_depend_on_storage() -> TestResult {
    let meta = metadata()?;
    let (all, _) = reachable(&meta, "rsrewind-query")?;
    for forbidden in FORBIDDEN {
        assert!(
            !all.contains(*forbidden),
            "rsrewind-query reaches {forbidden}"
        );
    }
    Ok(())
}
