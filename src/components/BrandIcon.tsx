import { useMemo, useState } from "react";
import { Search, X } from "lucide-react";
import { useT } from "@/lib/useT";
import { useStore } from "@/store";
import {
  BRANDS,
  getBrand,
  isNearBlack,
  isPresetBrand,
  resolveBrand,
  type Brand,
  type BrandGroup,
} from "@/components/brandIcons";

export { isPresetBrand } from "@/components/brandIcons";

/**
 * 品牌图标：按 vendor id / 名称 / 模型名匹配内置 logo，未知厂商回退首字母色块。
 *
 * 注册表（键、显示名、主色、path、匹配词）在 `brandIcons.ts`；本文件只管**画**。
 * 19 个品牌的 path 来自 simple-icons（CC0，见 `brandIcons.generated.ts`），
 * 其余走首字母兜底 —— 刻意不画「看起来像但其实不是」的假 logo。
 */

interface BrandIconProps {
  /** vendor id、厂商名或模型名，任一即可用于推断品牌 */
  hint?: string;
  /** 无法推断品牌时，用于生成首字母占位的显示名 */
  fallbackLabel?: string;
  /**
   * 用户**显式指定**的图标，两种形态（与 cc-switch 的 `icon` 列同一模型）：
   * - **预设键**（`"anthropic"`、`"zhipu"`…）→ 渲染内置品牌 logo；
   * - **data-URL**（`data:image/png;base64,…`）→ 渲染上传的自定义图片。
   *
   * 两者都**优先于**启发式匹配 —— 显式选择必须胜过程序的猜测，否则用户会发现「选了没用」。
   */
  iconUrl?: string;
  size?: number;
  className?: string;
}

export function BrandIcon({ hint, fallbackLabel, iconUrl, size = 18, className }: BrandIconProps) {
  // 深色模式下近黑品牌色要提亮，否则等于没有图标（详见 brandIcons.ts 的 isNearBlack）。
  const theme = useStore((s) => s.theme);
  const dark = useIsDark(theme);

  // 显式预设键最高优先：用户挑了就按他挑的画，不再让启发式插手。
  const explicit = isPresetBrand(iconUrl) ? getBrand(iconUrl) : undefined;
  const brand = useMemo(
    () => explicit ?? resolveBrand(hint) ?? resolveBrand(fallbackLabel),
    [explicit, hint, fallbackLabel],
  );

  // 自定义上传图标（data-URL）：仅当它**不是**预设键时才走 <img>。
  // 预设键不是 URL，塞进 src 会被浏览器当相对路径去请求、渲染成碎图标。
  if (iconUrl && !explicit) {
    return (
      <span
        className={`inline-flex shrink-0 items-center justify-center overflow-hidden rounded-full bg-surface-hover ${className ?? ""}`}
        style={{ width: size, height: size }}
        title={hint ?? fallbackLabel}
      >
        <img src={iconUrl} alt="" width={size} height={size} className="h-full w-full object-cover" />
      </span>
    );
  }

  if (brand?.path) {
    const color = dark && isNearBlack(brand.color) ? "#E4E4E7" : brand.color;
    return (
      <span
        className={`inline-flex shrink-0 items-center justify-center rounded-full ${className ?? ""}`}
        style={{
          width: size,
          height: size,
          color,
          // 底色用品牌色的 10% alpha。近黑品牌在深色下已经提亮成浅灰，
          // 再叠一层浅底会糊成一片 —— 那时改用中性底。
          background: dark && isNearBlack(brand.color) ? "rgb(255 255 255 / 0.08)" : `${brand.color}1a`,
        }}
        title={brand.label}
      >
        <svg width={size * 0.62} height={size * 0.62} viewBox="0 0 24 24" aria-hidden="true">
          <path fill="currentColor" d={brand.path} />
        </svg>
      </span>
    );
  }

  // 有品牌但没有 logo（simple-icons 未收录）→ 用**品牌色**的首字母块，
  // 而不是随机哈希色：至少颜色是对的，视觉上仍能把同一家认出来。
  const label = brand?.label ?? fallbackLabel ?? hint ?? "?";
  const bg = brand ? brand.color : fallbackColor(label);
  return (
    <span
      className={`inline-flex shrink-0 items-center justify-center rounded-full font-semibold text-white ${className ?? ""}`}
      style={{
        width: size,
        height: size,
        // 近黑品牌在深色模式下提亮一档，否则字母块与背景糊在一起
        background: dark && isNearBlack(bg) ? "#3F3F46" : bg,
        fontSize: size * 0.5,
      }}
      title={label}
    >
      {initial(label)}
    </span>
  );
}

/** 当前是否深色（`theme` 可能是 `system`，那就问系统）。 */
function useIsDark(theme: string): boolean {
  return useMemo(() => {
    if (theme === "dark") return true;
    if (theme === "light") return false;
    return (
      typeof window !== "undefined" &&
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-color-scheme: dark)").matches
    );
  }, [theme]);
}

/** 名称首字母（英文取首字母，中文取首字） */
function initial(label: string): string {
  const trimmed = label.trim();
  if (!trimmed) return "?";
  return trimmed[0].toUpperCase();
}

/** 稳定地为未知品牌挑一个占位底色 */
const FALLBACK_COLORS = ["#6366F1", "#0EA5E9", "#10B981", "#F59E0B", "#EF4444", "#8B5CF6", "#EC4899"];
function fallbackColor(label: string): string {
  let h = 0;
  for (let i = 0; i < label.length; i++) h = (h * 31 + label.charCodeAt(i)) >>> 0;
  return FALLBACK_COLORS[h % FALLBACK_COLORS.length];
}

// ---------------------------------------------------------------------------
// 挑选器
// ---------------------------------------------------------------------------

const GROUP_ORDER: BrandGroup[] = ["official", "china", "gateway", "local"];

/**
 * 内置品牌的**挑选器**（厂商管理页与 Key 编辑器共用一份实现）。
 *
 * 抽出来共用的理由不是「省几行」：两处各写一遍必然漂移 —— 一边加了新品牌、另一边没加，
 * 或者「再点一次取消选择」这条交互只有一边有。而这两个入口对用户是同一件事
 * （「告诉程序这个站是哪家的」），行为不一致比少一个入口更让人困惑。
 *
 * # 为什么从「一排色块」改成「搜索 + 分组」
 *
 * 上一版是把全部预设平铺成一排 9×9 方块。8 个时还能扫一眼找到，
 * 现在是 32 个，平铺后在一个约 360px 宽的表单里要折成四五行、且只能靠 hover
 * 看 title 才知道哪个是哪个 —— 等于逼用户逐个悬停。
 *
 * 现在：一个搜索框 + 按「国际原厂 / 国内原厂 / 聚合中转 / 本机」分四段，
 * 每项**带文字标签**（不再只有图标）。搜索同时匹配中英文名与关键词，
 * 所以输 `glm`、`智谱`、`bigmodel` 都能找到智谱。
 */
export function BrandPresetPicker({
  value,
  onChange,
  disabled,
}: {
  value?: string;
  onChange: (next: string | undefined) => void;
  disabled?: boolean;
}) {
  const t = useT();
  const [q, setQ] = useState("");

  const groups = useMemo(() => {
    const needle = q.trim().toLowerCase();
    const match = (b: Brand) =>
      !needle ||
      b.key.includes(needle) ||
      b.label.toLowerCase().includes(needle) ||
      b.labelEn.toLowerCase().includes(needle) ||
      b.keywords.some((k) => k.includes(needle));
    return GROUP_ORDER.map((g) => ({
      group: g,
      items: BRANDS.filter((b) => b.group === g && match(b)),
    })).filter((x) => x.items.length > 0);
  }, [q]);

  return (
    <div className="space-y-2">
      <div className="relative">
        <Search
          size={13}
          className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-text-muted"
          aria-hidden="true"
        />
        <input
          type="text"
          value={q}
          disabled={disabled}
          onChange={(e) => setQ(e.target.value)}
          placeholder={t("brandPicker.search")}
          aria-label={t("brandPicker.search")}
          className="w-full rounded-control border border-border bg-surface py-1.5 pl-7 pr-7 text-xs text-text-primary placeholder:text-text-muted focus:border-primary focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
        />
        {q && (
          <button
            type="button"
            onClick={() => setQ("")}
            aria-label={t("common.clear")}
            className="absolute right-1.5 top-1/2 flex h-5 w-5 -translate-y-1/2 items-center justify-center rounded text-text-muted hover:text-text-primary"
          >
            <X size={12} aria-hidden="true" />
          </button>
        )}
      </div>

      {/* 限高 + 自己滚：这一块嵌在表单里，32 个品牌全展开会把保存按钮顶到屏幕外 */}
      <div className="max-h-56 space-y-2.5 overflow-y-auto pr-1">
        {groups.map(({ group, items }) => (
          <div key={group}>
            <div className="mb-1 text-[10px] font-medium uppercase tracking-wide text-text-muted">
              {t(`brandPicker.group.${group}`)}
            </div>
            <div className="flex flex-wrap gap-1">
              {items.map((b) => {
                const active = value === b.key;
                return (
                  <button
                    key={b.key}
                    type="button"
                    title={b.label}
                    aria-pressed={active}
                    disabled={disabled}
                    // 点已选中的 = 取消选择，回到自动匹配
                    onClick={() => onChange(active ? undefined : b.key)}
                    className={`inline-flex min-h-7 items-center gap-1.5 rounded-control border px-1.5 py-1 text-[11px] transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${
                      active
                        ? "border-primary bg-primary/12 text-text-primary"
                        : "border-border text-text-secondary hover:border-primary/50 hover:bg-surface-hover"
                    }`}
                  >
                    <BrandIcon iconUrl={b.key} fallbackLabel={b.label} size={16} />
                    <span className="max-w-[9rem] truncate">{b.label}</span>
                  </button>
                );
              })}
            </div>
          </div>
        ))}
        {groups.length === 0 && (
          <p className="py-3 text-center text-[11px] text-text-muted">
            {t("brandPicker.noMatch", { q })}
          </p>
        )}
      </div>
    </div>
  );
}
