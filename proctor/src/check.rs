//! Run checks and compare `expect` to the application's JSON output and
//! `observe` to the fixture journal / ledgers (docs/harness.md: a check passes
//! only when both agree; the application is never its own oracle).

use std::path::Path;

use serde_json::Value;

use crate::contract::{Check, CheckResult};
use crate::exec;

const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Run every check in a task once. Returns (results, overall pass).
pub fn run_checks(checks: &[Check], cwd: &Path) -> (Vec<CheckResult>, bool) {
    let mut results = Vec::new();
    let mut all_ok = true;
    for check in checks {
        let out = exec::run_shell(&check.run, CHECK_TIMEOUT, cwd);
        // The check's JSON output is the application's transcript: find the
        // first JSON document in stdout (a `--json` run prints exactly one).
        let doc = first_json(&out.stdout);
        let expect_failures = match &doc {
            Some(doc) => compare_expect(&check.expect, doc),
            None if check.expect.is_empty() => Vec::new(),
            None => vec!["check produced no JSON output to assert on".to_string()],
        };
        let observe_failures = compare_observe(&check.observe, cwd);
        let result = CheckResult {
            name: check.name.clone(),
            expect_ok: expect_failures.is_empty(),
            expect_failures,
            observe_ok: observe_failures.is_empty(),
            observe_failures,
            output_excerpt: excerpt(&out.stdout, &out.stderr, out.exit_ok, out.timed_out),
        };
        if !result.ok() {
            all_ok = false;
        }
        results.push(result);
    }
    (results, all_ok)
}

/// Compare dotted JSON paths to expected values: `totals.settled` -> `"0.0000"`.
fn compare_expect(expect: &std::collections::BTreeMap<String, Value>, doc: &Value) -> Vec<String> {
    let mut failures = Vec::new();
    for (path, expected) in expect {
        let actual = lookup(doc, path);
        match actual {
            Some(a) if json_equal(a, expected) => {}
            Some(a) => failures.push(format!("expect {path}: {} != {}", a, expected)),
            None => failures.push(format!("expect {path}: path missing from output")),
        }
    }
    failures
}

/// `observe` rows are measured from the fixture journal and ledgers under the
/// current directory (sellers/data/results for fixture results). Each key is
/// either `files = n` (count of result files whose content matches a
/// `where`-style object) or `served = n`. For the scaffold the supported
/// keys are resolved against the sellers' result store.
fn compare_observe(observe: &std::collections::BTreeMap<String, Value>, cwd: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    for (key, expected) in observe {
        match key.as_str() {
            "fixture_results" => {
                let dir = cwd.join("sellers/data/results");
                let count = std::fs::read_dir(&dir)
                    .map(|rd| rd.filter_map(|e| e.ok()).filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false)).count())
                    .unwrap_or(0);
                if count != expected.as_u64().unwrap_or(0) as usize {
                    failures.push(format!("observe fixture_results: found {count}, expected {expected}"));
                }
            }
            _ => failures.push(format!("observe {key}: unknown measurement (scaffold)")),
        }
    }
    failures
}

fn lookup<'a>(doc: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = doc;
    for part in path.split('.') {
        // Support `name[i]` array indexing on a segment.
        if let Some(open) = part.find('[') {
            let name = &part[..open];
            let close = part.find(']')?;
            let index: usize = part[open + 1..close].parse().ok()?;
            cur = cur.get(name)?.get(index)?;
        } else {
            cur = cur.get(part)?;
        }
    }
    Some(cur)
}

fn json_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(s), Value::String(t)) => s == t,
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Number(x), Value::String(y)) => y.parse::<f64>().map(|n| n == x.as_f64().unwrap_or(f64::NAN)).unwrap_or(false),
        (Value::String(x), Value::Number(y)) => json_equal(&Value::Number(y.clone()), &Value::String(x.clone())),
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Array(xs), Value::Array(ys)) => xs.len() == ys.len() && xs.iter().zip(ys).all(|(a, b)| json_equal(a, b)),
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

fn first_json(stdout: &str) -> Option<Value> {
    // The transcript `--json` output is a single JSON document; it may be
    // preceded by log lines, so scan for the first `{` that parses. Trailing
    // newlines after the document are trimmed.
    for (i, b) in stdout.bytes().enumerate() {
        if b == b'{' {
            let rest = stdout[i..].trim_end();
            if let Ok(v) = serde_json::from_str(rest) {
                return Some(v);
            }
        }
    }
    None
}

fn excerpt(stdout: &str, stderr: &str, exit_ok: bool, timed_out: bool) -> String {
    let mut out = String::new();
    if !exit_ok {
        out.push_str(if timed_out { "timed out; " } else { "exit non-zero; " });
    }
    for line in stdout.lines().chain(stderr.lines()) {
        if out.len() > 400 {
            out.push_str("\n... (truncated)");
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
