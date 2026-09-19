//! Execute every `#[rule]` concretely over a seeded stream.
//!
//! This is NOT a substitute for the Prover: it samples schedules rather than quantifying
//! over them. What it does give, today and without Prover access, is (a) proof the rule
//! bodies are executable and non-vacuous, and (b) an immediate counterexample seed when a
//! property is wrong -- which is where most modelling errors are caught.
#![cfg(all(feature = "certora", feature = "rt"))]

use redis_cvlr::driver::concrete::exercise;
use redis_cvlr::specs::*;

const N: u64 = 20_000;

fn run(name: &str, f: fn()) -> bool {
    let (n, rejected, failures) = exercise(name, f, N);
    let executed = n - rejected;
    let status = if !failures.is_empty() {
        "FAIL"
    } else if executed == 0 {
        "VACUOUS"
    } else {
        "ok"
    };
    println!(
        "{:<6} {:<48} executed {:>6}/{:<6} rejected {:>5}  {}",
        status,
        name,
        executed,
        n,
        rejected,
        if failures.is_empty() { String::new() } else { format!("seeds {:?}", failures) }
    );
    failures.is_empty() && executed > 0
}

#[test]
fn all_rules_execute() {
    let mut ok = true;
    println!();

    ok &= run("string_set_integrity", string_rules::string_set_integrity);
    ok &= run("string_set_ttl_argument", string_rules::string_set_ttl_argument);
    ok &= run("string_set_nx_xx_uses_logical_existence", string_rules::string_set_nx_xx_uses_logical_existence);

    ok &= run("expire_visibility_split", expire_rules::expire_visibility_split);
    ok &= run("expire_lazy_active_boundary", expire_rules::expire_lazy_active_boundary);
    ok &= run("expire_condition_semantics", expire_rules::expire_condition_semantics);
    ok &= run("expire_lazy_propagates_bare_del", expire_rules::expire_lazy_propagates_bare_del);
    ok &= run("expire_replica_hides_without_deleting", expire_rules::expire_replica_hides_without_deleting);
    ok &= run("expire_master_link_sees_expired_key_as_valid", expire_rules::expire_master_link_sees_expired_key_as_valid);

    ok &= run("keyspace_expires_subset_of_keys", keyspace_rules::keyspace_expires_subset_of_keys);
    ok &= run("keyspace_active_expiry_only_removes", keyspace_rules::keyspace_active_expiry_only_removes);
    ok &= run("keyspace_dbsize_counts_physical", keyspace_rules::keyspace_dbsize_counts_physical);

    ok &= run("multi_watch_is_value_blind", multi_rules::multi_watch_is_value_blind);

    println!();
    println!("--- expected to fail (targets FINDINGS.md F-01) ---");
    let f01 = run("multi_touch_dirties_every_watcher", multi_rules::multi_touch_dirties_every_watcher);
    println!();

    assert!(ok, "a rule that was expected to hold did not");
    assert!(!f01, "multi_touch_dirties_every_watcher was expected to FAIL (F-01); it passed -- \
                   either the model no longer transcribes the multi.c:415 break, or the \
                   scenario no longer reaches it. Re-read FINDINGS.md before 'fixing' this.");
}
