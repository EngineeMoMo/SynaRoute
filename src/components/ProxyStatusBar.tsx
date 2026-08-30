import * as React from "react";
import { Button } from "@/components/ui/Button";
import { StatusBarPicker } from "@/components/ui/StatusBarPicker";
import { Tooltip } from "@/components/ui/Tooltip";
import type { ProxyState } from "@/types";
import { api } from "@/lib/bridge";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { discoverableModels, routingPrimaryKey } from "@/lib/modelSets";
import { Play, Square, Copy, Waypoints, ArrowRight, AlertTriangle, LogOut } from "lucide-react";
import { ToolConfigPreviewButton } from "@/components/ToolConfigPreview";

/** 顶部代理状态条：显示端点、运行状态，提供启停（FR-008/FR-019）+ 主 Key / Codex 模型快切（UX#6/#8） */
export function ProxyStatusBar({ proxy }: { proxy: ProxyState | null }) {
  // 细粒度订阅：整店解构（`useStore()`）会订阅**所有**字段，而 LogsPage 每 2s 就
  // `set({ events })` 一次 —— 那会让本组件（以及所有整店解构的组件）每 2s 全量重渲染，
  // 哪怕用户根本不在日志页。actions 是稳定引用，单独取不会引入额外渲染。
  const startProxy = useStore((s) => s.startProxy);
  const stopProxy = useStore((s) => s.stopProxy);
  const switchToOfficial = useStore((s) => s.switchToOfficial);
  const keys = useStore((s) => s.keys);
  const activeCategory = useStore((s) => s.activeCategory);
  const setPrimaryKey = useStore((s) => s.setPrimaryKey);
  const setActiveModel = useStore((s) => s.setActiveModel);
  const setActiveEffort = useStore((s) => s.setActiveEffort);
  const showToast = useStore((s) => s.showToast);
  // 这两个刻意取到**基本类型**而不是订阅整个 settings 对象：`persistOneSetting` 每次落盘都
  // `setState({ settings: next })` 换一个新对象引用，订阅整个对象会让状态条跟着主题、语言、
  // 端口、日志目录的任何一次改动一起重渲染。
  const activeModel = useStore((s) => s.settings?.activeModels?.[s.activeCategory] ?? "");
  const activeEffort = useStore((s) => s.settings?.activeEfforts?.[s.activeCategory] ?? "");
  const t = useT();

  /**
   * Codex 自己 `config.toml` 里那个模型（只读回显）。
   *
   * 为什么要它：上面那个下拉是**兜底**（`activeModels`），而 Codex 会把用户在 `/model` 里的
   * 选择写回 config.toml，且 `model_choice::pick` 对 Codex **优先尊重客户端发来的可服务名字**。
   * 于是「在这里选了 b、在 Codex 里改成 a」之后下拉一直显示 b 而每条请求走的是 a ——
   * 界面用权威口吻说了一个不生效的值。把真实值并列摆出来，用户一眼看出谁在生效。
   *
   * 只在 Codex 分类拉；`null` = 没接入 / 不是我们接入的 config，此时那一格不渲染。
   */
  const [codexModel, setCodexModel] = React.useState<string | null>(null);
  const refreshCodexModel = React.useCallback(() => {
    if (activeCategory !== "codex") {
      setCodexModel(null);
      return;
    }
    // 失败即当「读不到」：这是纯展示，弹错会为一格回显打扰用户。
    void api.getCodexConfigModel().then(setCodexModel).catch(() => setCodexModel(null));
  }, [activeCategory]);
  React.useEffect(refreshCodexModel, [refreshCodexModel, proxy?.status]);

  const enabledKeys = keys.filter((k) => k.enabled);
  const enabledCount = enabledKeys.length;
  const running = proxy?.status === "running";
  const endpoint = proxy?.port ? `http://127.0.0.1:${proxy.port}` : "—";
  const isCodex = activeCategory === "codex";

  // 当前主 Key：**首个启用 Key**（非 priority===0），与后端路由和托盘同口径，详见 routingPrimaryKey。
  const primary = routingPrimaryKey(keys);
  // 候选模型集与分类页、与后端 /v1/models 同口径（共用 modelSets.ts，避免两份实现漂移）。
  const discoverable = isCodex ? discoverableModels(enabledKeys) : [];

  // 健康度聚合：此前状态条只看 enabled 数，全部 Key 探测为 down 时仍显示绿点「运行中/N」,
  // 用户必须点进列表才知道一个都用不了。数据本来就在 keys[].health.status，只是没被用。
  //
  // 口径说明：只把**明确 down** 计为不可用。unknown（未探测过）/checking 不算——
  // 探测与路由是刻意分离的（见 health::is_candidate：真实流量才是可用性裁判），
  // 把未探测算成故障会造成刚加完 Key 就报红的误导。
  const downCount = enabledKeys.filter((k) => k.health.status === "down").length;
  const allDown = enabledCount > 0 && downCount === enabledCount;
  const someDown = downCount > 0 && !allDown;

  const routeModeLabel =
    enabledCount <= 1 ? t("proxy.directMode") : t("proxy.failoverMode", { n: enabledCount });

  const copyEndpoint = () => {
    if (proxy?.port) void navigator.clipboard?.writeText(endpoint);
  };

  // 成功 toast 一律靠 action 的返回值来发：这三个 action 内部失败时**自己已经**弹了 error，
  // 这里再无条件弹 success 就会出现「失败了却提示已切换」（toast 是单值，后弹的顶掉先弹的）。
  const onPickPrimary = (id: string) => {
    const name = keys.find((k) => k.id === id)?.name ?? id;
    void setPrimaryKey(id).then((ok) => {
      if (ok) showToast("success", t("proxy.primaryKeySwitched", { name }));
    });
  };
  const onPickModel = (m: string) => {
    void setActiveModel(activeCategory, m).then((ok) => {
      if (!ok) return;
      // 切完重读一次 config.toml：`select_model` 会把它一起写进去，不重读的话旁边那格
      // 「Codex 当前」会停在旧值 —— 那正是这一格要消除的现象。
      void refreshCodexModel();
      showToast(
        "success",
        m ? t("proxy.modelSwitched", { name: m }) : t("proxy.modelSwitchedAuto"),
      );
    });
  };
  const onPickEffort = (e: string) => {
    void setActiveEffort(activeCategory, e).then((ok) => {
      if (!ok) return;
      showToast(
        "success",
        e ? t("proxy.effortSwitched", { name: t(`category.effort.${e}`) }) : t("proxy.effortSwitchedOff"),
      );
    });
  };

  return (
    // flex-wrap：加了三个下拉后窄窗口必然放不下（应用最小宽度 900），不许它们被挤压变形。
    <div className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-border bg-surface px-6 py-3">
      <div className="flex items-center gap-2">
        <span
          className={`inline-block h-2 w-2 rounded-full ${
            allDown ? "bg-danger" : running ? "bg-success" : "bg-text-muted"
          }`}
        />
        <span className="text-sm font-medium text-text-primary">
          {running ? t("proxy.running") : t("proxy.stopped")}
        </span>
      </div>

      {/* 健康度告警：全部/部分 Key 明确不可用时，在状态条上直接可见，而不是只有点进列表才能发现。
          提示补的是**文字本身答不了的那个问题**：「不可用」是探测结论，而路由的裁判是真实流量，
          所以它不等于「一定转发不了」——没有这句，用户会以为代理已经彻底废了。 */}
      {allDown && (
        <Tooltip content={t("proxy.allKeysDownTip")} side="bottom">
          <span className="flex items-center gap-1 rounded-control bg-danger/10 px-1.5 py-1 text-xs font-medium text-danger">
            <AlertTriangle size={13} />
            {t("proxy.allKeysDown", { n: enabledCount })}
          </span>
        </Tooltip>
      )}
      {someDown && (
        <Tooltip content={t("proxy.someKeysDownTip")} side="bottom">
          <span className="flex items-center gap-1 rounded-control bg-warning/10 px-1.5 py-1 text-xs font-medium text-warning">
            <AlertTriangle size={13} />
            {t("proxy.someKeysDown", { down: downCount, total: enabledCount })}
          </span>
        </Tooltip>
      )}

      {/* 路由模式徽标。这是整条状态条上最晦涩的元素——只有一个图标加一个光秃秃的数字，
          此前全靠原生 title 解释（1s 延迟、无样式）。数字含义、以及「直连/故障转移」
          两种模式的差别都得说清楚。 */}
      <Tooltip content={t("proxy.routeModeTip")} side="bottom">
        <span
          className={`flex items-center gap-1 rounded-control px-1.5 py-1 ${
            enabledCount > 1 ? "text-primary" : "text-text-muted"
          }`}
          aria-label={routeModeLabel}
        >
          {enabledCount > 1 ? <Waypoints size={15} /> : <ArrowRight size={15} />}
          <span className="text-xs font-medium tabular-nums">{enabledCount}</span>
        </span>
      </Tooltip>

      <div className="flex items-center gap-1.5 font-mono text-xs text-text-secondary">
        {/* 端点自身也值得一条：用户常问「这个地址要填到哪」 */}
        <Tooltip content={t("proxy.endpointTip")} side="bottom">
          <span>{endpoint}</span>
        </Tooltip>
        {proxy?.port && (
          <Tooltip content={t("proxy.copyEndpoint")} side="bottom">
            <button
              onClick={copyEndpoint}
              aria-label={t("proxy.copyEndpoint")}
              className="rounded p-1 hover:bg-surface-hover"
            >
              <Copy size={12} />
            </button>
          </Tooltip>
        )}
      </div>

      {/* 主 Key 快切（UX#6）：此前只有托盘有，主窗口要进列表找卡片才能改。
          **必须走 store.setPrimaryKey → 后端 Store::set_primary_key**：重排规则收归后端是
          修过的历史问题——前端自己算优先级会与托盘漂移，表现为「从托盘设的主」和
          「从界面设的主」结果不一致。 */}
      <div className="h-4 w-px bg-border" />
      <StatusBarPicker
        label={t("proxy.primaryKey")}
        title={t("proxy.primaryKeyHint")}
        value={primary?.id ?? ""}
        placeholder={t("proxy.primaryKeyNone")}
        emptyHint={t("proxy.primaryKeyNone")}
        options={enabledKeys
          .slice()
          .sort((a, b) => a.priority - b.priority)
          .map((k) => ({
            value: k.id,
            label: k.name,
            hint: k.health.status === "down" ? t("proxy.someKeysDownHint") : undefined,
          }))}
        onChange={onPickPrimary}
      />

      {/* Codex 的「当前模型」「推理强度」（UX#8）：原先埋在分类页中部卡片里，
          与它们影响的东西（转发行为）离得很远。提到状态条后，分类页那张卡片已整块删除——
          同一个状态留两处可改，迟早出现「在这边改了、那边还显示旧值」。 */}
      {isCodex && (
        <>
          <div className="h-4 w-px bg-border" />
          <StatusBarPicker
            label={t("category.activeModel")}
            title={t("category.activeModelHint")}
            value={activeModel}
            placeholder={t("category.activeModelAuto")}
            emptyHint={t("category.activeModelEmpty")}
            options={[
              { value: "", label: t("category.activeModelAuto") },
              ...discoverable.map((m) => ({ value: m, label: m })),
            ]}
            onChange={onPickModel}
          />
          {/* Codex 自己那个（只读）。刻意只在**与兜底不同**时才出现：相同时多一格纯噪音，
              而不同时它就是「谁在生效」的答案 —— pick 对 Codex 优先尊重客户端发来的名字。 */}
          {codexModel && codexModel !== activeModel && (
            <Tooltip content={t("category.activeModelHint")} side="bottom">
              <div className="flex shrink-0 items-center gap-1 text-[11px] text-text-muted">
                <span>{t("category.codexCurrentModel")}</span>
                <span className="font-mono text-text-secondary">{codexModel}</span>
              </div>
            </Tooltip>
          )}
          <StatusBarPicker
            label={t("category.effortTitle")}
            title={t("category.effortHint")}
            value={activeEffort}
            placeholder={t("category.effort.off")}
            emptyHint={t("category.effort.off")}
            options={[
              { value: "", label: t("category.effort.off") },
              { value: "low", label: t("category.effort.low") },
              { value: "medium", label: t("category.effort.medium") },
              { value: "high", label: t("category.effort.high") },
              { value: "xhigh", label: t("category.effort.xhigh") },
            ]}
            onChange={onPickEffort}
          />
        </>
      )}

      <div className="ml-auto flex items-center gap-2">
        <ToolConfigPreviewButton />
        {running ? (
          <>
            {/* 切官方：停止代理并还原配置，但保留 MCP 注册（大脑聚合继续可用）。
                放在「停止」左侧，因为它比完全停止更温和（只断代理，不断工具）。
                「切官方」与「停止」的差别只有这条提示说得清，而选错的后果实打实
                （以为只是断代理、结果把大脑聚合也停了）。 */}
            <Tooltip content={t("proxy.switchToOfficialHint")} side="bottom">
              <Button
                size="sm"
                variant="ghost"
                onClick={() => void switchToOfficial()}
                className="text-text-muted hover:text-text-primary"
              >
                <LogOut size={14} /> {t("proxy.switchToOfficial")}
              </Button>
            </Tooltip>
            <Tooltip content={t("proxy.stopTip")} side="bottom">
              <Button size="sm" variant="outline" onClick={() => void stopProxy()}>
                <Square size={14} /> {t("proxy.stop")}
              </Button>
            </Tooltip>
          </>
        ) : (
          // 启动是本页的主操作（UI-1）：给它渐变以拉开与「停止 / 切官方」的层级差。
          // 停止按钮维持 outline 中性样式——破坏性/退出类操作不该比启动更抢眼。
          <Button
            size="sm"
            onClick={() => void startProxy()}
            className="bg-gradient-to-r from-primary to-primary-deep"
          >
            <Play size={14} /> {t("proxy.start")}
          </Button>
        )}
      </div>
    </div>
  );
}
