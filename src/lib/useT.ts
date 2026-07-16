// 组件内取词 Hook：从全局 store 读当前语言，返回绑定语言的 t()。
// 语言切换时 store 更新 -> 订阅组件重渲染 -> 文案即时切换，无需刷新。
import { useStore } from "@/store";
import { translate, type TFunc } from "@/lib/i18n";

export function useT(): TFunc {
  const lang = useStore((s) => s.lang);
  return (key, vars) => translate(lang, key, vars);
}
