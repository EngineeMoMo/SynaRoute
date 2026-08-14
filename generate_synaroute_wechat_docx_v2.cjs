const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

const root = path.resolve('wechat_docx_build_v2');
const output = path.resolve('SynaRoute公众号文章-官网风格重制版.docx');
fs.rmSync(root, { recursive: true, force: true });
for (const dir of ['_rels', 'docProps', 'word/_rels', 'word/media']) fs.mkdirSync(path.join(root, dir), { recursive: true });

const esc = (value) => String(value).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
const run = (text, o = {}) => {
  const rp = [];
  if (o.bold) rp.push('<w:b/>');
  if (o.italic) rp.push('<w:i/>');
  if (o.color) rp.push(`<w:color w:val="${o.color}"/>`);
  if (o.size) rp.push(`<w:sz w:val="${o.size}"/>`);
  if (o.font) rp.push(`<w:rFonts w:ascii="${o.font}" w:eastAsia="${o.font}" w:hAnsi="${o.font}"/>`);
  return `<w:r>${rp.length ? `<w:rPr>${rp.join('')}</w:rPr>` : ''}<w:t${/^\s|\s$/.test(text) ? ' xml:space="preserve"' : ''}>${esc(text)}</w:t></w:r>`;
};

function para(runs, style = 'Body', o = {}) {
  const spacing = o.spacing || '<w:spacing w:before="70" w:after="130" w:line="360" w:lineRule="auto"/>';
  const align = o.align ? `<w:jc w:val="${o.align}"/>` : '';
  const border = o.border ? `<w:pBdr><w:bottom w:val="single" w:sz="${o.border.size || 6}" w:space="8" w:color="${o.border.color || 'E4E4E7'}"/></w:pBdr>` : '';
  const indent = o.indent ? `<w:ind w:left="${o.indent}" w:right="${o.indent}"/>` : '';
  return `<w:p><w:pPr>${style ? `<w:pStyle w:val="${style}"/>` : ''}${spacing}${align}${border}${indent}</w:pPr>${Array.isArray(runs) ? runs.join('') : run(runs, o.run || {})}</w:p>`;
}
function text(text, o = {}) { return para(run(text, o), o.style || 'Body', o); }
function heading(textValue, level = 1) { return para(run(textValue, { bold: true }), `Heading${level}`, { spacing: level === 1 ? '<w:spacing w:before="330" w:after="150"/>' : '<w:spacing w:before="220" w:after="100"/>' }); }
function bullet(value) { return `<w:p><w:pPr><w:pStyle w:val="Body"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr><w:spacing w:before="30" w:after="60" w:line="340" w:lineRule="auto"/></w:pPr>${run(value)}</w:p>`; }
function callout(value, tone = 'purple') {
  const fill = tone === 'green' ? 'ECFDF5' : tone === 'gray' ? 'F4F4F5' : 'F5F3FF';
  const line = tone === 'green' ? '22C55E' : tone === 'gray' ? 'A1A1AA' : '6D5EF7';
  const color = tone === 'green' ? '166534' : tone === 'gray' ? '3F3F46' : '4338CA';
  return `<w:p><w:pPr><w:pStyle w:val="Callout"/><w:spacing w:before="160" w:after="170" w:line="380" w:lineRule="auto"/><w:ind w:left="280" w:right="280"/><w:pBdr><w:left w:val="single" w:sz="20" w:space="12" w:color="${line}"/></w:pBdr><w:shd w:val="clear" w:color="auto" w:fill="${fill}"/></w:pPr>${run(value, { bold: true, size: 27, color })}</w:p>`;
}
function imageParagraph(svg, name, width, height) {
  fs.writeFileSync(path.join(root, 'word/media', name), svg);
  const ids = { 'hero.svg': 'rId5', 'route.svg': 'rId6', 'failover.svg': 'rId7', 'brain.svg': 'rId8' };
  const rid = ids[name];
  const cx = Math.round(width * 9525);
  const cy = Math.round(height * 9525);
  return `<w:p><w:pPr><w:jc w:val="center"/><w:spacing w:before="100" w:after="170"/></w:pPr><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="${cx}" cy="${cy}"/><wp:docPr id="${rid.slice(3)}" name="${name}" descr="SynaRoute product illustration"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="0" name="${name}"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:embed="${rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="${cx}" cy="${cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>`;
}
function card(title, body, accent = '6D5EF7') {
  return `<w:tc><w:tcPr><w:tcW w:w="4850" w:type="dxa"/><w:shd w:val="clear" w:color="auto" w:fill="FFFFFF"/><w:tcMar><w:top w:w="180" w:type="dxa"/><w:start w:w="220" w:type="dxa"/><w:bottom w:w="180" w:type="dxa"/><w:end w:w="220" w:type="dxa"/></w:tcMar><w:tcBorders><w:top w:val="single" w:sz="7" w:color="E4E4E7"/><w:left w:val="single" w:sz="7" w:color="E4E4E7"/><w:bottom w:val="single" w:sz="7" w:color="E4E4E7"/><w:right w:val="single" w:sz="7" w:color="E4E4E7"/></w:tcBorders></w:tcPr><w:p><w:pPr><w:spacing w:after="90"/></w:pPr>${run(title, { bold: true, size: 27, color: accent })}</w:p>${para(run(body, { size: 22, color: '52525B' }), 'Body', { spacing: '<w:spacing w:before="0" w:after="0" w:line="330" w:lineRule="auto"/>' })}</w:tc>`;
}
function twoCards(a, b) {
  return `<w:tbl><w:tblPr><w:tblW w:w="9700" w:type="dxa"/><w:tblLayout w:type="fixed"/><w:tblBorders><w:top w:val="nil"/><w:left w:val="nil"/><w:bottom w:val="nil"/><w:right w:val="nil"/><w:insideH w:val="nil"/><w:insideV w:val="nil"/></w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="4850"/><w:gridCol w:w="4850"/></w:tblGrid><w:tr>${a}${b}</w:tr></w:tbl><w:p><w:pPr><w:spacing w:after="90"/></w:pPr></w:p>`;
}

const hero = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="430" viewBox="0 0 1200 430"><rect width="1200" height="430" rx="28" fill="#FAFAFA"/><path d="M0 350 C210 270 340 390 545 308 C720 238 835 295 1200 120 L1200 430 L0 430Z" fill="#F5F3FF"/><g transform="translate(78 85)"><rect width="106" height="106" rx="28" fill="#6D5EF7"/><circle cx="34" cy="34" r="15" fill="#FFF"/><circle cx="53" cy="53" r="25" fill="#FFF"/><circle cx="75" cy="77" r="15" fill="#FFF"/><path d="M45 45 L70 70" stroke="#6D5EF7" stroke-width="8"/></g><text x="220" y="125" fill="#18181B" font-size="68" font-family="Segoe UI,Microsoft YaHei" font-weight="700">SynaRoute</text><text x="222" y="180" fill="#71717A" font-size="27" font-family="Segoe UI,Microsoft YaHei">本地 AI 路由代理 · 多 Key 备份 · 多模型协同</text><g transform="translate(220 248)"><rect width="190" height="44" rx="22" fill="#EDE9FE"/><text x="25" y="29" fill="#6D5EF7" font-size="19" font-family="Segoe UI,Microsoft YaHei" font-weight="700">LOCAL-FIRST AI</text></g><g transform="translate(430 248)"><rect width="205" height="44" rx="22" fill="#DCFCE7"/><text x="25" y="29" fill="#15803D" font-size="19" font-family="Segoe UI,Microsoft YaHei" font-weight="700">FAILOVER READY</text></g><g transform="translate(655 248)"><rect width="220" height="44" rx="22" fill="#F4F4F5"/><text x="25" y="29" fill="#52525B" font-size="19" font-family="Segoe UI,Microsoft YaHei" font-weight="700">MULTI-MODEL BRAIN</text></g><path d="M930 85 L1080 85 M930 115 L1035 115 M930 145 L1080 145" stroke="#D4D4D8" stroke-width="8" stroke-linecap="round"/><circle cx="1045" cy="295" r="58" fill="#EDE9FE"/><circle cx="1045" cy="295" r="24" fill="#6D5EF7"/><circle cx="930" cy="340" r="34" fill="#FFF" stroke="#D4D4D8" stroke-width="5"/><circle cx="1130" cy="340" r="34" fill="#FFF" stroke="#D4D4D8" stroke-width="5"/><path d="M962 329 L1017 303 M1074 303 L1098 329" stroke="#A1A1AA" stroke-width="5"/></svg>`;
const route = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="430" viewBox="0 0 1200 430"><rect width="1200" height="430" rx="28" fill="#FAFAFA"/><text x="56" y="62" fill="#18181B" font-size="30" font-family="Segoe UI,Microsoft YaHei" font-weight="700">请求链路：客户端只需要认识一个本地入口</text><g font-family="Segoe UI,Microsoft YaHei"><rect x="70" y="160" width="230" height="125" rx="18" fill="#FFF" stroke="#E4E4E7" stroke-width="3"/><text x="110" y="210" fill="#18181B" font-size="25" font-weight="700">AI 客户端</text><text x="110" y="250" fill="#71717A" font-size="20">Claude CLI / Codex</text><rect x="465" y="140" width="270" height="165" rx="22" fill="#F5F3FF" stroke="#6D5EF7" stroke-width="4"/><circle cx="520" cy="205" r="20" fill="#6D5EF7"/><text x="560" y="210" fill="#4338CA" font-size="27" font-weight="700">SynaRoute</text><text x="505" y="252" fill="#71717A" font-size="20">127.0.0.1 : 端口</text><rect x="900" y="95" width="220" height="72" rx="16" fill="#FFF" stroke="#E4E4E7" stroke-width="3"/><text x="953" y="140" fill="#18181B" font-size="23" font-weight="700">Key 1</text><rect x="900" y="190" width="220" height="72" rx="16" fill="#FFF" stroke="#E4E4E7" stroke-width="3"/><text x="953" y="235" fill="#18181B" font-size="23" font-weight="700">Key 2</text><rect x="900" y="285" width="220" height="72" rx="16" fill="#FFF" stroke="#E4E4E7" stroke-width="3"/><text x="953" y="330" fill="#18181B" font-size="23" font-weight="700">Key 3</text></g><g stroke="#A1A1AA" stroke-width="5" fill="none"><path d="M300 222 L452 222"/><path d="M735 222 C805 222 810 131 890 131"/><path d="M735 222 L890 226"/><path d="M735 222 C805 222 810 321 890 321"/></g><g font-family="Segoe UI,Microsoft YaHei" font-size="20"><text x="338" y="198" fill="#71717A">统一入口</text><text x="760" y="112" fill="#71717A">按优先级尝试</text><text x="460" y="365" fill="#52525B">默认只监听本机回环地址，不对外暴露</text></g></svg>`;
const failover = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="430" viewBox="0 0 1200 430"><rect width="1200" height="430" rx="28" fill="#FAFAFA"/><text x="56" y="62" fill="#18181B" font-size="30" font-family="Segoe UI,Microsoft YaHei" font-weight="700">故障转移：失败才切换，不做轮询</text><g font-family="Segoe UI,Microsoft YaHei"><rect x="75" y="130" width="300" height="210" rx="18" fill="#FFF" stroke="#E4E4E7" stroke-width="3"/><text x="110" y="178" fill="#71717A" font-size="20">本次请求</text><text x="110" y="228" fill="#18181B" font-size="28" font-weight="700">主 Key → 备用 Key</text><text x="110" y="275" fill="#71717A" font-size="20">客户端不需要修改配置</text><rect x="505" y="105" width="260" height="90" rx="18" fill="#FEF2F2" stroke="#FCA5A5" stroke-width="3"/><circle cx="550" cy="150" r="15" fill="#EF4444"/><text x="585" y="158" fill="#991B1B" font-size="25" font-weight="700">Key A 失败</text><text x="550" y="230" fill="#71717A" font-size="19">429 / 5xx / 超时</text><rect x="845" y="235" width="260" height="90" rx="18" fill="#F0FDF4" stroke="#86EFAC" stroke-width="3"/><circle cx="890" cy="280" r="15" fill="#22C55E"/><text x="925" y="288" fill="#166534" font-size="25" font-weight="700">Key B 成功</text></g><path d="M375 235 L485 155" stroke="#A1A1AA" stroke-width="5" fill="none"/><path d="M765 180 C815 205 820 240 830 270" stroke="#A1A1AA" stroke-width="5" fill="none"/><path d="M805 182 l-20 5 8 17" fill="#A1A1AA"/><path d="M820 268 l3 20 17-10" fill="#A1A1AA"/><text x="505" y="375" fill="#6D5EF7" font-size="22" font-family="Segoe UI,Microsoft YaHei" font-weight="700">多 Key 才能形成可用的备份链路</text></svg>`;
const brain = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="470" viewBox="0 0 1200 470"><rect width="1200" height="470" rx="28" fill="#FAFAFA"/><text x="56" y="62" fill="#18181B" font-size="30" font-family="Segoe UI,Microsoft YaHei" font-weight="700">大脑聚合：并行作答 → 聚合 → 决策者输出</text><g font-family="Segoe UI,Microsoft YaHei"><rect x="75" y="128" width="230" height="68" rx="16" fill="#FFF" stroke="#D4D4D8" stroke-width="3"/><text x="145" y="171" fill="#18181B" font-size="24" font-weight="700">成员 A</text><rect x="75" y="220" width="230" height="68" rx="16" fill="#FFF" stroke="#D4D4D8" stroke-width="3"/><text x="145" y="263" fill="#18181B" font-size="24" font-weight="700">成员 B</text><rect x="75" y="312" width="230" height="68" rx="16" fill="#FFF" stroke="#D4D4D8" stroke-width="3"/><text x="145" y="355" fill="#18181B" font-size="24" font-weight="700">成员 C</text><rect x="470" y="180" width="250" height="135" rx="22" fill="#F5F3FF" stroke="#6D5EF7" stroke-width="4"/><text x="535" y="230" fill="#4338CA" font-size="27" font-weight="700">聚合处理</text><text x="510" y="270" fill="#71717A" font-size="20">压缩汇总 / 全量上下文</text><text x="527" y="298" fill="#71717A" font-size="18">可配置并发与超时</text><rect x="890" y="180" width="235" height="135" rx="22" fill="#18181B"/><text x="946" y="230" fill="#FFF" font-size="27" font-weight="700">决策者</text><text x="925" y="270" fill="#A1A1AA" font-size="20">综合意见</text><text x="936" y="298" fill="#A7F3D0" font-size="20">输出最终方案</text></g><g stroke="#A1A1AA" stroke-width="5" fill="none"><path d="M305 162 C380 162 390 220 455 230"/><path d="M305 254 L455 254"/><path d="M305 346 C380 346 390 290 455 280"/><path d="M720 248 L875 248"/></g><text x="58" y="425" fill="#71717A" font-size="20" font-family="Segoe UI,Microsoft YaHei">提示：聚合会产生多次模型调用，当前首版聚合处理纯文本响应。</text></svg>`;

const content = [];
content.push(imageParagraph(hero, 'hero.svg', 640, 229));
content.push(para([run('SynaRoute', { bold: true, size: 38, color: '18181B' }), run('：把 AI 工作流接到一层更可控的本地路由上', { bold: true, size: 38, color: '18181B' })], 'Title', { align: 'center', spacing: '<w:spacing w:before="40" w:after="100"/>' }));
content.push(text('多 Key 备份 · 多模型协同 · 本地运行', { align: 'center', run: { size: 23, color: '71717A' }, spacing: '<w:spacing w:before="0" w:after="180"/>' }));
content.push(callout('你不需要为每个客户端反复改配置：SynaRoute 在本机提供一个统一入口，负责 Key 路由、模型映射，以及可选的大脑聚合。'));

content.push(heading('先说结论：SynaRoute 解决两类问题', 1));
content.push(twoCards(
  card('01  稳定工作流', '主 Key 失败或超时，按优先级尝试下一个可用 Key。你继续使用 Claude CLI、Codex 或其他接入工具，不必临时改配置。', '6D5EF7'),
  card('02  多模型协同', '多个模型并行回答同一个问题，再交给决策者综合。适合代码审查、方案设计和疑难排查等需要多视角的任务。', '8B5CF6')
));
content.push(text('它不是把所有请求都“平均分发”，也不是承诺所有上游永远可用。它做的是把本地配置和上游波动，收敛到一层可观察、可调整的路由代理。', { run: { size: 23, color: '52525B' } }));

content.push(heading('它在请求链路中的位置', 1));
content.push(imageParagraph(route, 'route.svg', 640, 229));
content.push(text('SynaRoute 默认在 127.0.0.1 上启动本地代理端口。客户端请求先到本地，再由 SynaRoute 按你配置的 Key、模型和优先级转发到上游。', { run: { size: 23 } }));
content.push(callout('本地优先：代理默认只监听本机回环地址；密钥在本地加密存储，不需要注册云端账号。', 'green'));

content.push(heading('核心能力一：Key 故障转移', 1));
content.push(imageParagraph(failover, 'failover.svg', 640, 229));
content.push(text('当你配置多个 Key 后，SynaRoute 会按优先级使用它们。只有主 Key 的真实请求失败、超时或遇到限流/错误等情况时，才进入下一个候选 Key。', { run: { size: 23 } }));
content.push(heading('这和“轮询”不一样', 2));
content.push(text('SynaRoute 的默认策略是故障转移优先，而不是每次请求都轮流换 Key。正常情况下，优先 Key 继续承担请求；出现连续失败后，系统才尝试下一个候选。这样更接近“主备链路”，也更容易理解和排查。', { run: { size: 23 } }));
content.push(heading('你能看到什么？', 2));
['Key 的启用状态、优先级和健康信息。', '真实请求触发切换时的运行日志。', '每个 Key 对应的模型映射和上游地址。', '失败时明确的错误，而不是静默地假装成功。'].forEach(v => content.push(bullet(v)));
content.push(callout('适合：手里有多个厂商/中转站 Key，希望主线路出现波动时，工作流还有备用路径。', 'gray'));

content.push(heading('核心能力二：AI 大脑聚合', 1));
content.push(imageParagraph(brain, 'brain.svg', 640, 251));
content.push(text('大脑聚合不是简单地“多问几个模型”。SynaRoute 把一次请求拆成协作流程：多个成员模型并行作答 → 按策略处理结果 → 交给最终决策者综合输出。', { run: { size: 23 } }));
content.push(heading('它适合什么问题？', 2));
['代码审查：让不同模型分别关注逻辑、安全和边界条件。', '方案设计：比较不同实现路径的收益、成本与风险。', '疑难排查：从多个假设出发，减少只沿一条思路排查的盲区。', '内容创作：分别处理结构、表达和事实检查，再由决策者统一整理。'].forEach(v => content.push(bullet(v)));
content.push(heading('两种聚合方式', 2));
content.push(twoCards(
  card('压缩汇总', '先压缩成员结果，再交给决策者。成员多、上下文大时更节省上下文。', '6D5EF7'),
  card('全量上下文', '把成员的完整结果交给决策者。成员较少、希望保留更多细节时使用。', '8B5CF6')
));
content.push(callout('透明使用：大脑聚合会产生多次模型调用，实际消耗 Key 额度并增加等待时间；当前首版主要处理纯文本响应。', 'gray'));

content.push(heading('不仅是转发：模型映射与本地边界', 1));
content.push(twoCards(
  card('模型映射', '客户端请求的模型名与上游真实模型名不一致时，可以通过映射规则完成转换，减少客户端侧的重复适配。', '6D5EF7'),
  card('本地加密存储', 'API Key 保存在本机的加密存储中，不写入客户端的明文配置；配置预览会对敏感信息脱敏。', '22C55E')
));
content.push(text('需要说明的是：本地运行不等于上游服务不会出问题。SynaRoute 只负责本地路由、配置和转发；上游的限流、余额、服务状态仍由对应服务商决定。', { run: { size: 23, color: '52525B' } }));

content.push(heading('三步开始使用', 1));
content.push(twoCards(
  card('1  添加 Key', '进入对应的 Claude CLI、Claude 桌面端或 Codex 分类，添加一个或多个 Key，设置模型与优先级。', '6D5EF7'),
  card('2  启动本地代理', '点击启动，SynaRoute 在 127.0.0.1 分配端口，并生成客户端需要的接入配置。', '6D5EF7')
));
content.push(twoCards(
  card('3  正常使用客户端', '新开终端或重启客户端，让它读取新的本地入口。之后按普通方式使用即可。', '22C55E'),
  card('需要大脑聚合时', '在“大脑”页配置成员、决策者与聚合策略，再运行一次具体需求。', '8B5CF6')
));

content.push(heading('它适合谁？', 1));
['长期使用 Claude CLI、Codex 或类似 AI 工具的开发者。', '手上有多个不同厂商、不同线路 API Key 的用户。', '希望把“改配置、换 Key、查模型名”集中管理的人。', '对代码审查、方案比较和复杂问题排查有多视角需求的人。'].forEach(v => content.push(bullet(v)));
content.push(heading('也请提前知道这些边界', 2));
['故障转移不是负载均衡：默认是主 Key 失败后再切换。', '大脑聚合不是准确率保证：它提供多视角，不替代人的判断。', '聚合会增加调用次数、成本和延迟，是否开启应按任务价值决定。', '当前聚合流程以纯文本响应为主，多模态和复杂工具调用不属于这一版的核心范围。'].forEach(v => content.push(bullet(v)));

content.push(heading('如果你也在用多个 Key，不妨试试', 1));
content.push(text('SynaRoute 的出发点很简单：把 AI 客户端与上游服务之间最容易反复折腾的部分，放进一个本地、可视化、可观察的路由层里。', { run: { size: 25, color: '18181B', bold: true } }));
content.push(text('需要稳定工作流时，用多 Key 做故障转移；需要多视角思考时，打开大脑聚合。两项能力可以分别使用，也可以放在同一套本地配置里管理。', { run: { size: 23 } }));
content.push(callout('官网： https://synaroute.mofamilys.com/zh', 'green'));
content.push(text('如果你正在使用 Claude CLI、Claude 桌面端或 Codex，欢迎把你的接入场景、遇到的问题和希望支持的模型告诉我。', { run: { size: 23, color: '52525B' } }));
content.push(para(run('SynaRoute  ·  Local AI routing, made observable.', { size: 20, color: 'A1A1AA', italic: true }), 'Body', { align: 'center', spacing: '<w:spacing w:before="260" w:after="60"/>' }));

const docXml = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><w:body>${content.join('')}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="900" w:right="1100" w:bottom="950" w:left="1100" w:header="520" w:footer="520" w:gutter="0"/></w:sectPr></w:body></w:document>`;
const stylesXml = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Segoe UI" w:eastAsia="Microsoft YaHei" w:hAnsi="Segoe UI"/><w:sz w:val="24"/><w:color w:val="18181B"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:line="360" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="Body"><w:name w:val="Body"/><w:basedOn w:val="Normal"/><w:rPr><w:rFonts w:ascii="Segoe UI" w:eastAsia="Microsoft YaHei" w:hAnsi="Segoe UI"/><w:sz w:val="24"/><w:color w:val="18181B"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:rPr><w:rFonts w:ascii="Segoe UI" w:eastAsia="Microsoft YaHei" w:hAnsi="Segoe UI"/><w:b/><w:sz w:val="38"/><w:color w:val="18181B"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="Heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Body"/><w:qFormat/><w:pPr><w:outlineLvl w:val="0"/><w:spacing w:before="330" w:after="150"/></w:pPr><w:rPr><w:rFonts w:ascii="Segoe UI" w:eastAsia="Microsoft YaHei" w:hAnsi="Segoe UI"/><w:b/><w:sz w:val="31"/><w:color w:val="4338CA"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="Heading 2"/><w:basedOn w:val="Normal"/><w:next w:val="Body"/><w:qFormat/><w:pPr><w:outlineLvl w:val="1"/><w:spacing w:before="220" w:after="100"/></w:pPr><w:rPr><w:rFonts w:ascii="Segoe UI" w:eastAsia="Microsoft YaHei" w:hAnsi="Segoe UI"/><w:b/><w:sz w:val="27"/><w:color w:val="6D5EF7"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Callout"><w:name w:val="Callout"/><w:basedOn w:val="Normal"/><w:rPr><w:rFonts w:ascii="Segoe UI" w:eastAsia="Microsoft YaHei" w:hAnsi="Segoe UI"/><w:sz w:val="27"/><w:b/></w:rPr></w:style></w:styles>`;
const numberingXml = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="520" w:hanging="260"/></w:pPr><w:rPr><w:rFonts w:ascii="Symbol" w:hAnsi="Symbol"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>`;
const packageRels = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>`;
const documentRels = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/hero.svg"/><Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/route.svg"/><Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/failover.svg"/><Relationship Id="rId8" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/brain.svg"/></Relationships>`;
const typesXml = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="svg" ContentType="image/svg+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/></Types>`;
const coreXml = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>SynaRoute 公众号文章（官网风格重制版）</dc:title><dc:creator>MoFamily</dc:creator><cp:lastModifiedBy>Claude</cp:lastModifiedBy></cp:coreProperties>`;
const appXml = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Claude Code</Application></Properties>`;

fs.writeFileSync(path.join(root, '[Content_Types].xml'), typesXml);
fs.writeFileSync(path.join(root, '_rels', '.rels'), packageRels);
fs.writeFileSync(path.join(root, 'word', 'document.xml'), docXml);
fs.writeFileSync(path.join(root, 'word', 'styles.xml'), stylesXml);
fs.writeFileSync(path.join(root, 'word', 'numbering.xml'), numberingXml);
fs.writeFileSync(path.join(root, 'word', '_rels', 'document.xml.rels'), documentRels);
fs.writeFileSync(path.join(root, 'docProps', 'core.xml'), coreXml);
fs.writeFileSync(path.join(root, 'docProps', 'app.xml'), appXml);

const zipPath = output.replace(/\.docx$/i, '.zip');
fs.rmSync(zipPath, { force: true });
fs.rmSync(output, { force: true });
const ps = `Compress-Archive -Path '${root.replace(/'/g, "''")}\\*' -DestinationPath '${zipPath.replace(/'/g, "''")}' -Force`;
execFileSync('powershell.exe', ['-NoProfile', '-Command', ps], { stdio: 'inherit' });
fs.renameSync(zipPath, output);
console.log(output);
