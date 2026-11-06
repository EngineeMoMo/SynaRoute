"""Controlled mutations of active-request modules only; never reset the checkout.
Run from repo root: python -I scripts/verify-active-request-mutations.py
Each mutation restores exact bytes with a fresh mtime in finally. Logs kept under .build-tmp.
A nonzero exit only counts if Rust actually reports failing tests (not compile/link errors).
"""
from pathlib import Path
import os, re, subprocess, time, json

ROOT = Path(__file__).resolve().parents[1]
CARGO = Path.home() / '.cargo/bin/cargo.exe'
OUT = ROOT / '.build-tmp/active-mutations'
OUT.mkdir(parents=True, exist_ok=True)
ACTIVE = 'src-tauri/src/proxy/active_requests.rs'
STREAM = 'src-tauri/src/proxy/streaming.rs'
CASES = [
 ('handler-cancel', ACTIVE, '_ = control.cancelled() => Ok(stopped_response()),', '_ = std::future::pending::<()>() => Ok(stopped_response()),', 'queued_request_can_be_stopped'),
 ('backpressure-cancel', ACTIVE, '_ = c.cancelled() => break,\n                sent = tx.send(frame)', '_ = std::future::pending::<()>() => break,\n                sent = tx.send(frame)', 'stop_under_backpressure'),
 ('unregister', ACTIVE, 'self.registry.0.lock().remove(&self.control.0.id);', 'let _ = &self.registry;', 'registry_is_scoped'),
 ('cancel-frame', ACTIVE, 'return Poll::Ready(Some(stopped_frame(self.protocol, self.sse)));', 'return Poll::Ready(None);', 'all_downstream_protocols_receive'),
 ('terminal-race', ACTIVE, 'let end = *p.end.get_or_insert(end);', 'p.end = Some(end); let end = p.end.unwrap();', 'registry_is_scoped'),
 ('category-isolation', ACTIVE, '.filter(|e| e.category == category).cloned();', '.filter(|_e| { let _ = category; true }).cloned();', 'registry_is_scoped'),
 ('usage-settlement', STREAM, 'self.store.backfill_usage_for_collapsed_event(self.category, Some(&self.key_id), &collapse, u, &self.real_model);', 'let _ = (&self.store, &collapse, u);', 'live_stream_stop_preserves_usage'),
 ('operator-health', STREAM, 'if !matches!(self.active.end(), Some(End::Operator | End::Disconnected)) && (self.eof || self.errored) {', 'if self.eof || self.errored || self.active.end() == Some(End::Operator) {', 'operator_stop_preserves_existing_binding'),
]
results = []
for name, relative, old, new, test in CASES:
    path = ROOT / relative
    original = path.read_bytes()
    text = original.decode('utf-8')
    newline = '\r\n' if '\r\n' in text else '\n'
    old, new = old.replace('\n', newline), new.replace('\n', newline)
    if text.count(old) != 1:
        raise RuntimeError(f'{name}: expected one anchor, got {text.count(old)}')
    changed = text.replace(old, new).encode('utf-8')
    try:
        path.write_bytes(changed)
        cmd = [str(CARGO), 'test', '--lib', 'active_requests::tests::' + test, '--', '--nocapture']
        try:
            run = subprocess.run(cmd, cwd=ROOT/'src-tauri', capture_output=True, timeout=120)
            output = (run.stdout + run.stderr).decode('utf-8', errors='replace')
            (OUT / (name + '.log')).write_text(output, encoding='utf-8')
            matched = re.search(r'test result: FAILED\. (\d+) passed; (\d+) failed;', output)
            status = 'caught' if run.returncode and matched and int(matched[2]) > 0 else 'NOT_CAUGHT'
        except subprocess.TimeoutExpired as error:
            status = 'TIMEOUT'
            (OUT / (name + '.log')).write_bytes((error.stdout or b'') + (error.stderr or b''))
    finally:
        if path.read_bytes() != changed:
            raise RuntimeError(f'{name}: file changed concurrently; refusing to overwrite it; original bytes saved separately')
        path.write_bytes(original)
        os.utime(path, None) # Never restore old timestamps: cargo must rebuild the clean source.
    results.append({'mutation': name, 'status': status, 'test': test})
    print(f'{name}: {status}', flush=True)
    (OUT/'results.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
    if status != 'caught':
        raise SystemExit(f'{name}: investigate before continuing')
print('All source files restored; run clean regression next.', flush=True)
