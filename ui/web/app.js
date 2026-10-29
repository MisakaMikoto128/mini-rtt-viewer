import { $, API, getJson, lsGet, lsSet, postJson, request } from './api.js';
import { fmtBytes as fmt, fmtRate, fmtUptime } from './log-view.js';
import { createSearch } from './search.js';
import { hexToText, initDisplaySettings, textToHex } from './settings.js';
import { initDialogs, showToast } from './dialogs.js';
import { initSound, playSound } from './sound.js';

const log = $('log');
let connected = false, autoScroll = true;
let paused = false, powerOn = false;
// 复位会经历断开/重连；窗口内只保留复位反馈，避免一次操作连响三次。
let resetSoundUntil = 0;
// S1 速率/S2 时长状态:stats 每 500ms 一拍,lastStats 记上次字节快照差值算 B/s
let lastStats = null;
// S4 时间戳开关(纯前端显示层;localStorage 记忆,与字号/主题同模式)
let tsOn = false;
// S7 行数上限:以 stats.cap(服务端 MAX_LOG_ROWS)为准,初始值仅是
// 首拍到站前的 trim 兜底(与服务端当前常量一致)
let rowCap = 500;

// 纯显示偏好本地恢复；连接、接收和发送设置只由服务端 /api/prefs 恢复。
(function restoreDisplayPrefs() {
  if (lsGet('auto-scroll') !== null) {
    $('auto-scroll').checked = lsGet('auto-scroll') === '1';
    autoScroll = $('auto-scroll').checked;
  }
  if (lsGet('timestamps') === '1') {
    tsOn = true;
    $('timestamps').checked = true;
  }
})();

function appendRow(row, live) {
  const div = document.createElement('div');
  div.className = 'row';
  // 行记录 = 搜索的文本索引:runs 原文数组 + 行全文;搜索高亮重绘行时按 runs
  // 重建内联色 → 不破坏行内 ANSI span 结构。ts=本机到达时刻(S4,显示层;
  // rec.text/导出/搜索都不含它)。**只有 live rows 事件记 ts**:snapshot 回放的
  // 老行到达时刻不可考 → 不带前缀(已知口径,与任务书一致)
  const rec = { el: div, runs: row.runs, text: row.runs.map(r => r.text).join(''),
    ts: live === false ? null : Date.now(), ranges: [], renderedRef: null };
  div.__rec = rec;
  search.renderRow(rec); // 渲染单点:时间戳前缀 + ANSI runs(搜索高亮重绘同一路径)
  log.appendChild(div);
  if (search.isOpen()) { search.scanRec(rec); search.updateCount(); } // 新行增量扫描:只更新计数,不滚动
}
function trim() {
  const removed = [];
  // 上限与行数显示同源:stats.cap(服务端 MAX_LOG_ROWS),不另行硬编码
  while (log.children.length > rowCap) { removed.push(log.firstChild); log.removeChild(log.firstChild); }
  if (removed.length) search.onRowsRemoved(removed);
}
// 自动滚动互斥:搜索条打开期间流式新行不拉回底部;唯一滚动来源是命中导航。
// follow-suspend(F4,FR-16 语义):勾选态下用户上滚(wheel 负 delta 或
// 拖动滚动条离开贴底)→ 暂停跟随,数据不再拉底;滚回贴底(±4px)→ 自动恢复。
// 取消勾选 = 完全手动(现状保持)
let follow = true;
function atBottom() { return log.scrollTop + log.clientHeight >= log.scrollHeight - 4; }
function scrollBottom() { if (search.isOpen()) return; if (autoScroll && follow) log.scrollTop = log.scrollHeight; }
log.addEventListener('wheel', e => { if (autoScroll && e.deltaY < 0) follow = false; }, { passive: true });
log.addEventListener('scroll', () => { if (autoScroll) follow = atBottom(); });
let clearSeq = 0;
function onMsg(ev) {
  const m = JSON.parse(ev.data);
  if (m.type === 'snapshot') {
    clearSeq = m.clearSeq ?? 0;
    search.resetRows();
    m.rows.forEach(r => appendRow(r, false));
    scrollBottom();
  }
  else if (m.type === 'cleared') { clearSeq = m.seq; search.resetRows(); window.__clearedCount = (window.__clearedCount || 0) + 1; }
  else if (m.type === 'rows') {
    if (m.seq <= clearSeq) return;
    if (m.dropped) {
      const removed = [];
      for (let i = 0; i < m.dropped && log.firstChild; i++) { removed.push(log.firstChild); log.removeChild(log.firstChild); }
      search.onRowsRemoved(removed);
    }
    m.rows.forEach(appendRow); trim(); scrollBottom();
  }
  else if (m.type === 'state') setState(m.connected, m.status);
  else if (m.type === 'power') {
    if (m.ok) {
      powerOn = !!m.on;
      $('power').checked = powerOn;
      playSound('setting');
    } else {
      $('power').checked = powerOn;
      showToast(m.message || '电源切换失败');
    }
  }
  else if (m.type === 'device') renderDevice(m.info);
  else if (m.type === 'progress') {
    const txt = m.text.replace('● ', '');
    $('status').textContent = txt;
  }
  else if (m.type === 'names') fillDatalist(m.names);
  else if (m.type === 'jlinks') fillJlinks(m.list);
  else if (m.type === 'stats') {
    $('tx').textContent = 'TX ' + fmt(m.tx);
    $('rx').textContent = 'RX ' + fmt(m.rx);
    $('rows').textContent = m.rows + ' / ' + m.cap + ' 行'; // S7:上限语境,数字来自服务端
    if (m.cap) rowCap = m.cap;
    $('session').textContent = fmtUptime(m.sessionSec); // S2:会话时长,断开为 0
    updateRates(m); // S1:RX/TX 速率
  }
}
// ---- S1/S2 底栏统计 ----
function updateRates(m) {
  // 口径选择:速率恒显示(与 SerialHub fmtRate 一致),无新数据自然归零;
  // 断开(setState false)显式归零。服务端断开时清零字节,差值
  // 为负由 fmtRate 夹 0,不会出现负速率
  const now = performance.now();
  if (lastStats) {
    const dt = (now - lastStats.t) / 1000;
    if (dt > 0.05) { // 500ms 一拍,防 0 除与首拍抖动
      $('rx-rate').textContent = fmtRate((m.rx - lastStats.rx) / dt);
      $('tx-rate').textContent = fmtRate((m.tx - lastStats.tx) / dt);
    }
  }
  lastStats = { t: now, rx: m.rx, tx: m.tx };
}
function zeroRates() { $('rx-rate').textContent = '0 B/s'; $('tx-rate').textContent = '0 B/s'; lastStats = null; }
// 单只主按钮:「连接」(accent)⇄「断开」(红色系 danger)切换
function setState(on, status) {
  const changed = connected !== on;
  connected = on;
  if (changed && performance.now() >= resetSoundUntil) playSound(on ? 'connect' : 'disconnect');
  $('dot').classList.toggle('on', on);
  if (status) $('status').textContent = status.replace('● ', '');
  // S1/S2:断开清零(速率归零显示、时长归 0、字节快照复位)
  if (!on) {
    zeroRates();
    $('session').textContent = '0s';
    powerOn = false;
    $('power').checked = false;
  }
  const btn = $('connect-btn');
  btn.textContent = on ? '断开' : '连接';
  btn.classList.toggle('danger', on);
  btn.classList.toggle('primary', !on);
  $('reset-btn').disabled = !on;
  $('power').disabled = !on;
  $('send-btn').disabled = !on;
  // 断开后设备信息为上次残留数据(F2):置灰示不可信,连接态恢复
  $('device-details').classList.toggle('off', !on);
  for (const id of ['chip', 'chip-arrow', 'iface', 'speed', 'channel']) $(id).disabled = on;
  $('jlink').disabled = on;
}
function renderDevice(d) {
  if (!d) return;
  // P0-2/V2:不再强制展开——设备信息默认折叠(保面板一屏),点击 summary 可展开;
  // 断开置灰 .off 语义由 setState 维护,不受折叠影响
  const g = $('device-info');
  g.innerHTML = '';
  const put = (k, v) => {
    const a = document.createElement('span'); a.className = 'k'; a.textContent = k;
    const b = document.createElement('span'); b.textContent = v || '—';
    g.append(a, b);
  };
  put('固件版本', d.firmware);
  put('目标芯片', d.target);
  put('接口', (d.iface || '—') + ' · ' + (d.speedKhz || '—') + 'kHz');
  put('序列号', d.serial);
  // P2-1/V6:分隔符定宽居中 span,消除「·SWD」光学不对称
  const cl = $('chip-line');
  const seg = t => { const s = document.createElement('span'); s.textContent = t; return s; };
  const sep = document.createElement('span');
  sep.className = 'sep'; sep.textContent = '·';
  cl.textContent = '';
  cl.append(seg(d.target || '—'), sep, seg(d.iface || '—'));
}
// 可编辑下拉:输入即筛选,↑↓ 选择,Enter 确认,Esc 关闭,箭头展开全部
// P0-4:浮层默认隐藏,仅聚焦/输入/点箭头展开;失焦/Esc/选中后关闭
let chipOptions = [];
let chipListOpen = false;
function fillDatalist(names) {
  chipOptions = names;
  renderChipList($('chip').value); // 只重绘候选,不改开合状态 → names 到达不再自动展开
}
function renderChipList(filter) {
  const list = $('chip-list');
  const f = (filter || '').toUpperCase();
  // P1-8:与当前 #chip 值一致的候选常驻 accent 底白字(忽略大小写)
  const cur = ($('chip').value || '').trim().toUpperCase();
  const opts = chipOptions.filter(o => o.toUpperCase().includes(f));
  // 候选名来自设备库/磁盘缓存(device_names.txt 本机可篡改),innerHTML 拼接
  // 是脚本注入面——DOM API 建 + textContent 承载名字(审计 F-2)
  list.textContent = '';
  for (const [index, option] of opts.entries()) {
    const div = document.createElement('div');
    const current = option.toUpperCase() === cur;
    div.id = `chip-option-${index}`;
    div.className = 'opt' + (current ? ' current' : '');
    div.dataset.v = option;
    div.textContent = option;
    div.setAttribute('role', 'option');
    div.setAttribute('aria-selected', String(current));
    list.appendChild(div);
  }
  list.style.display = (chipListOpen && opts.length) ? 'block' : 'none';
  list.querySelectorAll('.opt').forEach(el => {
    el.addEventListener('mousedown', e => { e.preventDefault(); pickChip(el.dataset.v); });
  });
}
function openChipList(filter) {
  chipListOpen = true;
  $('chip').setAttribute('aria-expanded', 'true');
  $('chip-arrow').setAttribute('aria-expanded', 'true');
  renderChipList(filter !== undefined ? filter : $('chip').value);
}
function closeChipList() {
  chipListOpen = false;
  $('chip-list').style.display = 'none';
  $('chip').setAttribute('aria-expanded', 'false');
  $('chip-arrow').setAttribute('aria-expanded', 'false');
  $('chip').removeAttribute('aria-activedescendant');
}
function setActiveChip(items, active) {
  items.forEach((el, i) => el.classList.toggle('active', i === active));
  $('chip').setAttribute('aria-activedescendant', items[active].id);
  items[active].scrollIntoView({ block: 'nearest' });
}
function pickChip(v) {
  $('chip').value = v;
  closeChipList();
  postPref('chip', { chip: v }); // F1:候选选定同样入库(程序化赋值不触发 change)
}
(function initChipCombo() {
  const input = $('chip'), list = $('chip-list');
  input.addEventListener('input', () => openChipList());
  input.addEventListener('focus', () => openChipList());
  input.addEventListener('blur', () => setTimeout(closeChipList, 150));
  input.addEventListener('keydown', e => {
    const items = [...list.querySelectorAll('.opt')];
    let active = items.findIndex(el => el.classList.contains('active'));
    if (e.key === 'ArrowDown' && items.length) {
      e.preventDefault();
      active = Math.min(active + 1, items.length - 1);
      setActiveChip(items, active);
    } else if (e.key === 'ArrowUp' && items.length) {
      e.preventDefault();
      active = Math.max(active - 1, 0);
      setActiveChip(items, active);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (active >= 0) pickChip(items[active].dataset.v);
      else if (items.length === 1) pickChip(items[0].dataset.v);
    } else if (e.key === 'Escape') {
      closeChipList();
    }
  });
  $('chip-arrow').addEventListener('mousedown', e => e.preventDefault());
  $('chip-arrow').addEventListener('keydown', e => {
    if (e.key === 'Escape') {
      closeChipList();
      $('chip').focus();
    }
  });
  $('chip-arrow').onclick = () => { chipListOpen ? closeChipList() : openChipList(''); };
})();
// 选定的 J-Link 序列号(/api/prefs 恢复用):列表到达时若在列则套用一次
let pendingJlinkSn = null;
function fillJlinks(list) {
  const sel = $('jlink');
  const prev = sel.value; // 刷新(插拔/连接时 worker 重发)不清掉用户已选
  // 产品名来自 DLL acProduct 固件字符串,innerHTML 拼接是注入面(审计 F-2)——
  // DOM API 建,textContent 承载名字
  sel.textContent = '';
  const auto = document.createElement('option');
  auto.value = '';
  auto.textContent = '自动';
  sel.appendChild(auto);
  for (const j of list) {
    const o = document.createElement('option');
    o.value = j.sn;
    o.textContent = j.name;
    sel.appendChild(o);
  }
  const want = pendingJlinkSn != null ? String(pendingJlinkSn) : prev;
  if (want && [...sel.options].some(o => o.value === want)) {
    sel.value = want;
    pendingJlinkSn = null;
  }
}

function connect() {
  const body = {
    chip: $('chip').value,
    ifaceIndex: +$('iface').value,
    speedIndex: $('speed').selectedIndex,
    channel: +$('channel').value,
  };
  // J-Link 下拉接线(审计 P1):选定才带 jlinkSn,空 = 服务端自动选首台
  if ($('jlink').value !== '') body.jlinkSn = +$('jlink').value;
  postJson('/api/connect', body).catch(error => {
    $('status').textContent = error.message;
    showToast(error.message);
  });
}
function disconnect() {
  request('/api/disconnect', { method: 'POST' }).catch(error => showToast(error.message));
}
$('connect-btn').onclick = () => { connected ? disconnect() : connect(); };
$('reset-btn').onclick = () => {
  resetSoundUntil = performance.now() + 1800;
  request('/api/reset', { method: 'POST' })
    .then(() => playSound('reset'))
    .catch(error => {
      resetSoundUntil = 0;
      showToast(error.message);
    });
};
$('power').onchange = e => postJson('/api/power', { on: e.target.checked })
  .catch(error => {
    $('power').checked = powerOn;
    showToast(error.message);
  });

// 运行时设置:changed → POST settings(worker 原子热切换,动态生效)
// 断帧间隔 change 时夹取 1-200,并由服务端偏好持久化。
$('keep-awake').onchange = e => postSettings({ keepAwake: e.target.checked })
  .catch(error => showToast(error.message));
$('rx-ending').onchange = e => postSettings({ rxEnding: +e.target.value });
$('encoding').onchange = e => postSettings({ encodingIndex: +e.target.value });
$('hex-rx').onchange = e => postSettings({ hexRx: e.target.checked });
$('frame-timeout').addEventListener('change', e => {
  let v = parseInt(e.target.value, 10);
  if (isNaN(v)) v = 20;
  v = Math.min(200, Math.max(1, v));
  e.target.value = String(v);
  postSettings({ frameTimeout: v });
});
function postSettings(patch) {
  return postJson('/api/settings', patch).then(() => playSound('setting'));
}
// 定时发送:连接态才触发,重发最近一次发送的内容(0.001-999 秒);
// 间隔非法自动停,不弹错误
function syncTimerFields() {
  $('timer-fields').hidden = !$('timer-on').checked;
}
$('timer-on').onchange = e => {
  syncTimerFields();
  postTimer({ on: e.target.checked }).catch(error => {
    e.target.checked = false;
    syncTimerFields();
    showToast(error.message);
  });
};
$('timer-interval').addEventListener('change', e => {
  const v = parseFloat(e.target.value);
  if (v >= 0.001 && v <= 999) postTimer({ on: $('timer-on').checked, intervalSec: v });
  else {
    $('timer-on').checked = false;
    syncTimerFields();
    postTimer({ on: false });
  }
});
function postTimer(body) {
  const text = $('send-text').value;
  if (text.trim()) {
    body.text = text;
    body.hex = $('hex-send').checked;
  }
  return postJson('/api/timer', body).then(() => playSound('setting'));
}
function setPaused(on) {
  paused = on;
  $('pause-btn').textContent = on ? '继续' : '暂停';
  $('pause-btn').setAttribute('aria-pressed', String(on));
}
$('pause-btn').onclick = () => {
  const next = !paused;
  postJson('/api/pause', { on: next })
    .then(() => {
      setPaused(next);
      playSound(next ? 'pause' : 'resume');
    })
    .catch(error => showToast(error.message || '暂停状态更新失败'));
};
$('clear-btn').onclick = () => request('/api/clear', { method: 'POST' })
  .then(() => {
    search.resetRows();
    playSound('clear');
  })
  .catch(error => showToast(error.message));
$('mark-btn').onclick = () => postJson('/api/mark', { text: '' })
  .then(() => playSound('mark'))
  .catch(error => showToast(error.message));

// 发送 + 会话内历史:↑ 浏览(最新在前)/ ↓ 返回 / ↓ 到底恢复草稿;
// IME composing 期间不响应方向键与 Enter
const sendBox = $('send-text');
const sendHistory = [];
let histIdx = -1, draft = '';
sendBox.addEventListener('keydown', e => {
  if (e.isComposing || e.keyCode === 229) return; // IME 组合输入中
  if (e.key === 'Enter') {
    if (e.shiftKey) return; // Shift+Enter 换行:交给浏览器默认插入换行(F6)
    e.preventDefault(); send(); return;
  }
  if (e.key === 'ArrowUp') {
    if (!sendHistory.length || histIdx >= sendHistory.length - 1) return;
    if (histIdx === -1) draft = sendBox.value;
    histIdx++;
    sendBox.value = sendHistory[histIdx];
    e.preventDefault();
  } else if (e.key === 'ArrowDown') {
    if (histIdx === -1) return;
    histIdx--;
    sendBox.value = histIdx === -1 ? draft : sendHistory[histIdx];
    e.preventDefault();
  }
});
sendBox.addEventListener('input', () => { histIdx = -1; sendBox.classList.remove('send-err'); }); // 编辑即回到草稿模式 + 清错误红边
$('send-btn').onclick = send;
function send() {
  if (!sendBox.value.trim()) return;
  postJson('/api/send', { text: sendBox.value, hex: $('hex-send').checked })
    .then(() => {
      sendBox.classList.remove('send-err');
      const v = sendBox.value;
      const i = sendHistory.indexOf(v);
      if (i >= 0) sendHistory.splice(i, 1);
      sendHistory.unshift(v);
      if (sendHistory.length > 50) sendHistory.pop();
      histIdx = -1; draft = '';
      playSound('send');
    })
    .catch(error => {
      sendBox.classList.add('send-err');
      showToast(error.message || '发送失败');
    });
}
$('auto-scroll').onchange = e => {
  autoScroll = e.target.checked;
  lsSet('auto-scroll', autoScroll ? '1' : '0');
  if (autoScroll) { follow = true; scrollBottom(); }
  playSound('setting');
};

// 面板业务偏好:change → 服务端 /api/settings,reload 由 /api/prefs 回填。
// 文本输入(目标设备)按键防抖 400ms;select/checkbox 一次 change 即一值,
// 按键分桶合并,互不吞并
const prefTimers = {};
function postPref(key, patch, delay) {
  clearTimeout(prefTimers[key]);
  prefTimers[key] = setTimeout(() => { delete prefTimers[key]; postSettings(patch).catch(() => {}); }, delay || 0);
}
// 发送行尾/HEX 发送经 settings 通道记忆(行尾字节由服务端追加,FR-12)
$('tx-ending').addEventListener('change', e => postPref('sendEnding', { sendEnding: +e.target.value }));
// ---- 连接设置组五项:统一经服务端偏好持久化 ----
$('channel').addEventListener('change', e => postPref('channel', { channel: +e.target.value }));
$('iface').addEventListener('change', e => postPref('iface', { ifaceIndex: +e.target.value }));
$('speed').addEventListener('change', e => postPref('speed', { speedIndex: e.target.selectedIndex }));
$('chip').addEventListener('input', () => postPref('chip', { chip: $('chip').value }, 400));
$('chip').addEventListener('change', () => postPref('chip', { chip: $('chip').value }));
$('auto-frame').addEventListener('change', e => postPref('autoFrame', { autoFrame: e.target.checked }));
// HEX 发送:开关记忆 + 双向转换(勾选 文本→hex,取消 hex→文本;不可解析保留
// 原文;只改输入框,不触发发送/历史)
$('hex-send').addEventListener('change', e => {
  postPref('hexSend', { hexSend: e.target.checked });
  const v = sendBox.value;
  if (!v.trim()) return;
  sendBox.value = e.target.checked ? textToHex(v) : (hexToText(v) ?? v);
});
// ---- S4 时间戳开关:显示层装饰,localStorage 记忆(与字号/主题同模式,
// 不进服务端 prefs)---- 切换按现有行 runs 全量重绘(SerialHub :2661-2665 同法):
// 前缀加在行首、ANSI 着色 runs 不受影响;CSS pre-wrap 换行重排时前缀只落在
// 首物理行(与 FR-17 硬换行/按列换行的交互:前缀属行首,续行是纯文本回流)。
// 搜索匹配不含前缀(rec.text 只是 runs 原文,偏移不受影响);导出内容来自
// 服务端原始行文本,天然无前缀;snapshot 回放的老行无 rec.ts → 无前缀(已知口径)
$('timestamps').addEventListener('change', e => {
  tsOn = e.target.checked;
  lsSet('timestamps', tsOn ? '1' : '0');
  for (const el of [...log.children]) if (el.__rec) search.renderRow(el.__rec);
  playSound('setting');
});

initDisplaySettings();
initSound();

const search = createSearch({ log, postSettings, timestamps: () => tsOn });

function connectWS() {
  const ws = new WebSocket(API.replace('http', 'ws') + '/ws');
  ws.onmessage = onMsg;
  ws.onclose = () => setTimeout(connectWS, 1500);
}
connectWS();
setInterval(() => getJson('/api/status').then(s => {
  // F3(UX P1-4):轮询同步连接态**与文案**——sessionStatus 由服务端
  // State/Progress 统一维护,页面中途打开或 WS 漏掉 state 消息时,
  // 状态栏 2s 内对齐,不再出现「未连接 + 绿点 + 断开按钮」同屏
  setPaused(s.paused);
  if (connected !== s.connected) setState(s.connected, s.sessionStatus);
  else if ($('status').textContent !== s.sessionStatus) {
    $('status').textContent = s.sessionStatus;
  }
}).catch(() => {}), 2000);
getJson('/api/devices').then(fillDatalist).catch(() => {});
getJson('/api/jlinks').then(fillJlinks).catch(() => {});
getJson('/api/status').then(s => {
  if (s.device) renderDevice(s.device);
  if (s.version) $('version').textContent = 'v' + s.version; // T10:低调版本号
  // 首屏同步连接态:WS snapshot 不带 state 消息,不调 setState 的话
  // 刷新后 ~2.2s 内红点/「未连接」/禁用按钮与真实连接态矛盾(2s 轮询兜底才纠正)
  setPaused(s.paused);
  if (connected !== s.connected) setState(s.connected, s.sessionStatus);
}).catch(() => {});
initDialogs({ log, selectionInLog: search.selectionInLog });

// 面板初值恢复:服务端持久化偏好(%APPDATA% prefs.json,500ms 快照)。
// 主题/字号仍走 localStorage,不经此通道
getJson('/api/prefs').then(p => {
  if (p.chip) $('chip').value = p.chip;
  $('iface').value = String(p.ifaceIndex);
  $('speed').selectedIndex = Math.min(p.speedIndex, $('speed').length - 1);
  $('channel').selectedIndex = Math.min(p.channel, $('channel').length - 1);
  $('rx-ending').value = String(p.rxEnding);
  $('frame-timeout').value = p.frameTimeout;
  $('encoding').value = String(p.encodingIndex);
  $('hex-rx').checked = !!p.hexRx;
  $('auto-frame').checked = !!p.autoFrame;
  $('keep-awake').checked = !!p.keepAwake;
  if (p.timerIntervalSec != null) $('timer-interval').value = p.timerIntervalSec;
  $('timer-on').checked = !!p.timerOn;
  syncTimerFields();
  if (p.sendEnding != null) $('tx-ending').value = String(p.sendEnding);
  if (typeof p.hexSend === 'boolean') $('hex-send').checked = !!p.hexSend;
  if (typeof p.searchRegex === 'boolean') search.setRegex(p.searchRegex);
  // J-Link 下拉选定恢复(列表由 /api/jlinks 异步填充,这里先记下待套用)
  if (p.jlinkSerial != null) pendingJlinkSn = p.jlinkSerial;
}).catch(() => {});
// 发送历史:服务端持久化(prefs.send_history,去重置顶上限 50)——启动拉一次,
// 最新在前;本次会话新发送在 send() 里即时并入本地(同规则),↑↓ 浏览共用一份
getJson('/api/history').then(h => {
  if (Array.isArray(h)) for (const t of h) if (!sendHistory.includes(t)) sendHistory.push(t);
}).catch(() => {});
