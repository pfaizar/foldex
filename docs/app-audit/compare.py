#!/usr/bin/env python3
"""Equal-work baseline/current benchmarks. Source additions restore on exit.

Run with a clean, isolated target worktree; normal Rust tests remain unchanged.
Results are local dev-profile warm-file-cache timings, not Windows release/GPU SLOs.
"""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import statistics
import subprocess

HERE = Path(__file__).resolve().parent
AFTER = r'''
        let mut builder = IndexBuilder::create_with_options(&base.join("continued"), &scope,
            &BuildOptions { directory_only: true, ..Default::default() }, &cancel).unwrap();
        let started = Instant::now();
        while !builder.scan_step(512, &cancel, &|| {}).unwrap().done {}
        let mut index = builder.finish(true).unwrap();
        let spec = QuerySpec { text: needle.clone(), match_mode: foldex_index::MatchMode::Substring, ..Default::default() };
        let page = index.query(&spec, 0, 256, &cancel).unwrap();
        assert_eq!(index.status().files, count as u64); assert_eq!(page.total, Some(1));
        println!("COMPARISON {}", serde_json::json!({"probe":"continued_directory", "created":count, "rows":index.status().files, "filter_matches":page.total, "complete":index.status().complete, "elapsed_ms":started.elapsed().as_secs_f64()*1000.0}));
        for run in 0..5 {
            fs::write(scope.join(&needle), format!("changed{run}")).unwrap();
            let started = Instant::now();
            let result = NativeIndex::reconcile(&index.status().database_path, &[needle.clone().into()], &BuildOptions { directory_only:true, ..Default::default() }, &cancel).unwrap();
            println!("COMPARISON {}", serde_json::json!({"probe":"metadata_refresh", "run":run, "records":count, "elapsed_ms":started.elapsed().as_secs_f64()*1000.0, "enumerated_entries":result.metrics.enumerated_entries, "changed_ancestor_metadata_reads":result.metrics.metadata_reads, "reused_shards":result.metrics.reused_shards, "copied_bytes":result.metrics.copied_bytes, "retired_rows":result.metrics.retired_rows}));
            index = result.index;
            assert_eq!(index.status().files, count as u64);
        }
        drop(index);
'''
BEFORE = r'''
        for run in 0..5 {
            fs::write(scope.join(&needle), format!("changed{run}")).unwrap();
            let mut builder = IndexBuilder::create(&base.join(format!("refresh{run}")), &scope, &cancel).unwrap();
            let started = Instant::now();
            while !builder.scan_step(512, &cancel, &|| {}).unwrap().done {}
            let index = builder.finish(true).unwrap();
            assert_eq!(index.status().files, count as u64);
            println!("COMPARISON {}", serde_json::json!({"probe":"metadata_refresh", "run":run, "records":count, "elapsed_ms":started.elapsed().as_secs_f64()*1000.0, "enumerated_entries":index.status().entries}));
            drop(index);
        }
'''
@contextmanager
def additions(repo, improved):
    originals = []
    try:
        for target, probe in [("crates/foldex/src/search.rs", "comparison_search.rs"), ("crates/foldex/src/pane/listing.rs", "comparison_accessibility.rs")]:
            path = repo / target
            original = path.read_bytes()
            if b"mod app_comparison_probes" in original: raise RuntimeError("Probe already appended")
            addition = (HERE / "probes" / probe).read_text().replace("/* AFTER_DIRECTORY */", AFTER if improved else "").replace("/* BEFORE_REFRESH */", "" if improved else BEFORE).replace("/* POOL_SETUP */", "let pool = paging::Pool::new(hub.shared.clone()).unwrap();" if improved else "").replace("/* POOL_DROP */", "drop(pool);" if improved else "").replace("/* STEP_CALL */", "context.step(&cache_base, false, &cancel, &cancel, Some(&pool)).unwrap()" if improved else "context.step(&cache_base, false, &cancel, &cancel).unwrap()").encode()
            expected = original + addition
            originals.append((path, original, expected))
            path.write_bytes(expected)
        yield
    finally:
        for path, original, expected in reversed(originals):
            if path.read_bytes() != expected: raise RuntimeError(f"Source changed while benchmarking; preserved {path}")
            path.write_bytes(original)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--improved", action="store_true")
    parser.add_argument("--cargo", default="cargo")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--mode", choices=["all", "coordinator"], default="all")
    parser.add_argument("--runs", type=int, default=5)
    args = parser.parse_args()
    if not 1 <= args.runs <= 10: parser.error("--runs must be 1–10")
    env = dict(os.environ, FOLDEX_COMPARISON_RUNS=str(args.runs))
    command = [args.cargo, "test", "--locked", "--offline", "-p", "foldex", "--no-default-features", "--features", "renderer-glow", "app_comparison_query_lane" if args.mode == "coordinator" else "app_comparison_", "--", "--nocapture", "--test-threads=1"]
    with additions(args.repo, args.improved):
        result = subprocess.run(command, cwd=args.repo, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    args.output.with_suffix(".log").write_text(result.stdout)
    if result.returncode: raise RuntimeError(f"Benchmark failed; see {args.output.with_suffix('.log')}")
    records = [json.loads(line.split("COMPARISON ", 1)[1]) for line in result.stdout.splitlines() if "COMPARISON " in line]
    groups = {}
    for row in records:
        if "elapsed_ms" in row: groups.setdefault((row["probe"], row.get("query"), row.get("phase")), []).append(row["elapsed_ms"])
    medians = [{"probe": key[0], "query": key[1], "phase": key[2], "runs":len(times), "median_ms":statistics.median(times)} for key, times in groups.items()]
    args.output.write_text(json.dumps({"environment":("Linux development profile; 100k synthetic names; Context.step query-worker lane; 1ms polling; no Windows/GPU latency claim" if args.mode == "coordinator" else "Linux dev profile; warm file cache; full 768-row windows; synthetic 100k names and 150k real empty files; no Windows/GPU latency claim"), "improved":args.improved, "records":records, "medians":medians}, indent=2)+"\n")
    print(json.dumps({"records":len(records), "output":str(args.output)}))
if __name__ == "__main__": main()
