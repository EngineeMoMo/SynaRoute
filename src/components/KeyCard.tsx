import * as React from "react";
import { Card } from "@/components/ui/Card";
import { Badge } from "@/components/ui/Badge";
import { Switch } from "@/components/ui/Switch";
import { Button } from "@/components/ui/Button";
import { Tooltip } from "@/components/ui/Tooltip";
import { HealthBadge } from "@/components/HealthBadge";
import { BrandIcon } from "@/components/BrandIcon";
import { type ProviderKey, protocolLabel } from "@/types";
import { useStore } from "@/store";
import { formatRelativeTime } from "@/lib/utils";
import { balanceFingerprint, formatBalanceAmount, usedPercent } from "@/lib/balance";
import { usePolling } from "@/lib/usePolling";
import { openExternalUrl } from "@/lib/openExternal";
import { useT } from "@/lib/useT";
import { ChevronUp, ChevronDown, RefreshCw, Pencil, Trash2, ArrowRight, Wallet, ExternalLink } from "lucide-react";

/**
 * 单个厂商 Key 卡片（FR-001/003/006/010/011）。
 *
 * **用 memo 包住（PERF-2）**：CategoryPage 每 5s 轮询一次 `listKeys`，日志页每 2s 刷 events。
 * 一份分类下有 N 张卡片，无 memo 时每次轮询都要把 N 张全部重渲染一遍（含 Tooltip、
 * 徽标、Switch 全套子树），而绝大多数轮询的返回内容与上次完全一致。
 *
 * 这个 memo **依赖 store 侧的 `reuseUnchanged`**：IPC 每次都返回全新对象，
 * 若不在 store 里复用未变对象，`prevProps.k === nextProps.k` 恒为 false、memo 恒失效
 * （实测确认过：轮询后 k 引用必变而内容全等）。两者必须成对存在，改一处要想到另一处。
 */
export const KeyCard = React.memo(function KeyCard({ k, onEdit, isFirst, isLast, isRoutingPrimary }: {
  k: ProviderKey;
  onEdit: (k: ProviderKey) => void;
  isFirst: boolean;
  isLast: boolean;
  /**
   * 这条是否为**路由意义上**的主 Key（首个启用 Key）。
   *
   * 由父级算好传入，而不是在卡片内用 `k.priority === 0` 判 —— 后者需要看到整列才能得出
   * 正确结论（priority-0 可能被禁用、也可能多条同为 0），单张卡片没有那个视野。
   * 口径与 `routingPrimaryKey` / 后端 `enabled_keys_sorted` / 状态条 / 托盘完全一致。
   */
  isRoutingPrimary: boolean;
}) {
  // 细粒度订阅：KeyCard 会被渲染 N 份（每个 Key 一份），整店解构时任何无关字段变化
  // （如日志页每 2s 的 events）都会把整列卡片全部重渲染一遍。
  const toggleKey = useStore((s) => s.toggleKey);
  const deleteKey = useStore((s) => s.deleteKey);
  const checkHealth = useStore((s) => s.checkHealth);
  const moveKey = useStore((s) => s.moveKey);
  const setPrimaryKey = useStore((s) => s.setPrimaryKey);
  const vendors = useStore((s) => s.vendors);
  const t = useT();
  // Key 自己的 icon 覆盖优先于厂商的（见 ProviderKey.icon：「自定义」下多条 Key 各指不同站点，
  // 图标必须能单独设）。两者都空才退回按名字启发式猜。
  const vendorIcon = k.icon ?? vendors.find((v) => v.id === k.vendor)?.icon;
  // 就地二次确认：不用原生 confirm()（在 Tauri WebView2 里行为不可靠，会导致删除不触发）
  const [confirmingDelete, setConfirmingDelete] = React.useState(false);
  // 模型徽标折叠态（UX-3）：超过 3 个时默认折叠，点击「+N」展开
  const [modelsExpanded, setModelsExpanded] = React.useState(false);

  // ---- 余额（第④批）----
  // 只订阅本 Key 那一条，不订阅整张 balances 表：后者一变（任意别的 Key 查完余额）
  // 就会把这一列卡片全部重渲染，正是上面 memo 要避免的事。
  const balanceEntry = useStore((s) => s.balances[k.id]);
  const balanceBusy = useStore((s) => !!s.balanceLoading[k.id]);
  const refreshBalance = useStore((s) => s.refreshBalance);
  const showToast = useStore((s) => s.showToast);
  const balanceEnabled = !!k.balanceQuery?.enabled;
  // 站点域名根（用于「点地址在浏览器打开」）。取 origin 而非完整 baseUrl：后者带
  // `/v1` 之类 API 路径，浏览器打开多半是 404 或一段 JSON 错误；用户真正想去的是站点首页
  // （查额度/看公告/读文档）。地址不合法时为 null，此时不渲染可点链接。
  const siteOrigin = React.useMemo(() => {
    try {
      return new URL(k.baseUrl.trim()).origin;
    } catch {
      return null;
    }
  }, [k.baseUrl]);
  // 指纹进 deps：用户改了查询地址/认证方式后指纹变，这里会自动重查一次，
  // 而不是继续显示旧配置查出来的那条错误（看着像「改了没生效」）。
  const fingerprint = balanceFingerprint(k);

  React.useEffect(() => {
    // `k.enabled` 门：已禁用的 Key **不自动**打上游（用户禁用常因欠费/出问题，自动请求
    // 既烧额度又可能触发限流；健康探测对禁用 Key 同样不发，这里保持同一约定）。
    // 手动点刷新按钮不受此门限制 —— 用户主动要看时仍可查。
    if (!balanceEnabled || !k.enabled) return;
    // 是否真的发请求由 store 判定（指纹 + TTL + 并发去重），这里只管声明「该有值」。
    // 判定逻辑放 store 而不是这里，是因为 StrictMode 下本 effect 会跑两次，
    // 只有在 store 里同步读写那面旗才拦得住重复请求。
    void refreshBalance(k.id, fingerprint);
  }, [k.id, balanceEnabled, k.enabled, fingerprint, refreshBalance]);

  /**
   * 余额自动轮询（对齐 cc-switch 的 `autoQueryInterval`）。
   *
   * `autoIntervalMin` 此前是**死字段**：结构里有、UI 能填、值能落盘，却没有任何代码读它
   * —— 用户设了「每 30 分钟自动查」，实际永远只在打开界面时查一次。
   *
   * 三条刻意约束：
   * 1. **`0` = 不自动查**（与字段文档一致）。`usePolling` 对 `intervalMs <= 0` 直接不起表，
   *    故这里传 0 即可，无需另加分支。
   * 2. **`force: false`**：仍然过 store 的指纹 + TTL 判定。轮询只是「到点了去问一下」，
   *    真正要不要打上游由 store 决定 —— 否则用户把间隔设成 1 分钟就是每分钟一次真实上游
   *    请求，而余额几乎不会那么快变。
   * 3. **窗口不可见时停表**（`usePolling` 自带）：余额查询打的是真实上游、消耗额度，
   *    最小化到托盘后还在后台定时烧额度是用户不会预期的。
   */
  const autoIntervalMs = React.useMemo(() => {
    const min = k.balanceQuery?.autoIntervalMin ?? 0;
    // 下限 1 分钟：防手填 0.x 之类的极小值把上游打爆（0 仍然表示「关闭」，见上）。
    return min > 0 ? Math.max(min, 1) * 60_000 : 0;
  }, [k.balanceQuery?.autoIntervalMin]);

  usePolling(
    // 新鲜度门槛取**间隔的 90%**而不是间隔本身：挂载即查后 queriedAt ≈ 网络延迟 L，
    // 第 1 次 tick 在 t=T 时缓存年龄 = T−L 恒小于 T —— 门槛取 T 会把每个奇数次 tick
    // 全部拦掉，实际生效周期变成配置值的 2 倍（设 30 分钟实为 1 小时，静默偏差）。
    // 留 10% 余量后 tick 时年龄 T−L > 0.9T（只要 L < 0.1T，间隔下限 1 分钟 → 余量 6s，
    // 网络往返远小于它），奇数次 tick 正常放行。后端缓存同样留了 10% 余量（get_balance_cache）。
    () => void refreshBalance(k.id, fingerprint, false, autoIntervalMs * 0.9),
    autoIntervalMs,
    balanceEnabled && k.enabled,
  );

  const balance = balanceEntry?.result;
  // `remaining` 必须显式判 null 才显示：后端刻意让它可缺失（查不到时不给 0），
  // 这里若写 `balance.remaining ?? 0` 就会把「取不到」渲染成「余额 0」，
  // 让用户以为额度真用光了 —— 后端为此付的代价不能在最后一步毁掉。
  const hasAmount = balance?.ok && balance.remaining != null;
  const pct = hasAmount ? usedPercent(balance.remaining!, balance.total) : null;

  // 显示的模型列表：映射优先，否则显示前几个原生模型
  const displayModels = k.mappings.length > 0 ? k.mappings : k.models.slice(0, modelsExpanded ? k.models.length : 3);
  const hasMore = k.mappings.length === 0 && k.models.length > 3;

  return (
    <Card className="p-0">
      <div className="flex items-start gap-3 p-4">
        {/* 优先级上移/下移（FR-010）：越靠上越优先，故障转移先用它。
            拖拽在 Tauri WebView 里不稳，改用明确的上/下按钮，点一下与相邻 Key 交换。 */}
        <div className="mt-0.5 flex flex-col">
          <Tooltip content={t("key.moveUp")} side="left">
            <button
              className="text-text-muted hover:text-text-secondary disabled:cursor-not-allowed disabled:opacity-30"
              aria-label={t("key.moveUp")}
              disabled={isFirst}
              onClick={() => void moveKey(k.id, "up")}
            >
              <ChevronUp size={15} />
            </button>
          </Tooltip>
          <Tooltip content={t("key.moveDown")} side="left">
            <button
              className="text-text-muted hover:text-text-secondary disabled:cursor-not-allowed disabled:opacity-30"
              aria-label={t("key.moveDown")}
              disabled={isLast}
              onClick={() => void moveKey(k.id, "down")}
            >
              <ChevronDown size={15} />
            </button>
          </Tooltip>
        </div>

        <BrandIcon hint={k.vendor} fallbackLabel={k.name} iconUrl={vendorIcon} size={28} className="mt-0.5" />

        <div className="min-w-0 flex-1">
          {/* 标题行 */}
          <div className="flex items-center gap-2">
            <span className="truncate text-sm font-semibold text-text-primary">
              {k.name}
            </span>
            {isRoutingPrimary ? (
              /* 主 Key 徽标用实心渐变（UI-1）：这是卡片列表里唯一需要「一眼找到」的标识，
                 故给它全卡片最强的视觉权重。其余徽标一律保持低饱和的 /12 底色，
                 渐变只用在这一处——铺开用就等于没有重点。

                 **判据是「路由意义上的主 Key」（首个启用 Key），不是 `priority === 0`。**
                 后端路由（`enabled_keys_sorted`）与状态条、托盘都用前者。用 priority===0 会在
                 两种真实场景下与实际路由不一致：
                 ① priority-0 那条被禁用 → 徽标指着一条根本不进候选池的 Key，而真正在用的
                    那条没有任何标识；
                 ② 历史配置/cc-switch 导入曾让多条同为 0 → **多张卡片同时显示「主 Key」**，
                    一个「设为主」按钮都没有，用户无法从界面判断谁是主。
                 现在徽标恒定唯一，且与「实际先用哪条」严格一致。 */
              <Badge
                variant="primary"
                className="border-0 bg-gradient-to-r from-primary to-primary-deep text-primary-foreground"
              >
                {t("key.primary")}
              </Badge>
            ) : (
              /* 「设为主」只对**已启用**的 Key 给：把禁用 Key 设为主毫无意义
                 （它不进候选池，设完徽标还是不会亮在它身上），点了只会让人困惑。
                 与托盘「主 Key」子菜单只列启用项同一口径。 */
              k.enabled && (
                <Tooltip content={t("key.setPrimaryHint")} side="top">
                  <button
                    type="button"
                    onClick={() => void setPrimaryKey(k.id)}
                    className="shrink-0 rounded-control border border-border px-1.5 py-0.5 text-[11px] text-text-muted hover:border-primary hover:text-primary"
                  >
                    {t("key.setPrimary")}
                  </button>
                </Tooltip>
              )
            )}
            <HealthBadge health={k.health} />
          </div>

          {/* 端点与协议（UX-3：合并一行以减少卡片高度）。
              地址做成可点：中转站的额度/公告/文档都在自己的站点上，用户排查
              「这个 Key 是不是欠费了」的第一动作就是去开那个域名。原先只能选中复制、
              再切浏览器粘贴。点的是**域名根**而不是完整 baseUrl —— 后者带 `/v1`
              之类 API 路径，直接打开多半是 404 或一段 JSON 错误，帮不上忙。 */}
          <div className="mt-1 flex items-center gap-2 text-xs">
            {siteOrigin ? (
              <Tooltip content={t("key.openSiteTip", { site: siteOrigin })}>
                <button
                  type="button"
                  onClick={() => {
                    void openExternalUrl(siteOrigin).catch((e) =>
                      showToast("error", String((e as Error)?.message ?? e)),
                    );
                  }}
                  className="group flex min-w-0 items-center gap-1 truncate font-mono text-text-secondary hover:text-primary hover:underline"
                >
                  <span className="truncate">{k.baseUrl}</span>
                  <ExternalLink size={11} className="shrink-0 opacity-0 transition-opacity group-hover:opacity-100" />
                </button>
              </Tooltip>
            ) : (
              // 地址不合法（历史配置里有纯文本地址）时不给可点的假链接
              <span className="truncate font-mono text-text-secondary">{k.baseUrl}</span>
            )}
            <span className="shrink-0 text-text-muted">· {protocolLabel(k.protocol)} {t("key.protocolSuffix")}</span>
          </div>

          {/* 模型 / 映射摘要（FR-006），UX-3：超过 3 个时折叠 */}
          <div className="mt-2 flex flex-wrap items-center gap-1.5">
            {k.mappings.length > 0 ? (
              k.mappings.map((m) => (
                <Badge key={m.id} variant="info" title={t("key.mappingTitle")}>
                  {m.realName} <ArrowRight size={10} /> {m.expectedName}
                </Badge>
              ))
            ) : (
              <>
                {displayModels.map((m) => (
                  <Badge key={m.realName} variant="neutral">
                    {m.realName}
                  </Badge>
                ))}
                {hasMore && (
                  <button
                    onClick={() => setModelsExpanded(!modelsExpanded)}
                    className="text-xs text-text-muted hover:text-text-secondary"
                  >
                    {modelsExpanded ? t("key.collapse") : `+${k.models.length - 3}`}
                  </button>
                )}
              </>
            )}
          </div>

          <Tooltip
            content={k.health.lastChecked ? new Date(k.health.lastChecked).toLocaleString() : t("health.unknown")}
            side="top"
          >
            <div className="mt-2 text-[11px] text-text-muted">
              {t("key.healthCheckLabel", { time: formatRelativeTime(k.health.lastChecked, t) })}
              {k.health.latencyMs != null &&
                (k.health.status === "down" ? (
                  // 探测失败时这个延迟只是「失败前的往返耗时」，标红并注明，避免被读成探测成功。
                  <span className="text-danger">{` · ${k.health.latencyMs}ms · ${t("key.healthProbeFailed")}`}</span>
                ) : (
                  ` · ${k.health.latencyMs}ms`
                ))}
            </div>
          </Tooltip>

          {/* 余额行（第④批）。只在这条 Key 开了余额查询时出现 ——
              没开的 Key 显示一行「未配置」纯属噪音，而卡片高度是稀缺资源。 */}
          {balanceEnabled && (
            <div className="mt-1.5 border-t border-border pt-1.5 text-[11px]">
              <div className="flex items-center gap-1.5">
                <Wallet size={11} className="shrink-0 text-text-muted" />
                <span className="shrink-0 text-text-muted">{t("balance.cardLabel")}</span>
                {hasAmount ? (
                  <>
                    <span className="font-medium text-text-primary">
                      {formatBalanceAmount(balance.remaining!)} {balance.unit ?? "USD"}
                    </span>
                    {/* 总额与百分比：上游给了才显示，不硬凑分母 */}
                    {balance.total != null && (
                      <span className="truncate text-text-muted">
                        · {t("balance.ofTotal", {
                          total: formatBalanceAmount(balance.total),
                          unit: balance.unit ?? "USD",
                        })}
                        {pct != null && ` · ${t("balance.usedPct", { pct: String(pct) })}`}
                      </span>
                    )}
                    {balance.planName && (
                      <span className="truncate text-text-muted">· {balance.planName}</span>
                    )}
                  </>
                ) : (
                  <span className="text-text-muted">
                    {balanceBusy ? t("balance.probing") : "—"}
                  </span>
                )}

                {/* 查询时刻 + 手动刷新。时刻要常驻显示，陈旧才是可见的 ——
                    余额默认 10 分钟才自动重查一次，不标时间用户会把旧值当当下值。 */}
                <div className="ml-auto flex shrink-0 items-center gap-1">
                  {balance && (
                    <Tooltip content={new Date(balance.queriedAt).toLocaleString()} side="top">
                      <span className="text-text-muted">
                        {formatRelativeTime(balance.queriedAt, t)}
                      </span>
                    </Tooltip>
                  )}
                  <Tooltip content={t("balance.refresh")} side="top">
                    <button
                      type="button"
                      aria-label={t("balance.refresh")}
                      disabled={balanceBusy}
                      onClick={() => void refreshBalance(k.id, fingerprint, true)}
                      className="text-text-muted hover:text-text-secondary disabled:cursor-not-allowed disabled:opacity-40"
                    >
                      <RefreshCw size={11} className={balanceBusy ? "animate-spin" : ""} />
                    </button>
                  </Tooltip>
                </div>
              </div>

              {/* 失败原因**就地如实显示**，不只放进 tooltip：查询失败时用户最需要的就是
                  这句话（是 404 路径错、还是 401 密钥错、还是超时），藏起来等于没说。
                  长文本截断，全文进 tooltip。 */}
              {balance && !balance.ok && balance.error && (
                <Tooltip content={balance.error} side="top">
                  <div className="mt-1 truncate text-danger">{balance.error}</div>
                </Tooltip>
              )}

              {/* 上游明确说「这个号不能用了」——与查询失败分开显示（用 warning 而非 danger）：
                  两者处理方式完全不同（一个是改配置，一个是去充钱/换号）。 */}
              {balance?.isValid === false && (
                <div className="mt-1 truncate text-warning">
                  {balance.invalidMessage ?? t("balance.keyInactive")}
                </div>
              )}
            </div>
          )}
        </div>

        {/* 右侧操作区 */}
        <div className="flex flex-col items-end gap-2">
          <Switch
            checked={k.enabled}
            onCheckedChange={(v) => void toggleKey(k.id, v)}
            aria-label={t("key.enableAria")}
          />
          {confirmingDelete ? (
            <div className="flex items-center gap-1">
              <Button
                size="sm"
                variant="danger"
                title={t("common.confirmDelete")}
                onClick={() => {
                  setConfirmingDelete(false);
                  void deleteKey(k.id);
                }}
              >
                {t("common.confirmDelete")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                title={t("common.cancel")}
                onClick={() => setConfirmingDelete(false)}
              >
                {t("common.cancel")}
              </Button>
            </div>
          ) : (
            <div className="flex items-center gap-0.5">
              {/* 三个图标按钮都要 aria-label：Tooltip 只是 hover/focus 时出现的视觉层，
                  不构成无障碍名称——没有 aria-label 时屏幕阅读器只会读到一个空按钮。
                  文案与 Tooltip 内容同源，避免两处各说一套。 */}
              <Tooltip content={t("key.checkHealth")} side="top">
                <Button
                  size="icon"
                  variant="ghost"
                  aria-label={t("key.checkHealth")}
                  onClick={() => void checkHealth(k.id)}
                >
                  <RefreshCw size={14} />
                </Button>
              </Tooltip>
              <Tooltip content={t("common.edit")} side="top">
                <Button
                  size="icon"
                  variant="ghost"
                  aria-label={t("common.edit")}
                  onClick={() => onEdit(k)}
                >
                  <Pencil size={14} />
                </Button>
              </Tooltip>
              <Tooltip content={t("common.delete")} side="top">
                <Button
                  size="icon"
                  variant="ghost"
                  aria-label={t("common.delete")}
                  onClick={() => setConfirmingDelete(true)}
                >
                  <Trash2 size={14} />
                </Button>
              </Tooltip>
            </div>
          )}
        </div>
      </div>
    </Card>
  );
});
