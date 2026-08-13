import { describe, it, expect } from "vitest";
import { formatBalanceAmount, usedPercent, balanceFingerprint } from "./balance";
import type { BalanceQuery, ProviderKey } from "@/types";

/**
 * 这组测试钉住的核心判据只有一条：**任何非零余额都不能被显示成 0**。
 *
 * 由来：后端刻意让 `remaining` 是 `Option<f64>` 而非 0（`docs/17` §2.1：显示 0 会让
 * 用户以为额度真用光了，比不显示更糟）。若前端最后一步用固定两位小数格式化，
 * `0.0042 USD` 就变成 `0.00` —— 后端为此付的代价被一行格式化代码毁掉，
 * 且这种失效在 tsc 和肉眼 review 下都看不出来。
 */
describe("formatBalanceAmount", () => {
  it("整数不带小数、带千分位", () => {
    expect(formatBalanceAmount(0)).toBe("0");
    expect(formatBalanceAmount(1024)).toBe("1,024");
    expect(formatBalanceAmount(1_234_567)).toBe("1,234,567");
  });

  it("常规小数保两位", () => {
    expect(formatBalanceAmount(84.2)).toBe("84.20");
    expect(formatBalanceAmount(3.456)).toBe("3.46");
  });

  it("极小非零额度不塌成 0（本文件的核心判据）", () => {
    for (const tiny of [0.0042, 0.001, 0.0000001, 1e-9]) {
      const out = formatBalanceAmount(tiny);
      // 判据不写死具体字符串（那会把格式钉死、挡住后续调整），
      // 而是校验「格式化结果反解回来仍不为 0」——这才是要守的语义。
      expect(Number(out.replace(/,/g, ""))).not.toBe(0);
      expect(out).not.toBe("0.00");
    }
  });

  it("负余额（欠费站点）如实显示负号，不取绝对值", () => {
    expect(formatBalanceAmount(-12.5)).toBe("-12.50");
    expect(formatBalanceAmount(-3)).toBe("-3");
  });

  it("非有限值不编数字，回退占位符", () => {
    expect(formatBalanceAmount(NaN)).toBe("?");
    expect(formatBalanceAmount(Infinity)).toBe("?");
  });
});

describe("usedPercent", () => {
  it("有 total 才给百分比", () => {
    expect(usedPercent(25, 100)).toBe(75);
    expect(usedPercent(100, 100)).toBe(0);
    expect(usedPercent(0, 100)).toBe(100);
  });

  it("total 缺失或非正时返回 null，不拿假分母凑数", () => {
    expect(usedPercent(25, undefined)).toBeNull();
    expect(usedPercent(25, 0)).toBeNull();
    expect(usedPercent(25, -5)).toBeNull();
  });

  it("remaining 超出 total 或为负时钳到 0~100", () => {
    // 赠送额度不计入 total 的站点会给出 remaining > total
    expect(usedPercent(150, 100)).toBe(0);
    // 欠费站点的负余额
    expect(usedPercent(-10, 100)).toBe(100);
  });
});

/** 造一条最小可用的 Key，只填指纹关心的字段。 */
function keyWith(baseUrl: string, balance?: Partial<BalanceQuery>): ProviderKey {
  return {
    id: "k1",
    categoryId: "claude-cli",
    name: "k",
    vendor: "custom",
    baseUrl,
    protocol: "anthropic",
    hasSecret: true,
    enabled: true,
    priority: 0,
    params: {},
    models: [],
    mappings: [],
    health: { status: "unknown", failCount: 0 },
    balanceQuery: balance
      ? {
          enabled: true,
          template: "generic",
          url: "{{baseUrl}}/user/balance",
          method: "GET",
          auth: "bearer",
          timeoutSecs: 10,
          autoIntervalMin: 0,
          ...balance,
        }
      : undefined,
  } as ProviderKey;
}

describe("balanceFingerprint", () => {
  it("同一份配置指纹稳定（否则卡片每次渲染都判成配置已变、无限重查）", () => {
    const a = keyWith("https://api.example.com", {});
    const b = keyWith("https://api.example.com", {});
    expect(balanceFingerprint(a)).toBe(balanceFingerprint(b));
  });

  it("影响请求的字段变了，指纹必须变", () => {
    const base = balanceFingerprint(keyWith("https://api.example.com", {}));
    const changed = [
      // Key 自己的接口地址：{{baseUrl}} 展开后就是它
      balanceFingerprint(keyWith("https://api2.example.com", {})),
      balanceFingerprint(keyWith("https://api.example.com", { url: "{{origin}}/balance" })),
      balanceFingerprint(keyWith("https://api.example.com", { method: "POST" })),
      balanceFingerprint(keyWith("https://api.example.com", { auth: "x-api-key" })),
      balanceFingerprint(keyWith("https://api.example.com", { baseUrlOverride: "https://p.x.com" })),
      balanceFingerprint(keyWith("https://api.example.com", { timeoutSecs: 30 })),
      balanceFingerprint(keyWith("https://api.example.com", { remainingPath: "data.balance" })),
      balanceFingerprint(keyWith("https://api.example.com", { accessToken: "tok" })),
      balanceFingerprint(keyWith("https://api.example.com", { userId: "7" })),
    ];
    for (const fp of changed) expect(fp).not.toBe(base);
  });

  it("enabled / template 变化不改指纹（关开关、切预设按钮不该触发重查）", () => {
    const base = balanceFingerprint(keyWith("https://api.example.com", {}));
    expect(balanceFingerprint(keyWith("https://api.example.com", { enabled: false }))).toBe(base);
    expect(balanceFingerprint(keyWith("https://api.example.com", { template: "custom" }))).toBe(base);
  });

  it("空覆盖项与「未填」等价：编辑器落 undefined、后端回 undefined，两侧指纹必须一致", () => {
    // 若这条不成立，每次保存后卡片都会判成「配置变了」而白发一个请求
    const withEmpty = balanceFingerprint(
      keyWith("https://api.example.com", { baseUrlOverride: undefined, remainingPath: undefined }),
    );
    const withoutFields = balanceFingerprint(keyWith("https://api.example.com", {}));
    expect(withEmpty).toBe(withoutFields);
  });

  it("未配置余额查询时返回空串", () => {
    expect(balanceFingerprint(keyWith("https://api.example.com"))).toBe("");
  });
});
