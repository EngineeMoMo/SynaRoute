export function parseMeasurements(output) {
  return output.split(/\r?\n/).flatMap((line) => {
    const marker = line.indexOf("SYNAROUTE_PERF ");
    return marker < 0 ? [] : [JSON.parse(line.slice(marker + "SYNAROUTE_PERF ".length))];
  });
}

export function compareReports(baseline, current) {
  const compatible = ["platform", "arch", "cpu", "profile", "rust"].every((field) => baseline.environment[field] === current.environment[field]);
  const comparisons = [];
  for (const measurement of current.measurements) {
    const previous = baseline.measurements.find((entry) => entry.name === measurement.name);
    if (!previous) continue;
    for (const field of ["headers_us", "total_us", "direct_headers_us", "paired_header_overhead_us"]) {
      for (const percentile of ["p50", "p95", "p99"]) {
        const before = previous[field]?.[percentile];
        const after = measurement[field]?.[percentile];
        if (typeof before !== "number" || before <= 0 || typeof after !== "number") continue;
        comparisons.push({ scenario: measurement.name, metric: `${field}.${percentile}`, before, after, changePercent: (after / before - 1) * 100 });
      }
    }
  }
  return { compatible, comparisons };
}
