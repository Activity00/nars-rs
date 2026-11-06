use std::sync::Arc;
use std::time::Duration;

use nars_agent_ir::{
    RunId, SideEffect, ToolCall, ToolCallId, ToolManifest, ToolResult, ToolStatus,
};
use nars_tools::manifest::{Tool, ToolContext, ToolRegistry, object_schema};
use nars_tools::policy::{ApprovalMode, Policy, PolicyConfig};
use nars_tools::{BrokerConfig, Sandbox, ToolBroker, build_call};
use serde_json::json;
use tempfile::TempDir;

fn call(tool: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        run_id: RunId::from_static("run_test"),
        tool: tool.to_owned(),
        arguments,
        side_effect: SideEffect::ReadOnly,
        requires_approval: false,
        created_at_ms: 0,
    }
}

fn broker(dir: &TempDir) -> ToolBroker {
    ToolBroker::with_builtin_tools(dir.path(), Policy::default()).unwrap()
}

#[tokio::test]
async fn reads_a_file_with_metadata() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "one\ntwo\nthree\n").unwrap();
    let result = broker(&dir)
        .execute(call("read_file", json!({"path": "hello.txt"})))
        .await;
    assert!(result.is_success(), "{result:?}");
    assert_eq!(result.stdout, "one\ntwo\nthree");
    assert_eq!(result.metadata["total_lines"], 3);
}

#[tokio::test]
async fn pages_through_lines() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "one\ntwo\nthree\n").unwrap();
    let result = broker(&dir)
        .execute(call(
            "read_file",
            json!({"path": "hello.txt", "offset": 2, "limit": 1, "numbered": true}),
        ))
        .await;
    assert!(result.is_success(), "{result:?}");
    assert_eq!(result.stdout, "    2 | two");
}

#[tokio::test]
async fn rejects_path_escape_and_binary_files() {
    let dir = TempDir::new().unwrap();
    let broker = broker(&dir);
    let result = broker
        .execute(call("read_file", json!({"path": "../escape.txt"})))
        .await;
    assert_eq!(result.status, ToolStatus::Failed);

    std::fs::write(dir.path().join("bin.dat"), [0_u8, 1, 2]).unwrap();
    let result = broker
        .execute(call("read_file", json!({"path": "bin.dat"})))
        .await;
    assert_eq!(result.status, ToolStatus::Failed);
}

#[tokio::test]
async fn lists_and_searches_files() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src/nested")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "fn alpha() {}\nfn beta() {}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/nested/mod.rs"), "fn gamma() {}\n").unwrap();
    std::fs::write(dir.path().join("README.md"), "alpha docs\n").unwrap();
    let broker = broker(&dir);

    let listing = broker
        .execute(call("list_files", json!({"path": "src", "depth": 3})))
        .await;
    assert!(listing.is_success(), "{listing:?}");
    assert!(listing.stdout.contains("src/lib.rs"));
    assert!(listing.stdout.contains("src/nested/"));

    let search = broker
        .execute(call("search", json!({"query": "alpha", "path": "."})))
        .await;
    assert!(search.is_success(), "{search:?}");
    assert!(search.stdout.contains("src/lib.rs:1:"));
    assert!(search.stdout.contains("README.md:1:"));
    assert!(!search.stdout.contains("gamma"));
}

#[tokio::test]
async fn applies_a_patch_and_returns_a_diff() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn alpha() {}\n").unwrap();
    let broker = broker(&dir);
    let call = call(
        "apply_patch",
        json!({"path": "lib.rs", "old_text": "alpha", "new_text": "beta"}),
    );
    let preview = broker.preview(&call).expect("patch preview");
    assert!(preview.unified_diff.unwrap().contains("-fn alpha()"));
    let result = broker.execute(call).await;
    assert!(result.is_success(), "{result:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "fn beta() {}\n"
    );
    let diff = result.diff.expect("diff");
    assert_eq!(diff.added, 1);
    assert_eq!(diff.removed, 1);
}

#[tokio::test]
async fn creates_new_files_and_rejects_ambiguous_replacement() {
    let dir = TempDir::new().unwrap();
    let broker = broker(&dir);
    let result = broker
        .execute(call(
            "apply_patch",
            json!({"path": "new.txt", "old_text": "", "new_text": "hello\n"}),
        ))
        .await;
    assert!(result.is_success(), "{result:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
        "hello\n"
    );

    std::fs::write(dir.path().join("dup.txt"), "x x x\n").unwrap();
    let result = broker
        .execute(call(
            "apply_patch",
            json!({"path": "dup.txt", "old_text": "x", "new_text": "y"}),
        ))
        .await;
    assert_eq!(result.status, ToolStatus::Failed);
    assert!(result.error.unwrap().contains("occurs 3 times"));
}

#[tokio::test]
async fn broker_denies_policy_denied_calls() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "x\n").unwrap();
    let policy = Policy::new(PolicyConfig {
        mode: ApprovalMode::Deny,
        ..PolicyConfig::default()
    });
    let broker = ToolBroker::with_builtin_tools(dir.path(), policy).unwrap();
    let manifest = broker
        .manifests()
        .into_iter()
        .find(|manifest| manifest.name == "apply_patch")
        .unwrap();
    let result = broker
        .execute(build_call(
            ToolCallId::new(),
            RunId::from_static("run_test"),
            &manifest,
            json!({"path": "lib.rs", "old_text": "x", "new_text": "y"}),
            0,
        ))
        .await;
    assert_eq!(result.status, ToolStatus::Denied);
}

struct SlowTool {
    manifest: ToolManifest,
}

impl Tool for SlowTool {
    fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }

    fn execute(&self, call: &ToolCall, _context: &ToolContext) -> ToolResult {
        std::thread::sleep(Duration::from_millis(300));
        ToolResult::success(call.id.clone(), "late")
    }
}

#[tokio::test]
async fn broker_enforces_manifest_timeout() {
    let dir = TempDir::new().unwrap();
    let sandbox = Sandbox::new(dir.path()).unwrap();
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(SlowTool {
            manifest: ToolManifest::new(
                "slow",
                "slow tool",
                object_schema(json!({}), &[]),
                SideEffect::ReadOnly,
            )
            .with_timeout(50),
        }))
        .unwrap();
    let broker = ToolBroker::new(
        registry,
        Policy::default(),
        sandbox,
        BrokerConfig::default(),
    );
    let result = broker.execute(call("slow", json!({}))).await;
    assert_eq!(result.status, ToolStatus::TimedOut);
}

#[test]
fn build_call_marks_high_risk_tools_for_approval() {
    let manifest = ToolManifest::new(
        "git_commit",
        "commit",
        object_schema(json!({}), &[]),
        SideEffect::GitHistoryChange,
    );
    let call = build_call(
        ToolCallId::new(),
        RunId::from_static("run_test"),
        &manifest,
        json!({}),
        1,
    );
    assert!(call.requires_approval);
}
