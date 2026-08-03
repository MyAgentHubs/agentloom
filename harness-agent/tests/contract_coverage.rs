fn contract() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/CONTRACT.md")).unwrap()
}

#[test]
fn documents_every_vocabulary_type() {
    let c = contract();
    for ty in myagent::vocabulary::VOCABULARY {
        assert!(c.contains(ty), "CONTRACT.md missing type `{ty}`");
    }
}

#[test]
fn documents_exit_codes_and_invariants() {
    let c = contract();
    for n in ["`0`", "`1`", "`2`", "`3`", "`4`", "`130`"] {
        assert!(c.contains(n), "missing exit code {n}");
    }
    for s in [
        "schema_version",
        "harness.runtime.v1",
        "seq",
        "Tier 0",
        "Tier 1",
        "null",
        "completed",
        "blocked",
        "needs_decision",
        "interrupted",
    ] {
        assert!(c.contains(s), "CONTRACT.md missing `{s}`");
    }
}

#[test]
fn documents_control_commands_and_criteria_and_status_set() {
    let c = contract();
    for cmd in [
        "stop",
        "approve",
        "reject",
        "pause",
        "resume",
        "revise",
        "inspect_runtime",
    ] {
        assert!(c.contains(cmd), "missing control command {cmd}");
    }
    for syn in ["cmd:", "contains:", "judge:"] {
        assert!(c.contains(syn), "missing criteria syntax {syn}");
    }
    for st in ["pending", "passed", "failed", "waived", "uncertain"] {
        assert!(c.contains(st), "missing status {st}");
    }
    assert!(c.contains(".myagenthubs/runs"), "missing journal layout");
}
