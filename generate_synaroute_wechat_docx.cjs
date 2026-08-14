const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');
const zlib = require('zlib');

const outDir = path.resolve('wechat_docx_build');
const docxPath = path.resolve('SynaRoute公众号文章-大脑聚合与Key故障转移.docx');
fs.rmSync(outDir, { recursive: true, force: true });
fs.mkdirSync(path.join(outDir, '_rels'), { recursive: true });
fs.mkdirSync(path.join(outDir, 'docProps'), { recursive: true });
fs.mkdirSync(path.join(outDir, 'word', '_rels'), { recursive: true });
fs.mkdirSync(path.join(outDir, 'word', 'media'), { recursive: true });

function esc(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function run(text, opts = {}) {
  const props = [];
  if (opts.bold) props.push('<w:b/>');
  if (opts.color) props.push(`<w:color w:val="${opts.color}"/>`);
  if (opts.size) props.push(`<w:sz w:val="${opts.size}"/>`);
  if (opts.font) props.push(`<w:rFonts w:ascii="${opts.font}" w:eastAsia="${opts.font}" w:hAnsi="${opts.font}"/>`);
  if (opts.highlight) props.push(`<w:highlight w:val="${opts.highlight}"/>`);
  return `<w:r>${props.length ? `<w:rPr>${props.join('')}</w:rPr>` : ''}<w:t${/^\s|\s$/.test(text) ? ' xml:space="preserve"' : ''}>${esc(text)}</w:t></w:r>`;
}

function p(text, style = 'Body', opts = {}) {
  const spacing = opts.spacing || '<w:spacing w:before="80" w:after="140" w:line="360" w:lineRule="auto"/>';
  const jc = opts.align ? `<w:jc w:val="${opts.align}"/>` : '';
  const pStyle = style ? `<w:pStyle w:val="${style}"/>` : '';
  const border = opts.borderBottom ? '<w:pBdr><w:bottom w:val="single" w:sz="8" w:space="8" w:color="10B981"/></w:pBdr>' : '';
  return `<w:p><w:pPr>${pStyle}${spacing}${jc}${border}</w:pPr>${run(text, opts.run || {})}</w:p>`;
}

function heading(text, level = 1) {
  return p(text, `Heading${level}`, { spacing: '<w:spacing w:before="360" w:after="180"/>', run: { bold: true } });
}

function bullet(text) {
  return `<w:p><w:pPr><w:pStyle w:val="Body"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr><w:spacing w:before="40" w:after="80" w:line="340" w:lineRule="auto"/></w:pPr>${run(text)}</w:p>`;
}

function quote(text) {
  return `<w:p><w:pPr><w:pStyle w:val="QuoteBlock"/><w:spacing w:before="160" w:after="180" w:line="360" w:lineRule="auto"/><w:ind w:left="360" w:right="360"/><w:pBdr><w:left w:val="single" w:sz="18" w:space="12" w:color="10B981"/></w:pBdr><w:shd w:val="clear" w:color="auto" w:fill="ECFDF5"/></w:pPr>${run(text, { bold: true, color: '065F46', size: 28 })}</w:p>`;
}

function pageBreak() {
  return '<w:p><w:r><w:br w:type="page"/></w:r></w:p>';
}

function svgToDataUri(svg) {
  return `data:image/svg+xml;base64,${Buffer.from(svg).toString('base64')}`;
}

const bannerSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="520" viewBox="0 0 1200 520">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#0F172A"/><stop offset="0.55" stop-color="#134E4A"/><stop offset="1" stop-color="#10B981"/></linearGradient>
    <radialGradient id="r" cx="80%" cy="20%" r="70%"><stop offset="0" stop-color="#A7F3D0" stop-opacity="0.55"/><stop offset="1" stop-color="#A7F3D0" stop-opacity="0"/></radialGradient>
  </defs>
  <rect width="1200" height="520" rx="42" fill="url(#g)"/>
  <rect width="1200" height="520" rx="42" fill="url(#r)"/>
  <circle cx="965" cy="130" r="78" fill="#FFFFFF" opacity="0.12"/>
  <circle cx="1048" cy="230" r="120" fill="#FFFFFF" opacity="0.08"/>
  <text x="80" y="150" fill="#FFFFFF" font-size="70" font-family="Arial, Microsoft YaHei" font-weight="800">SynaRoute</text>
  <text x="82" y="228" fill="#D1FAE5" font-size="34" font-family="Arial, Microsoft YaHei">AI 大脑聚合 + Key 故障转移</text>
  <text x="82" y="302" fill="#FFFFFF" opacity="0.92" font-size="28" font-family="Arial, Microsoft YaHei">让多个模型并行思考，让多个 Key 组成稳定资源池</text>
  <g transform="translate(790,330)">
    <rect x="0" y="0" width="280" height="88" rx="22" fill="#FFFFFF" opacity="0.16"/>
    <circle cx="55" cy="44" r="20" fill="#34D399"/><circle cx="122" cy="44" r="20" fill="#60A5FA"/><circle cx="189" cy="44" r="20" fill="#FBBF24"/>
    <path d="M55 44 C88 10 155 10 189 44" fill="none" stroke="#FFFFFF" stroke-width="6" opacity="0.75"/>
  </g>
</svg>`;

const brainSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="1100" height="560" viewBox="0 0 1100 560">
  <rect width="1100" height="560" rx="30" fill="#F8FAFC"/>
  <text x="50" y="70" fill="#0F172A" font-size="36" font-family="Arial, Microsoft YaHei" font-weight="800">大脑聚合：多个模型并行思考，再综合决策</text>
  <g font-family="Arial, Microsoft YaHei" font-size="24" font-weight="700">
    <rect x="70" y="155" width="190" height="84" rx="22" fill="#DBEAFE" stroke="#60A5FA" stroke-width="3"/><text x="118" y="207" fill="#1D4ED8">模型 A</text>
    <rect x="70" y="300" width="190" height="84" rx="22" fill="#DCFCE7" stroke="#34D399" stroke-width="3"/><text x="118" y="352" fill="#047857">模型 B</text>
    <rect x="70" y="445" width="190" height="84" rx="22" fill="#FEF3C7" stroke="#FBBF24" stroke-width="3"/><text x="118" y="497" fill="#92400E">模型 C</text>
    <rect x="460" y="285" width="210" height="105" rx="28" fill="#ECFDF5" stroke="#10B981" stroke-width="4"/><text x="500" y="328" fill="#065F46">综合决策</text><text x="514" y="362" fill="#065F46" font-size="20">归纳 / 对比 / 筛选</text>
    <rect x="845" y="285" width="190" height="105" rx="28" fill="#0F172A"/><text x="900" y="330" fill="#FFFFFF">最终答案</text><text x="884" y="365" fill="#A7F3D0" font-size="20">更全面、更可靠</text>
  </g>
  <g stroke="#64748B" stroke-width="4" fill="none" marker-end="url(#arrow)">
    <defs><marker id="arrow" markerWidth="10" markerHeight="10" refX="8" refY="3" orient="auto"><path d="M0,0 L0,6 L9,3 z" fill="#64748B"/></marker></defs>
    <path d="M260 197 C350 197 365 315 455 330"/><path d="M260 342 C350 342 365 342 455 342"/><path d="M260 487 C350 487 365 370 455 355"/><path d="M670 338 L838 338"/>
  </g>
</svg>`;

const failoverSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="1100" height="560" viewBox="0 0 1100 560">
  <rect width="1100" height="560" rx="30" fill="#F8FAFC"/>
  <text x="50" y="70" fill="#0F172A" font-size="36" font-family="Arial, Microsoft YaHei" font-weight="800">Key 故障转移：多个 Key 组成稳定资源池</text>
  <g font-family="Arial, Microsoft YaHei" font-size="24" font-weight="700">
    <rect x="70" y="250" width="190" height="105" rx="28" fill="#E0F2FE" stroke="#38BDF8" stroke-width="3"/><text x="115" y="295" fill="#075985">AI 客户端</text><text x="113" y="330" fill="#075985" font-size="20">Claude Code / Codex</text>
    <rect x="410" y="250" width="210" height="105" rx="28" fill="#ECFDF5" stroke="#10B981" stroke-width="4"/><text x="455" y="295" fill="#065F46">SynaRoute</text><text x="455" y="330" fill="#065F46" font-size="20">健康检查 / 路由</text>
    <rect x="790" y="125" width="190" height="76" rx="20" fill="#FEE2E2" stroke="#EF4444" stroke-width="3"/><text x="840" y="174" fill="#991B1B">Key 1 失败</text>
    <rect x="790" y="255" width="190" height="76" rx="20" fill="#DCFCE7" stroke="#22C55E" stroke-width="3"/><text x="840" y="304" fill="#166534">Key 2 可用</text>
    <rect x="790" y="385" width="190" height="76" rx="20" fill="#DCFCE7" stroke="#22C55E" stroke-width="3"/><text x="840" y="434" fill="#166534">Key 3 可用</text>
  </g>
  <g stroke="#64748B" stroke-width="4" fill="none" marker-end="url(#arrow)">
    <defs><marker id="arrow" markerWidth="10" markerHeight="10" refX="8" refY="3" orient="auto"><path d="M0,0 L0,6 L9,3 z" fill="#64748B"/></marker></defs>
    <path d="M260 303 L402 303"/><path d="M620 303 C700 303 710 170 783 165"/><path d="M620 303 L783 293"/><path d="M620 303 C700 303 710 420 783 423"/>
  </g>
  <path d="M782 135 L985 205" stroke="#EF4444" stroke-width="8" opacity="0.7"/>
  <text x="425" y="455" fill="#0F766E" font-size="24" font-family="Arial, Microsoft YaHei" font-weight="700">失败时自动尝试下一条可用资源，减少手动换 Key 和任务中断</text>
</svg>`;

function svgImageParagraph(svg, name, w, h) {
  const file = path.join(outDir, 'word', 'media', name);
  fs.writeFileSync(file, svg);
  const idMap = { 'banner.svg': 'rId5', 'brain.svg': 'rId6', 'failover.svg': 'rId7' };
  const rId = idMap[name];
  const cx = Math.round(w * 9525);
  const cy = Math.round(h * 9525);
  return `<w:p><w:pPr><w:jc w:val="center"/><w:spacing w:before="160" w:after="220"/></w:pPr><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="${cx}" cy="${cy}"/><wp:docPr id="${rId.replace('rId','')}" name="${name}" descr="SynaRoute 示意图"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="0" name="${name}"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:embed="${rId}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="${cx}" cy="${cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>`;
}

const body = [];
body.push(svgImageParagraph(bannerSvg, 'banner.svg', 620, 269));
body.push(p('我做了一个 AI 大脑聚合 + Key 故障转移工具：SynaRoute', 'Title', { align: 'center', spacing: '<w:spacing w:before="120" w:after="120"/>', run: { bold: true, size: 40, color: '0F172A' } }));
body.push(p('作者：MoFamily', 'Subtitle', { align: 'center', spacing: '<w:spacing w:before="0" w:after="300"/>', run: { color: '64748B', size: 22 } }));
body.push(quote('SynaRoute 的两个核心点：让多个 AI 模型并行思考，综合出更可靠答案；让多个 API Key 形成稳定资源池，一个 Key 不可用时尽量自动切换，减少工作流中断。'));

[
'如果你经常使用 Claude Code、Codex，或者其他 AI 编程工具，应该很容易遇到两个问题。',
'第一个问题是：单个模型再强，也会有盲区。有时候它理解错需求，有时候它给出的方案看起来合理，但细节经不起推敲。你想要的不是一个模型的单一答案，而是多个模型从不同角度一起分析，再给出一个更稳妥的结论。',
'第二个问题是：AI API 的稳定性并不总是可控。某个 Key 突然不可用，某条线路突然失败，某个上游服务临时异常。正在写代码、跑任务、排查问题的时候，请求突然中断，非常影响工作流。',
'为了解决这两个问题，我做了一个桌面软件：SynaRoute。'
].forEach(t => body.push(p(t)));

body.push(heading('SynaRoute 是什么？', 1));
[
'SynaRoute 是一个运行在本地的 AI API 路由与大脑聚合工具。',
'它位于你的 AI 客户端和上游模型服务之间。Claude Code、Codex、Claude 桌面端，或者其他兼容的 AI 工具，可以通过 SynaRoute 统一访问多个模型和多个 Key。',
'你可以把它理解成：一个本地 AI API 路由器，也是一个多模型大脑聚合器。',
'它既解决“怎么稳定调用 AI”的问题，也解决“怎么让 AI 给出更可靠答案”的问题。'
].forEach(t => body.push(p(t)));
body.push(quote('官网地址：https://synaroute.mofamilys.com/'));

body.push(heading('核心能力一：AI 大脑聚合', 1));
body.push(svgImageParagraph(brainSvg, 'brain.svg', 620, 316));
[
'很多时候，我们问 AI 的问题并不简单。比如：这段代码有没有隐藏问题？这个技术方案是否合理？这个 bug 可能出在哪里？这个需求应该怎么设计？这篇文章怎么写才更有说服力？',
'如果只问一个模型，你得到的是单一路径下的答案。这个答案可能很好，也可能遗漏关键点。',
'SynaRoute 的大脑聚合能力，是让多个模型并行回答同一个问题。每个模型从自己的角度进行分析，然后再由一个综合决策模型进行归纳、对比、筛选和总结，最后输出一份更完整、更稳妥的答案。',
'这有点像一次 AI 专家组会诊。不是只听一个模型的判断，而是让多个模型同时思考，再把它们的观点聚合起来。'
].forEach(t => body.push(p(t)));
body.push(heading('大脑聚合能带来什么？', 2));
[
'减少单模型幻觉。',
'降低单模型判断偏差。',
'发现更多边界情况。',
'获得更全面的方案比较。',
'让最终答案更接近多方会诊后的结果。'
].forEach(t => body.push(bullet(t)));
body.push(p('尤其是在复杂任务上，多模型聚合会比单模型更稳。比如代码审查时，一个模型可能关注逻辑问题，另一个模型可能关注安全风险，还有一个模型可能更擅长发现边界条件。最后把这些结果综合起来，你得到的就不是单一视角，而是多视角交叉验证后的结论。'));

body.push(heading('适合大脑聚合的场景', 2));
['代码审查。','技术方案设计。','疑难问题排查。','产品需求分析。','文章和营销文案创作。','多方案对比。','复杂决策辅助。'].forEach(t => body.push(bullet(t)));

body.push(heading('核心能力二：Key 故障转移', 1));
body.push(svgImageParagraph(failoverSvg, 'failover.svg', 620, 316));
[
'AI 工具真正用起来以后，稳定性非常重要。很多人都有多个 API Key，但多个 Key 如果只是放在那里，并不能自动提升稳定性。',
'因为当某个 Key 出问题时，你仍然需要手动切换：打开配置文件、替换环境变量、重启工具、重新测试、确认模型名是否匹配，然后继续刚才的任务。',
'这一套流程很打断工作状态。SynaRoute 要解决的就是这个问题。',
'它可以集中管理多个 API Key，并对 Key 和线路进行健康检查。当某个 Key、模型或上游服务不可用时，SynaRoute 可以尽量切换到其他可用资源。',
'这样你的 AI 客户端只需要连接 SynaRoute 这一个本地入口。后面的 Key 管理、状态检测和故障切换，由 SynaRoute 统一处理。'
].forEach(t => body.push(p(t)));
body.push(heading('Key 故障转移能带来什么？', 2));
['减少请求中断。','减少手动换 Key 的频率。','减少配置文件来回修改。','让多个 Key 真正形成可用资源池。','让 Claude Code、Codex 等工具的使用体验更稳定。'].forEach(t => body.push(bullet(t)));
body.push(p('对于 AI 编程工具重度用户来说，这一点非常重要。因为一旦 AI 成为日常工作流的一部分，中断成本就会很高。尤其是正在让 AI 跑一个较长任务时，中途失败很容易浪费时间。SynaRoute 希望让这类中断尽量少发生。'));

body.push(heading('一个典型场景', 1));
[
'假设你正在用 Claude Code 写代码。你让它分析一个模块、修改几个文件、再跑测试。任务进行到一半，上游 API 请求失败了。',
'如果没有路由和故障转移，你可能要手动排查：是不是 Key 失效？是不是余额不够？是不是线路异常？是不是模型不可用？是不是客户端配置写错？然后你还要手动换 Key、改配置、重启工具。',
'但如果通过 SynaRoute 接入，客户端只连接一个本地入口。当某个 Key 或线路不可用时，SynaRoute 会尽量切换到其他可用配置，你的工作流就更不容易被打断。',
'再比如，你遇到一个复杂 bug。你不确定是协议转换问题、配置问题、状态同步问题，还是调用链某一环的问题。你可以使用大脑聚合，让多个模型从不同方向分析，再综合出更有价值的排查建议。'
].forEach(t => body.push(p(t)));
body.push(quote('SynaRoute 想解决的两个核心问题：稳定调用，可靠思考。'));

body.push(heading('除了这两个核心能力，SynaRoute 还支持什么？', 1));
[
'多 Key 管理：集中管理多个 API Key，不需要在不同客户端之间反复复制、修改和切换。',
'AI API 路由：不同客户端可以通过统一入口访问上游模型服务。',
'模型映射：把客户端请求的模型，映射到真实可用的上游模型。',
'协议转换：在客户端和上游服务之间做适配，减少手动兼容不同 API 格式的成本。',
'健康检查：更直观地知道当前 Key、模型和线路是否可用。',
'本地加密存储：API Key 属于敏感信息，SynaRoute 会在本地进行加密存储。'
].forEach(t => body.push(bullet(t)));

body.push(heading('为什么要做成桌面软件？', 1));
[
'很多 AI API 代理工具更偏命令行或服务端配置。对开发者来说，也许可以接受，但对更多普通用户来说，部署、配置、维护都不够友好。',
'SynaRoute 选择做成桌面应用，是因为我希望它更接近普通用户的使用习惯：可视化配置、本地运行、不需要部署服务器、不需要长期维护复杂脚本。',
'你只需要启动 SynaRoute，然后让你的 AI 客户端连接到它。后续的大脑聚合、Key 管理、故障转移、协议适配和健康检查，交给软件处理。'
].forEach(t => body.push(p(t)));

body.push(heading('它和普通 API 代理有什么不同？', 1));
[
'普通 API 代理主要解决的是：请求怎么转发出去。',
'SynaRoute 想进一步解决的是：多个模型怎么协同思考，多个 Key 怎么形成稳定资源池，AI 工具怎么减少中断，复杂问题怎么得到更可靠的答案。',
'所以它不是单纯的 API 转发工具。它更像是一个面向 AI 工具重度用户的本地模型调度和聚合系统。'
].forEach(t => body.push(p(t)));

body.push(heading('一句话理解 SynaRoute', 1));
body.push(quote('SynaRoute 是一个本地 AI API 路由与大脑聚合工具：统一管理多个模型和 Key，让多个 AI 并行思考，也让多个 Key 形成稳定资源池。'));

body.push(heading('适合哪些人使用？', 1));
['Claude Code 用户。','Codex 用户。','AI 编程工具重度用户。','独立开发者。','经常使用多个 API Key 的用户。','需要多个模型协同分析的人。','经常做代码审查、技术方案、问题排查的人。','需要更稳定 AI API 接入的人。'].forEach(t => body.push(bullet(t)));

body.push(heading('下载和了解更多', 1));
[
'我已经整理了官网，后续也会继续完善教程、接入说明和使用案例。',
'官网地址：https://synaroute.mofamilys.com/',
'如果你也在使用 Claude Code、Codex，或者你也想让多个 AI 模型一起帮你思考，同时减少 Key 不可用带来的中断，欢迎试试 SynaRoute。',
'也欢迎把你的使用场景、遇到的问题、希望支持的模型或客户端告诉我。',
'如果这个工具能帮你少一点折腾，多一点稳定，多一点可靠的答案，那它就值得继续做下去。'
].forEach(t => body.push(p(t)));

body.push(heading('封面摘要', 1));
body.push(p('SynaRoute 是一个本地 AI API 路由与大脑聚合工具。它支持多模型并行思考、综合决策，也支持多 Key 管理、健康检查和故障转移，适合 Claude Code、Codex 和 AI 工具重度用户。'));

const documentXml = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><w:body>${body.join('')}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1134" w:right="1134" w:bottom="1134" w:left="1134" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr></w:body></w:document>`;

const stylesXml = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Microsoft YaHei" w:eastAsia="Microsoft YaHei" w:hAnsi="Microsoft YaHei"/><w:sz w:val="24"/><w:color w:val="1F2937"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:line="360" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="Body"><w:name w:val="Body"/><w:basedOn w:val="Normal"/><w:rPr><w:rFonts w:ascii="Microsoft YaHei" w:eastAsia="Microsoft YaHei" w:hAnsi="Microsoft YaHei"/><w:sz w:val="24"/><w:color w:val="1F2937"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:rPr><w:rFonts w:ascii="Microsoft YaHei" w:eastAsia="Microsoft YaHei" w:hAnsi="Microsoft YaHei"/><w:b/><w:sz w:val="40"/><w:color w:val="0F172A"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Subtitle"><w:name w:val="Subtitle"/><w:rPr><w:rFonts w:ascii="Microsoft YaHei" w:eastAsia="Microsoft YaHei" w:hAnsi="Microsoft YaHei"/><w:sz w:val="22"/><w:color w:val="64748B"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="Heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Body"/><w:qFormat/><w:pPr><w:outlineLvl w:val="0"/><w:spacing w:before="360" w:after="180"/></w:pPr><w:rPr><w:rFonts w:ascii="Microsoft YaHei" w:eastAsia="Microsoft YaHei" w:hAnsi="Microsoft YaHei"/><w:b/><w:sz w:val="32"/><w:color w:val="065F46"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="Heading 2"/><w:basedOn w:val="Normal"/><w:next w:val="Body"/><w:qFormat/><w:pPr><w:outlineLvl w:val="1"/><w:spacing w:before="260" w:after="120"/></w:pPr><w:rPr><w:rFonts w:ascii="Microsoft YaHei" w:eastAsia="Microsoft YaHei" w:hAnsi="Microsoft YaHei"/><w:b/><w:sz w:val="28"/><w:color w:val="0F766E"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="QuoteBlock"><w:name w:val="QuoteBlock"/><w:basedOn w:val="Normal"/><w:rPr><w:rFonts w:ascii="Microsoft YaHei" w:eastAsia="Microsoft YaHei" w:hAnsi="Microsoft YaHei"/><w:sz w:val="28"/><w:color w:val="065F46"/><w:b/></w:rPr></w:style></w:styles>`;

const numberingXml = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="520" w:hanging="260"/></w:pPr><w:rPr><w:rFonts w:ascii="Symbol" w:hAnsi="Symbol" w:hint="default"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>`;

const rels = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>`;
const docRels = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/banner.svg"/><Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/brain.svg"/><Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/failover.svg"/></Relationships>`;
const contentTypes = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="svg" ContentType="image/svg+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/></Types>`;
const core = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>SynaRoute公众号文章-大脑聚合与Key故障转移</dc:title><dc:creator>MoFamily</dc:creator><cp:lastModifiedBy>Claude</cp:lastModifiedBy></cp:coreProperties>`;
const app = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>Claude Code</Application></Properties>`;

fs.writeFileSync(path.join(outDir, '[Content_Types].xml'), contentTypes);
fs.writeFileSync(path.join(outDir, '_rels', '.rels'), rels);
fs.writeFileSync(path.join(outDir, 'word', 'document.xml'), documentXml);
fs.writeFileSync(path.join(outDir, 'word', 'styles.xml'), stylesXml);
fs.writeFileSync(path.join(outDir, 'word', 'numbering.xml'), numberingXml);
fs.writeFileSync(path.join(outDir, 'word', '_rels', 'document.xml.rels'), docRels);
fs.writeFileSync(path.join(outDir, 'docProps', 'core.xml'), core);
fs.writeFileSync(path.join(outDir, 'docProps', 'app.xml'), app);

fs.rmSync(docxPath, { force: true });
const zipPath = docxPath.replace(/\.docx$/i, '.zip');
fs.rmSync(zipPath, { force: true });
const ps = `Compress-Archive -Path '${outDir.replace(/'/g, "''")}\\*' -DestinationPath '${zipPath.replace(/'/g, "''")}' -Force`;
execFileSync('powershell.exe', ['-NoProfile', '-Command', ps], { stdio: 'inherit' });
fs.renameSync(zipPath, docxPath);
console.log(docxPath);
