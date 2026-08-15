import { describe, it, expect, beforeEach, afterEach, vi, type MockInstance } from "vitest";
import { useStore, BALANCE_TTL_MS } from "@/store";
import { api } from "@/lib/bridge";
import type { BalanceResult } from "@/types";

/**
 * `refreshBalance` 的三条判据。这三条**浏览器里验不出来**（看不见请求次数），
 * 而它们各自对应一个真实故障：
 *
 * 1. **并发去重** —— 不做的话 StrictMode 下卡片一挂载就发两个一模一样的请求；
 *    N 张卡片就是 2N 个。计费接口被打得很勤，部分中转站对此限流。
 * 2. **TTL 复用** —— 不做的话每次切分类回来都重查一轮。
 * 3. **指纹失配必重查** —— 不做的话用户把查询地址改对、保存后，卡片仍显示上一次那条
 *    404，而那正是他刚修掉的东西，看着像「改了没生效」。
 */

const okResult = (at: number): BalanceResult => ({
  ok: true,
  remaining: 42,
  unit: "USD",
  queriedAt: at,
});

// 按被打桩方法的**真实签名**标注。写成宽泛的 `ReturnType<typeof vi.spyOn>` 会因形参
// 逆变而报错（unknown 不能赋给 string）；`vi.spyOn<typeof api, "...">` 在这个对象上
// 也不满足约束（api 的成员是箭头函数属性而非方法）。直接用 MockInstance 最省事。
let spy: MockInstance<(keyId: string) => Promise<BalanceResult>> | undefined;

beforeEach(() => {
  useStore.setState({ balances: {}, balanceLoading: {} });
});

afterEach(() => {
  spy?.mockRestore();
});

describe("refreshBalance", () => {
  it("并发去重：同一 Key 连发两次只打一次上游（StrictMode 双跑的防线）", async () => {
    let resolveFn: (r: BalanceResult) => void = () => {};
    spy = vi.spyOn(api, "queryKeyBalance").mockImplementation(
      () => new Promise<BalanceResult>((res) => { resolveFn = res; }) as never,
    );

    const { refreshBalance } = useStore.getState();
    // 不 await 第一个：模拟 effect 连跑两次、第一次还没结算
    const p1 = refreshBalance("k1", "fp1");
    const p2 = refreshBalance("k1", "fp1");
    expect(spy).toHaveBeenCalledTimes(1);

    resolveFn(okResult(Date.now()));
    await Promise.all([p1, p2]);
    expect(spy).toHaveBeenCalledTimes(1);
    // 去重不能把 loading 旗留在表里，否则这条 Key 此后永远查不动
    expect(useStore.getState().balanceLoading["k1"]).toBeUndefined();
  });

  it("TTL 内且指纹相同：复用缓存，不打上游", async () => {
    useStore.setState({
      balances: { k1: { result: okResult(Date.now()), fingerprint: "fp1" } },
    });
    spy = vi.spyOn(api, "queryKeyBalance");

    await useStore.getState().refreshBalance("k1", "fp1");
    expect(spy).not.toHaveBeenCalled();
  });

  it("超过 TTL：重查", async () => {
    useStore.setState({
      balances: {
        k1: { result: okResult(Date.now() - BALANCE_TTL_MS - 1000), fingerprint: "fp1" },
      },
    });
    spy = vi.spyOn(api, "queryKeyBalance").mockResolvedValue(okResult(Date.now()) as never);

    await useStore.getState().refreshBalance("k1", "fp1");
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it("指纹变了（用户改了查询地址）：即使缓存很新也必须重查", async () => {
    useStore.setState({
      balances: { k1: { result: okResult(Date.now()), fingerprint: "fp-old" } },
    });
    spy = vi.spyOn(api, "queryKeyBalance").mockResolvedValue(okResult(Date.now()) as never);

    await useStore.getState().refreshBalance("k1", "fp-new");
    expect(spy).toHaveBeenCalledTimes(1);
    // 新结果要带上新指纹，否则下一次渲染又判成失配、无限重查
    expect(useStore.getState().balances["k1"].fingerprint).toBe("fp-new");
  });

  it("force（用户手点刷新）：绕过 TTL", async () => {
    useStore.setState({
      balances: { k1: { result: okResult(Date.now()), fingerprint: "fp1" } },
    });
    spy = vi.spyOn(api, "queryKeyBalance").mockResolvedValue(okResult(Date.now()) as never);

    await useStore.getState().refreshBalance("k1", "fp1", true);
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it("传 maxAgeMs（轮询用它把间隔当作新鲜度门槛）：缓存超龄才打上游", async () => {
    // 场景：用户设的自动间隔是 1 分钟（60_000 ms），现在缓存是 59 秒前的 —— 还没到期
    const customTTL = 60_000;
    useStore.setState({
      balances: { k1: { result: okResult(Date.now() - 59_000), fingerprint: "fp1" } },
    });
    spy = vi.spyOn(api, "queryKeyBalance");

    // 传了 maxAgeMs=60_000，59 秒内的缓存判为「还新鲜」→ 不打
    await useStore.getState().refreshBalance("k1", "fp1", false, customTTL);
    expect(spy).not.toHaveBeenCalled();

    // 同样的缓存，但已经 61 秒了 → 必须重查
    useStore.setState({
      balances: { k1: { result: okResult(Date.now() - 61_000), fingerprint: "fp1" } },
    });
    spy = vi.spyOn(api, "queryKeyBalance").mockResolvedValue(okResult(Date.now()) as never);
    await useStore.getState().refreshBalance("k1", "fp1", false, customTTL);
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it("查询失败的结果也要落缓存（卡片要显示那句原因，而不是停在「未查询」）", async () => {
    const failed: BalanceResult = {
      ok: false,
      queriedAt: Date.now(),
      error: "查询超时（10s）",
    };
    spy = vi.spyOn(api, "queryKeyBalance").mockResolvedValue(failed as never);

    await useStore.getState().refreshBalance("k1", "fp1");
    const cached = useStore.getState().balances["k1"];
    expect(cached.result.ok).toBe(false);
    expect(cached.result.error).toBe("查询超时（10s）");
    // 关键：绝不能因为失败就编一个 remaining 出来（显示「余额 0」会让用户以为额度用光）
    expect(cached.result.remaining).toBeUndefined();
  });

  it("IPC 本身抛错也如实入缓存，不静默（否则卡片永远停在「未查询」）", async () => {
    spy = vi.spyOn(api, "queryKeyBalance").mockRejectedValue(new Error("boom") as never);
    const err = vi.spyOn(console, "error").mockImplementation(() => {});

    await useStore.getState().refreshBalance("k1", "fp1");
    const cached = useStore.getState().balances["k1"];
    expect(cached.result.ok).toBe(false);
    expect(cached.result.error).toContain("boom");
    expect(useStore.getState().balanceLoading["k1"]).toBeUndefined();
    err.mockRestore();
  });
});

describe("deleteKey 清理余额缓存", () => {
  it("删 Key 时连带清掉它的余额缓存（该表按 keyId 只增不减）", async () => {
    useStore.setState({
      balances: {
        k1: { result: okResult(Date.now()), fingerprint: "fp1" },
        k2: { result: okResult(Date.now()), fingerprint: "fp2" },
      },
    });
    const del = vi.spyOn(api, "deleteKey").mockResolvedValue(undefined as never);
    // loadCategory 会打 4 个 IPC，这里只关心缓存清理，整条打桩掉
    const load = vi.spyOn(useStore.getState(), "loadCategory").mockResolvedValue();
    useStore.setState({ loadCategory: load as never });

    await useStore.getState().deleteKey("k1");
    expect(useStore.getState().balances["k1"]).toBeUndefined();
    // 只清目标那条，别的不能受影响
    expect(useStore.getState().balances["k2"]).toBeDefined();
    del.mockRestore();
  });
});
