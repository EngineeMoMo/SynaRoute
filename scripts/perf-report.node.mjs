import { test } from "node:test";
import assert from "node:assert/strict";
import { parseMeasurements, compareReports } from "./perf-report.mjs";

test("extracts performance measurements from cargo output and rejects malformed data", () => {
  assert.deepEqual(parseMeasurements('test test_name ... SYNAROUTE_PERF {"name":"scenario"}\nok\n'), [{ name: "scenario" }]);
  assert.throws(() => parseMeasurements("SYNAROUTE_PERF broken"));
});
test("compares matching scenarios and reports incompatible environments", () => {
  const environment = { platform: "win32", arch: "x64", cpu: "test", profile: "release", rust: "test" };
  const baseline = { environment, measurements: [{ name: "scenario", headers_us: { p50: 100, p99: 0 } }] };
  const current = { environment, measurements: [{ name: "scenario", headers_us: { p50: 80, p99: 40 } }] };
  const result = compareReports(baseline, current);
  assert.equal(result.compatible, true);
  assert.equal(result.comparisons.length, 1);
  assert.ok(Math.abs(result.comparisons[0].changePercent + 20) < 0.001);
  assert.equal(compareReports(baseline, { ...current, environment: { ...environment, cpu: "other" } }).compatible, false);
});
