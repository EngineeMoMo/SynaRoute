import { Terminal, AppWindow, Bot, type LucideIcon } from "lucide-react";
import type { CategoryType } from "@/types";

/**
 * 三个客户端分类的图标 —— **唯一事实来源**。
 *
 * # 为什么必须收成一处
 *
 * 这份映射此前散在**四个地方**、而且不一致：`Sidebar`、`CommandPalette`、
 * `OnboardingWizard` 各抄了一份 `{ Terminal, MonitorSmartphone, Code2 }`，
 * 而分类**页头**（`CategoryPage` 的 `PageHeader`）压根没接任何一份 —— 它三页
 * 统一写死 `Waypoints`（SynaRoute 自己的 logo 字形）。于是用户在 Claude CLI 页
 * 和 Codex 页看到的页头图标**是同一个方块**，「换了分类图标没变」正是这么来的。
 *
 * 收成一处之后，改一个分类的图标四个入口一起变，页头也不会再漏。
 *
 * # 为什么是这三个字形
 *
 * - `claude-cli` → `Terminal`：它就是跑在终端里的 CLI，最直白。
 * - `claude-desktop` → `AppWindow`：原来的 `MonitorSmartphone`（显示器+手机）
 *   会让人以为是「多端/响应式」，而它其实是**桌面 GUI 应用**。窗口字形更贴切。
 * - `codex` → `Bot`：原来的 `Code2`（代码括号）太泛，且与 CLI 的终端语义撞车
 *   （两者都「在写代码」）。Codex 的辨识点是**自主编码 agent**，用机器人字形
 *   把它和另外两个 Claude 分类区分开。
 *
 * ⚠️ 三个字形必须**两两可辨**：终端、窗口、机器人在 16px 下轮廓差异明显，
 * 不像 `Terminal` 与 `SquareChevronRight` 那样挤在一起分不清。
 */
export const CATEGORY_ICONS: Record<CategoryType, LucideIcon> = {
  "claude-cli": Terminal,
  "claude-desktop": AppWindow,
  codex: Bot,
};
