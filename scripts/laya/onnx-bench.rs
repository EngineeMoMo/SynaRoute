use laya::{LayaEngine, LayaOptions, Question, QuestionSet};
use serde_json::{json, Value};
use std::{fs, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args().collect();
    let tasks: Value = serde_json::from_slice(&fs::read(&arguments[2])?)?;
    let start = Instant::now();
    let engine = LayaEngine::create(LayaOptions { model_directory: Some(arguments[1].clone().into()), intra_op_threads: Some(2), inter_op_threads: Some(1), allow_download: false, ..Default::default() })?;
    let load_ms = start.elapsed().as_secs_f64() * 1000.0;
    let questions = QuestionSet::new().with("deep", Question::noul(tasks["question"].as_str().unwrap(), Value::Null, Value::Null))?;
    let mut results = Vec::new();
    for task in tasks["tasks"].as_array().unwrap() {
        let mut latencies = Vec::new();
        let mut score = None;
        let mut input_tokens = 0;
        for _ in 0..3 {
            let start = Instant::now();
            let answer = engine.predict(task["text"].clone(), &questions)?;
            latencies.push(start.elapsed().as_secs_f64() * 1000.0);
            score = answer.get("deep").and_then(|answer| answer.as_noul()).map(|answer| answer.probability);
            input_tokens = answer.usage().input_tokens;
        }
        let mut row = task.clone();
        row["score"] = json!(score);
        row["upgrade"] = json!(score.is_some_and(|score| score >= tasks["threshold"].as_f64().unwrap()));
        row["latencies_ms"] = json!(latencies);
        row["input_tokens"] = json!(input_tokens);
        results.push(row);
    }
    fs::write(&arguments[3], serde_json::to_vec_pretty(&json!({"label":"rust-onnx", "load_ms":load_ms, "results":results}))?)?;
    Ok(())
}
