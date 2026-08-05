/**
 * FAQ 条目。只存 i18n key，问答正文在 i18n 字典里。
 *
 * 前 8 条对应需求模板第 6.9 节要求的必答问题，后 2 条是这个产品特有的疑虑
 * （密钥安全、与手改配置的区别）—— 目标用户是开发者，这两条不答清楚转化不了。
 */

export interface FaqItem {
  id: string;
  questionKey: string;
  answerKey: string;
}

export const faqs: FaqItem[] = Array.from({ length: 10 }, (_, i) => {
  const n = i + 1;
  return { id: `q${n}`, questionKey: `faq.q${n}`, answerKey: `faq.a${n}` };
});
