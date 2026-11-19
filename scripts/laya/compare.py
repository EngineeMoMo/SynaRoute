import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import sys
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source")
    parser.add_argument("--model", required=True)
    parser.add_argument("--tasks", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--threads", type=int, default=2)
    arguments = parser.parse_args()
    if arguments.source:
        sys.path.insert(0, arguments.source)
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TOKENIZERS_PARALLELISM"] = "false"
    start = time.perf_counter()
    import torch
    import laya
    import psutil
    import numpy
    import transformers
    torch.set_num_threads(arguments.threads)
    agent = laya.Agent(arguments.model, device="cpu")
    load_ms = (time.perf_counter() - start) * 1000
    raw = Path(arguments.tasks).read_bytes()
    tasks = json.loads(raw)
    questions = {"deep": {"type": "noul", "instructions": tasks["question"]}}
    results = []
    for task in tasks["tasks"]:
        latencies = []
        for repeat in range(3):
            start = time.perf_counter()
            answer = agent.predict(task["text"], questions)
            latencies.append((time.perf_counter() - start) * 1000)
        score = answer.get("answers", {}).get("deep", {}).get("noul")
        usage = answer.get("usage", {})
        truncated = bool(usage.get("truncated") or usage.get("state_tokens_dropped"))
        results.append({**task, "score": score, "upgrade": isinstance(score, (int, float)) and score >= tasks["threshold"] and not truncated,
                        "latencies_ms": latencies, "usage": usage})
    process = psutil.Process()
    memory = process.memory_info()
    report = {"label": arguments.label, "source": laya.__file__, "model": arguments.model,
              "runtime": {"python": sys.version, "torch": torch.__version__, "numpy": numpy.__version__, "transformers": transformers.__version__},
              "threads": arguments.threads, "dataset_sha256": hashlib.sha256(raw).hexdigest(),
              "load_ms": load_ms, "rss_bytes": memory.rss, "peak_rss_bytes": getattr(memory, "peak_wset", None),
              "warm_median_ms": statistics.median(row["latencies_ms"][-1] for row in results), "results": results}
    for split in ("calibration", "held_out"):
        rows = [row for row in results if row["split"] == split]
        report[split] = {"count": len(rows), "true_upgrades": sum(row["upgrade"] and row["hard"] for row in rows),
                         "false_upgrades": sum(row["upgrade"] and not row["hard"] for row in rows),
                         "hard_count": sum(row["hard"] for row in rows)}
    Path(arguments.output).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({key: value for key, value in report.items() if key != "results"}, ensure_ascii=False))


if __name__ == "__main__":
    main()
