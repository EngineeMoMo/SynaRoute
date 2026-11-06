export function comparisonRef(env, event, git) {
  if (env.GITHUB_ACTIONS !== "true") return "HEAD";
  const sha = value => typeof value === "string" && /^[a-f0-9]{40,64}$/i.test(value) && !/^0+$/.test(value);
  if (["pull_request", "pull_request_target"].includes(env.GITHUB_EVENT_NAME)) {
    const base = event.pull_request?.base?.sha;
    if (!sha(base)) throw new Error("PR 缺少有效基准提交");
    return base;
  }
  if (env.GITHUB_REF?.startsWith("refs/tags/")) {
    // A tag push's before SHA is often zero; compare with the previous version tag, not itself.
    const tag = git(["describe", "--tags", "--abbrev=0", "--match", "v[0-9]*", "HEAD^"]).trim();
    return `refs/tags/${tag}`;
  }
  if (env.GITHUB_EVENT_NAME === "push") {
    if (!sha(event.before)) throw new Error("push 缺少有效 before 提交，不能与自身比较");
    return event.before;
  }
  if (env.GITHUB_EVENT_NAME === "workflow_dispatch") return "HEAD^";
  throw new Error(`没有为 CI 事件 ${env.GITHUB_EVENT_NAME} 定义棘轮基准`);
}
