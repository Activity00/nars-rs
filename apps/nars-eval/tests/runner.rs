use std::time::Duration;

use nars_eval::{ProviderChoice, load_tasks, run_all};
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread")]
async fn scripted_task_runs_end_to_end() {
    let root = TempDir::new().unwrap();
    let fixtures = root.path().join("fixtures");
    let fixture = fixtures.join("rename");
    std::fs::create_dir_all(fixture.join("src")).unwrap();
    std::fs::write(
        fixture.join("Cargo.toml"),
        "[package]\nname = \"eval_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        fixture.join("src/lib.rs"),
        "pub fn alpha() -> i32 {\n    1\n}\n",
    )
    .unwrap();

    let tasks_dir = root.path().join("tasks");
    std::fs::create_dir_all(&tasks_dir).unwrap();
    let task = serde_json::json!({
        "id": "rename",
        "category": "coding_tasks",
        "goal_type": "modify_code",
        "prompt": "Rename alpha to beta.",
        "setup": {"fixture": "rename"},
        "limits": {"max_steps": 8, "max_model_calls": 8},
        "script": [
            {"kind":"task_proposal","goal":{"type":"modify_code"},"candidate_actions":[{"tool":"read_file","arguments":{"path":"src/lib.rs"},"confidence":0.9}]},
            {"kind":"task_proposal","goal":{"type":"modify_code"},"candidate_actions":[{"tool":"apply_patch","arguments":{"path":"src/lib.rs","old_text":"alpha","new_text":"beta"},"confidence":0.95}]},
            {"kind":"task_proposal","goal":{"type":"modify_code"},"candidate_actions":[{"tool":"cargo_check","arguments":{},"confidence":0.9}]},
            {"kind":"final_answer","text":"Renamed alpha to beta.","success":true}
        ],
        "expect": {
            "files_modified": ["src/lib.rs"],
            "tools_used": ["read_file","apply_patch","cargo_check"],
            "approval_required": true,
            "run_status": "completed",
            "replay_hash_equal": true
        }
    });
    std::fs::write(
        tasks_dir.join("rename.json"),
        serde_json::to_string_pretty(&task).unwrap(),
    )
    .unwrap();

    let tasks = load_tasks(&tasks_dir).unwrap();
    assert_eq!(tasks.len(), 1);
    let report = run_all(
        &tasks_dir,
        &fixtures,
        ProviderChoice::Scripted,
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    assert_eq!(report.passed, 1, "{:?}", report.tasks);
    assert_eq!(report.failed, 0);
    assert!(report.tasks[0].metrics.replay_hash_equal);
}
