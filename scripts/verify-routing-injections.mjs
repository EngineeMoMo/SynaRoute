import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import {spawnSync} from 'node:child_process';
const root=process.cwd();
const cargo=path.join(os.homedir(),'.cargo/bin/cargo.exe');
const cases=[
 ['preview-expiry','src-tauri/src/routing/scenes.rs','plan.expires <= Instant::now() || plan.fingerprint != fingerprint(&before)','plan.fingerprint != fingerprint(&before)','scenes_validate_all_categories_before_mutation_and_reject_stale_previews'],
 ['judge-budget','src-tauri/src/routing/intent.rs','Duration::from_secs(4).min(deadline.map(|d| d.saturating_duration_since(Instant::now())).unwrap_or(Duration::from_secs(4)))','Duration::from_secs(7)','routing_matrix_failure_cooldown_and_deadline'],
 ['judge-usage','src-tauri/src/upstream/routing_judge.rs','if let Some(usage) = super::extract_usage(&body) {','if let Some(usage) = super::extract_usage(&body).filter(|_| false) {','routing_matrix_three_protocols_single_flight_and_usage'],
 ['privacy-block','src-tauri/src/routing/privacy.rs','let blocked = config.mode == PrivacyMode::Block && !result.counts.is_empty();','let blocked = false;','routing_matrix_privacy_fail_closed_and_warn_preserves_body'],
 ['late-failure','src-tauri/src/routing/runtime.rs','state.binding.as_ref().is_some_and(|b| b.ticket == lease.ticket)','state.binding.is_some()','routing_matrix_same_turn_late_failure_and_failed_candidate'],
 ['single-flight','src-tauri/src/routing/intent.rs','let Ok(_guard) = tokio::time::timeout(limit, gate.lock()).await else { return Ok((group.default_model.clone(), "judge_timeout")); };','let Ok(guard) = tokio::time::timeout(limit, gate.lock()).await else { return Ok((group.default_model.clone(), "judge_timeout")); }; drop(guard);','routing_matrix_three_protocols_single_flight_and_usage'],
];
fs.mkdirSync('.build-tmp/routing-injections',{recursive:true});
const results=[];
for(const [name,file,from,to,test] of cases){
 const full=path.join(root,file), before=fs.readFileSync(full,'utf8');
 if(before.split(from).length!==2) throw Error(`nonunique anchor ${name}`);
 const modified=before.replace(from,to);
 fs.writeFileSync(`.build-tmp/routing-injections/${name}.bak`,before);
 try {
  fs.writeFileSync(full,modified);
  const run=spawnSync(cargo,['test','--locked','--lib','--manifest-path','src-tauri/Cargo.toml',test],{encoding:'utf8',timeout:90000});
  const output=(run.stdout??'')+(run.stderr??'');
  fs.writeFileSync(`.build-tmp/routing-injections/${name}.log`,output);
  const caught=run.status!==0 && /test result: FAILED/.test(output) && output.includes(test);
  results.push({name,caught,status:run.status});
  console.log(`${name}: ${caught?'regression detected':'NOT VERIFIED'}`);
 } finally {
  if(fs.readFileSync(full,'utf8')!==modified) throw Error(`Concurrent edit: refusing restore ${file}; backup preserved`);
  fs.writeFileSync(full,before);
 }
}
fs.writeFileSync('.build-tmp/routing-injections/results.json',JSON.stringify(results,null,2));
if(results.some(r=>!r.caught))process.exitCode=1;
