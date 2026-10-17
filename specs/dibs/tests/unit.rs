use dibs::core::{self, mine, short_cmd, split_label, stale_at, who};
use dibs::hook::override_of;
use dibs::sys::Procs;
use serde_json::json;

fn hold(agent: &str) -> serde_json::Value {
    json!({"session": "S", "hold": true, "agent": agent})
}

fn covered(own: &serde_json::Value, sid: &str, agent: &str) -> bool {
    mine(own, Some(sid), Some(&Procs::new()), Some(agent))
}

// Traces: DIBS-AGENT-HOLD-SCOPE
#[test]
fn hold_covers_only_its_own_agent_unless_main() {
    assert!(covered(&hold("a1"), "S", "a1"));
    assert!(!covered(&hold("a1"), "S", "a2"));
    assert!(!covered(&hold("a1"), "S", ""));
    assert!(!covered(&hold("a1"), "T", "a1"));
    for who in ["", "a1", "a2"] {
        assert!(covered(&hold(""), "S", who));
        assert!(covered(&json!({"session": "S", "hold": true}), "S", who));
    }
}

// Traces: DIBS-AGENT-IDENTITY
#[test]
fn who_reads_the_agent_from_the_environment() {
    std::env::remove_var("CLAUDE_CODE_SESSION_ID");
    std::env::set_var("DIBS_AGENTS_LOOKUP", "0");
    std::env::set_var("DIBS_AGENT", "a1");
    assert_eq!(who(None, None).agent, "a1");
    std::env::remove_var("DIBS_AGENT");
    assert_eq!(who(None, None).agent, "");
}

// Traces: DIBS-STALE
#[test]
fn stale_at_grace_is_ten_minutes_or_a_quarter_of_the_span() {
    assert_eq!(stale_at(&json!({"t0": 0, "eta": 600})), Some(600.0 + 600.0));
    assert_eq!(stale_at(&json!({"t0": 0, "eta": 8000})), Some(8000.0 + 2000.0));
}

// Traces: DIBS-IDENTITY
#[test]
fn label_in_who_what_form_splits_into_tag_and_purpose() {
    assert_eq!(split_label("alpha-1f/d2w: vax probe build", "alpha-1f"), (Some("d2w".into()), "vax probe build".into()));
    assert_eq!(split_label("alpha: tick timing A/B", "alpha-1f"), (None, "tick timing A/B".into()));
    assert_eq!(split_label("A/B series", "alpha-1f"), (None, "A/B series".into()));
}

// Traces: DIBS-IDENTITY
#[test]
fn command_paths_shrink_to_file_names() {
    assert_eq!(short_cmd("C:/Users/x/AppData/Temp/scratch/gen.py --shards 1-4"), "gen.py --shards 1-4");
    assert_eq!(short_cmd("\"/usr/bin/python3\" /tmp/a/b/run.py"), "python3 run.py");
}

// Traces: DIBS-ANY-NAME
#[test]
fn resource_names_follow_the_pattern() {
    assert!(core::valid_name("port-3000") && core::valid_name("a.b_c"));
    assert!(!core::valid_name("GPU!") && !core::valid_name("-x") && !core::valid_name(&"a".repeat(65)));
}

// Traces: DIBS-HOOK-OVERRIDE
#[test]
fn override_prefix_gives_the_reason_and_the_rest_of_the_command() {
    let cases = [
        ("DIBS_OVERRIDE='pod idles' ssh h true", Some("pod idles"), "ssh h true"),
        ("  DIBS_OVERRIDE=\"pod idles\" ssh h", Some("pod idles"), "ssh h"),
        ("DIBS_OVERRIDE=billed ssh h", Some("billed"), "ssh h"),
        ("$env:DIBS_OVERRIDE = 'pod idles'; ssh h", Some("pod idles"), "ssh h"),
        ("$Env:DIBS_OVERRIDE='x';ssh h", Some("x"), "ssh h"),
        ("DIBS_OVERRIDE='' ssh h", None, "ssh h"),
        ("DIBS_OVERRIDE='  ' ssh h", None, "ssh h"),
        ("ssh h DIBS_OVERRIDE='late'", None, "ssh h DIBS_OVERRIDE='late'"),
        ("echo DIBS_OVERRIDE=x", None, "echo DIBS_OVERRIDE=x"),
    ];
    for (cmd, why, rest) in cases {
        let (w, r) = override_of(cmd);
        assert_eq!((w.as_deref(), r.trim()), (why, rest), "{cmd}");
    }
}
