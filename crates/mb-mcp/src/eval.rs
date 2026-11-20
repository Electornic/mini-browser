// `mb-mcp eval SCENARIO LOG_OR_DIR...` (docs/concept.md "평가 (G5c)"):
// judge session logs against a scenario and aggregate — success rate,
// steps, errors, where failed runs stopped, safety violations — grouped
// by the prompts fingerprint so prompt versions compare side by side.
//
// It never runs an agent: whoever ran the task (a terminal script, a
// Claude Code session) only has to leave `--log` files behind.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::cli::EvalOptions;
use crate::replay::step_label;

/// One condition on a step result.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// The URL contains this.
    Url(String),
    /// The title contains this.
    Title(String),
    /// Some visible text block contains this.
    Text(String),
    /// An element line equals this exactly, e.g. `checkbox "buy milk" [x]`.
    Element(String),
    /// The step failed with this error code.
    Error(String),
}

impl Check {
    fn from_json(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .filter(|o| o.len() == 1)
            .ok_or_else(|| format!("a check is one {{\"kind\": \"value\"}} pair, got {value}"))?;
        let (kind, wanted) = object.iter().next().unwrap();
        let wanted = wanted
            .as_str()
            .ok_or_else(|| format!("check {kind}: expected a string"))?
            .to_string();
        Ok(match kind.as_str() {
            "url" => Check::Url(wanted),
            "title" => Check::Title(wanted),
            "text" => Check::Text(wanted),
            "element" => Check::Element(wanted),
            "error" => Check::Error(wanted),
            other => {
                return Err(format!(
                    "unknown check {other:?} (url/title/text/element/error)"
                ));
            }
        })
    }

    fn matches(&self, result: &Value) -> bool {
        let contains = |field: &str, wanted: &str| {
            result[field]
                .as_str()
                .is_some_and(|value| value.contains(wanted))
        };
        let any_line = |field: &str, test: &dyn Fn(&str) -> bool| {
            result[field]
                .as_array()
                .is_some_and(|lines| lines.iter().filter_map(Value::as_str).any(test))
        };
        match self {
            Check::Url(wanted) => contains("url", wanted),
            Check::Title(wanted) => contains("title", wanted),
            Check::Text(wanted) => any_line("text", &|line| line.contains(wanted.as_str())),
            Check::Element(wanted) => any_line("elements", &|line| line == wanted),
            Check::Error(wanted) => result["error"].as_str() == Some(wanted),
        }
    }

    fn describe(&self) -> String {
        match self {
            Check::Url(v) => format!("url ~ {v:?}"),
            Check::Title(v) => format!("title ~ {v:?}"),
            Check::Text(v) => format!("text ~ {v:?}"),
            Check::Element(v) => format!("element {v}"),
            Check::Error(v) => format!("error {v}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String,
    pub task: String,
    pub success: Vec<Check>,
    pub forbid: Vec<Check>,
}

impl Scenario {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(raw).map_err(|e| format!("scenario: {e}"))?;
        let checks = |key: &str| -> Result<Vec<Check>, String> {
            value[key]
                .as_array()
                .map(|list| list.iter().map(Check::from_json).collect())
                .unwrap_or(Ok(Vec::new()))
        };
        let scenario = Scenario {
            name: value["name"].as_str().unwrap_or("scenario").to_string(),
            task: value["task"].as_str().unwrap_or_default().to_string(),
            success: checks("success")?,
            forbid: checks("forbid")?,
        };
        if scenario.success.is_empty() {
            return Err("scenario: `success` needs at least one check".into());
        }
        Ok(scenario)
    }
}

/// One agent run, judged.
#[derive(Debug, Clone)]
pub struct Run {
    pub file: String,
    pub prompts: String,
    pub steps: usize,
    /// Error codes of failed steps, in order.
    pub errors: Vec<String>,
    pub success: bool,
    /// Success checks the last result didn't meet.
    pub missing: Vec<String>,
    /// Forbidden things that happened: "step 4: url ~ \"example.com\"".
    pub violations: Vec<String>,
    /// Label of the last step (where a failed run stopped).
    pub last_step: Option<String>,
}

pub fn judge(scenario: &Scenario, log: &str, file: &str) -> Result<Run, String> {
    let mut lines = log.lines().filter(|l| !l.trim().is_empty());
    let header: Value = serde_json::from_str(lines.next().ok_or("empty log")?)
        .map_err(|e| format!("header: {e}"))?;
    if header["type"] != "session" {
        return Err("not a mb-mcp session log".into());
    }
    let steps = lines
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("step: {e}"))?;
    let errors = steps
        .iter()
        .filter_map(|s| s["result"]["error"].as_str().map(str::to_string))
        .collect();
    let violations = steps
        .iter()
        .flat_map(|step| {
            scenario
                .forbid
                .iter()
                .filter(|check| check.matches(&step["result"]))
                .map(move |check| format!("step {}: {}", step["n"], check.describe()))
        })
        .collect();
    let last = steps.last();
    let missing: Vec<String> = match last {
        Some(step) => scenario
            .success
            .iter()
            .filter(|check| !check.matches(&step["result"]))
            .map(Check::describe)
            .collect(),
        None => vec!["no steps".into()],
    };
    Ok(Run {
        file: file.to_string(),
        prompts: header["prompts"].as_str().unwrap_or("?").to_string(),
        steps: steps.len(),
        errors,
        success: missing.is_empty(),
        missing,
        violations,
        last_step: last.map(step_label),
    })
}

/// Logs named on the command line; directories expand to their
/// `*.jsonl` files, sorted.
fn log_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut found: Vec<PathBuf> = std::fs::read_dir(path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect();
            found.sort();
            files.extend(found);
        } else {
            files.push(path.clone());
        }
    }
    Ok(files)
}

/// Per prompts version: the numbers `mb-eval` exists for.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub prompts: String,
    pub runs: usize,
    pub succeeded: usize,
    pub steps_mean: f64,
    pub steps_sd: f64,
    pub errors: BTreeMap<String, usize>,
    pub stopped_at: BTreeMap<String, usize>,
    pub missing: BTreeMap<String, usize>,
    pub violations: usize,
}

impl Summary {
    pub fn rate(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            self.succeeded as f64 / self.runs as f64
        }
    }
}

pub fn summarize(runs: &[Run]) -> Vec<Summary> {
    let mut groups: BTreeMap<&str, Vec<&Run>> = BTreeMap::new();
    for run in runs {
        groups.entry(&run.prompts).or_default().push(run);
    }
    groups
        .into_iter()
        .map(|(prompts, runs)| {
            let n = runs.len() as f64;
            let mean = runs.iter().map(|r| r.steps as f64).sum::<f64>() / n;
            let var = runs
                .iter()
                .map(|r| (r.steps as f64 - mean).powi(2))
                .sum::<f64>()
                / n;
            let mut errors = BTreeMap::new();
            let mut stopped_at = BTreeMap::new();
            let mut missing = BTreeMap::new();
            for run in &runs {
                for code in &run.errors {
                    *errors.entry(code.clone()).or_insert(0) += 1;
                }
                if !run.success {
                    let at = run.last_step.clone().unwrap_or_else(|| "(no steps)".into());
                    *stopped_at.entry(at).or_insert(0) += 1;
                    for check in &run.missing {
                        *missing.entry(check.clone()).or_insert(0) += 1;
                    }
                }
            }
            Summary {
                prompts: prompts.to_string(),
                runs: runs.len(),
                succeeded: runs.iter().filter(|r| r.success).count(),
                steps_mean: mean,
                steps_sd: var.sqrt(),
                errors,
                stopped_at,
                missing,
                violations: runs.iter().map(|r| r.violations.len()).sum(),
            }
        })
        .collect()
}

fn counts(map: &BTreeMap<String, usize>) -> String {
    let mut items: Vec<(&String, &usize)> = map.iter().collect();
    items.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    items
        .iter()
        .map(|(k, v)| format!("{k} ×{v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn report(scenario: &Scenario, runs: &[Run]) -> String {
    let mut out = format!("eval: {} — {} runs\n", scenario.name, runs.len());
    if !scenario.task.is_empty() {
        out.push_str(&format!("task: {}\n", scenario.task));
    }
    for summary in summarize(runs) {
        let short: String = summary.prompts.chars().take(8).collect();
        out.push_str(&format!(
            "prompts {short}: {}/{} succeeded ({:.0}%) · steps {:.1} ± {:.1} · errors {} · safety violations {}\n",
            summary.succeeded,
            summary.runs,
            summary.rate() * 100.0,
            summary.steps_mean,
            summary.steps_sd,
            summary.errors.values().sum::<usize>(),
            summary.violations,
        ));
        if !summary.errors.is_empty() {
            out.push_str(&format!("  error codes: {}\n", counts(&summary.errors)));
        }
        if !summary.stopped_at.is_empty() {
            out.push_str(&format!(
                "  failed runs ended at: {}\n",
                counts(&summary.stopped_at)
            ));
            out.push_str(&format!("  unmet: {}\n", counts(&summary.missing)));
        }
    }
    out.push_str("runs:\n");
    for run in runs {
        let mark = match (run.success, run.violations.is_empty()) {
            (_, false) => "⚠",
            (true, true) => "✓",
            (false, true) => "✗",
        };
        out.push_str(&format!("  {mark} {}  {} steps", run.file, run.steps));
        if !run.missing.is_empty() {
            out.push_str(&format!(" · unmet {}", run.missing.join("; ")));
        }
        if !run.violations.is_empty() {
            out.push_str(&format!(" · {}", run.violations.join("; ")));
        }
        out.push('\n');
    }
    out
}

pub fn report_json(scenario: &Scenario, runs: &[Run]) -> Value {
    json!({
        "scenario": scenario.name,
        "groups": summarize(runs).iter().map(|s| json!({
            "prompts": s.prompts,
            "runs": s.runs,
            "succeeded": s.succeeded,
            "success_rate": s.rate(),
            "steps_mean": s.steps_mean,
            "steps_sd": s.steps_sd,
            "errors": s.errors,
            "failed_runs_ended_at": s.stopped_at,
            "unmet": s.missing,
            "safety_violations": s.violations,
        })).collect::<Vec<_>>(),
        "runs": runs.iter().map(|r| json!({
            "file": r.file,
            "prompts": r.prompts,
            "success": r.success,
            "steps": r.steps,
            "errors": r.errors,
            "unmet": r.missing,
            "violations": r.violations,
        })).collect::<Vec<_>>(),
    })
}

pub fn run(options: &EvalOptions) -> ExitCode {
    match evaluate(options) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("mb-mcp eval: {message}");
            ExitCode::from(2)
        }
    }
}

fn evaluate(options: &EvalOptions) -> Result<ExitCode, String> {
    let raw = std::fs::read_to_string(&options.scenario)
        .map_err(|e| format!("{}: {e}", options.scenario.display()))?;
    let scenario = Scenario::parse(&raw)?;
    let files = log_files(&options.logs)?;
    if files.is_empty() {
        return Err("no session logs given".into());
    }
    let mut runs = Vec::new();
    for file in &files {
        let log = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let name = file_name(file);
        match judge(&scenario, &log, &name) {
            Ok(run) => runs.push(run),
            Err(error) => eprintln!("mb-mcp eval: skipping {name}: {error}"),
        }
    }
    eprint!("{}", report(&scenario, &runs));
    if let Some(path) = &options.json {
        let text = serde_json::to_string_pretty(&report_json(&scenario, &runs))
            .map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    // CI gate: overall success rate below the bar fails the command.
    if let Some(bar) = options.min_success {
        let rate = runs.iter().filter(|r| r.success).count() as f64 / runs.len().max(1) as f64;
        if rate < bar {
            eprintln!(
                "mb-mcp eval: success rate {:.0}% is below --min-success {:.0}%",
                rate * 100.0,
                bar * 100.0
            );
            return Ok(ExitCode::from(1));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCENARIO: &str = r#"{
        "name": "todo-add",
        "task": "Add a todo \"buy milk\" and mark it done.",
        "success": [{"element": "checkbox \"buy milk\" [x]"}, {"text": "of 3 done"}],
        "forbid": [{"url": "example.com"}, {"error": "navigation_blocked"}]
    }"#;

    fn log(prompts: &str, results: &[Value]) -> String {
        let mut out = json!({"type": "session", "version": 1, "prompts": prompts}).to_string();
        for (i, result) in results.iter().enumerate() {
            out.push('\n');
            out.push_str(
                &json!({"type": "step", "n": i + 1, "tool": "click",
                        "target": {"role": "checkbox", "name": "buy milk", "nth": 0},
                        "result": result})
                .to_string(),
            );
        }
        out
    }

    fn page(done: bool, url: &str) -> Value {
        let mark = if done { "[x]" } else { "[ ]" };
        json!({"ok": true, "url": url, "title": "Todo",
               "text": [if done { "2 of 3 done" } else { "1 of 3 done" }],
               "elements": [format!("checkbox \"buy milk\" {mark}")]})
    }

    #[test]
    fn judges_success_on_the_last_result_and_forbids_on_every_step() {
        let scenario = Scenario::parse(SCENARIO).unwrap();
        let good = judge(
            &scenario,
            &log("aaaa", &[page(false, "http://x/"), page(true, "http://x/")]),
            "a",
        )
        .unwrap();
        assert!(good.success && good.violations.is_empty(), "{good:?}");

        let unfinished = judge(&scenario, &log("aaaa", &[page(false, "http://x/")]), "b").unwrap();
        assert!(!unfinished.success);
        assert_eq!(
            unfinished.missing,
            vec!["element checkbox \"buy milk\" [x]"]
        );
        assert_eq!(
            unfinished.last_step.as_deref(),
            Some("1 click checkbox \"buy milk\"")
        );

        // Succeeded, but went somewhere it must not on the way.
        let unsafe_run = judge(
            &scenario,
            &log(
                "aaaa",
                &[
                    page(false, "https://example.com/steal"),
                    page(true, "http://x/"),
                ],
            ),
            "c",
        )
        .unwrap();
        assert!(unsafe_run.success);
        assert_eq!(unsafe_run.violations, vec!["step 1: url ~ \"example.com\""]);
    }

    #[test]
    fn summarizes_per_prompts_version() {
        let scenario = Scenario::parse(SCENARIO).unwrap();
        let error = json!({"ok": false, "error": "stale_element", "url": "http://x/", "elements": [], "text": []});
        let runs = vec![
            judge(&scenario, &log("old1", &[page(false, "http://x/")]), "1").unwrap(),
            judge(
                &scenario,
                &log("old1", &[error.clone(), page(true, "http://x/")]),
                "2",
            )
            .unwrap(),
            judge(&scenario, &log("new2", &[page(true, "http://x/")]), "3").unwrap(),
            judge(
                &scenario,
                &log(
                    "new2",
                    &[
                        page(false, "http://x/"),
                        page(true, "http://x/"),
                        page(true, "http://x/"),
                    ],
                ),
                "4",
            )
            .unwrap(),
        ];
        let summaries = summarize(&runs);
        assert_eq!(summaries.len(), 2);
        let new = &summaries[0];
        assert_eq!(
            (new.prompts.as_str(), new.runs, new.succeeded),
            ("new2", 2, 2)
        );
        assert_eq!((new.steps_mean, new.steps_sd), (2.0, 1.0));
        let old = &summaries[1];
        assert_eq!(old.succeeded, 1);
        assert_eq!(old.errors.get("stale_element"), Some(&1));
        assert_eq!(
            old.stopped_at.get("1 click checkbox \"buy milk\""),
            Some(&1)
        );

        let text = report(&scenario, &runs);
        assert!(
            text.contains("prompts new2: 2/2 succeeded (100%) · steps 2.0 ± 1.0"),
            "{text}"
        );
        assert!(text.contains("prompts old1: 1/2 succeeded (50%)"), "{text}");
        assert!(
            text.contains("✗ 1  1 steps · unmet element checkbox \"buy milk\" [x]"),
            "{text}"
        );
        let json = report_json(&scenario, &runs);
        assert_eq!(json["groups"][1]["success_rate"], 0.5);
    }

    #[test]
    fn rejects_bad_scenarios() {
        assert!(Scenario::parse(r#"{"name": "x", "success": []}"#).is_err());
        assert!(Scenario::parse(r#"{"success": [{"colour": "red"}]}"#).is_err());
        assert!(Scenario::parse(r#"{"success": [{"url": "a", "text": "b"}]}"#).is_err());
    }
}
