import { $, lsGet, lsSet } from './api.js';
import { renderLogRow } from './log-view.js';

function compileSearch(query, useRegex, caseSensitive) {
  if (!query) return { regex: null, error: '' };
  const source = useRegex ? query : query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  try {
    return { regex: new RegExp(source, caseSensitive ? 'g' : 'gi'), error: '' };
  } catch (error) {
    return { regex: null, error: error.message };
  }
}

export function createSearch({ log, postSettings, timestamps }) {
  // ===== VS Code 式日志搜索(P0)=====
  // 数据源:#log 已渲染行文本(≤500,与服务端 MAX_LOG_ROWS 一致;被裁剪即不可搜);按行匹配,不跨行。
  // 词级高亮按 runs 重建保留 ANSI span;命中上限 2000 封顶;高亮重绘 rAF 批量提交。
  const sBar = $('search-bar'), sInput = $('s-input'), sCount = $('s-count');
  const sPrev = $('s-prev'), sNext = $('s-next'), sClose = $('s-close');
  const sCaseBtn = $('s-case'), sRegexBtn = $('s-regex');
  let searchOpen = false;
  let scase = (lsGet('search-case') ?? localStorage.getItem('search.case')) === '1';  // 持久化:开关;query 不持久化
  let sregex = false;
  let sre = null, serrMsg = '';       // 编译后的行匹配正则 / 非法正则原始消息
  let matches = [];                   // 有序命中 [{rec, rg}](与 DOM 行序一致,裁剪时构成前缀)
  let sCur = -1;                      // 当前命中下标,-1 = 未定位
  let capped = false;                 // 命中达 2000 封顶
  let sDebT = 0, sRaf = 0;
  let sDebouncing = false;            // debounce 窗口:计数位保持 —,不闪上一轮陈旧计数
  const sPending = new Set();         // 待重绘行(rAF 批量提交)

  function selectionInLog() {
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount) return '';
    const node = sel.getRangeAt(0).commonAncestorContainer;
    if (node !== log && !log.contains(node)) return '';
    return sel.toString();
  }
  function compileQuery() {
    const compiled = compileSearch(sInput.value, sregex, scase);
    sre = compiled.regex;
    serrMsg = compiled.error;
  }
  // 单行扫描:产出命中区间(行内偏移)入全局有序 matches,2000 封顶
  function scanRec(rec) {
    const ranges = [];
    if (sre && !capped) {
      sre.lastIndex = 0;
      let m;
      while ((m = sre.exec(rec.text))) {
        if (!m[0].length) { // 零宽匹配防御(如 a*):跳过并前进,防死循环
          sre.lastIndex++;
          if (sre.lastIndex > rec.text.length) break;
          continue;
        }
        ranges.push({ s: m.index, e: m.index + m[0].length });
        if (matches.length + ranges.length >= 2000) { capped = true; break; }
      }
    }
    rec.ranges = ranges;
    for (const rg of ranges) matches.push({ rec, rg });
    const plain = !ranges.length && (!rec.renderedRef || !rec.renderedRef.length);
    if (plain) rec.renderedRef = ranges; // DOM 已是无高亮形态,免重绘
    else queueRender(rec);
  }
  function curRgOf(rec) {
    return (sCur >= 0 && sCur < matches.length && matches[sCur].rec === rec)
      ? matches[sCur].rg : null;
  }
  function renderRow(rec) {
    renderLogRow(rec, { currentRange: curRgOf(rec), timestamps: timestamps() });
  }
  function queueRender(rec) {
    sPending.add(rec);
    if (!sRaf) sRaf = requestAnimationFrame(() => {
      sRaf = 0;
      const list = [...sPending]; sPending.clear();
      for (const r of list) if (r.el.isConnected) renderRow(r);
    });
  }
  function rebuild() { // 全量重算:重编正则 → 逐行扫描 → 只重绘高亮状态变化的行
    compileQuery();
    matches = []; sCur = -1; capped = false;
    for (const el of [...log.children]) {
      if (el.__rec) scanRec(el.__rec);
    }
    updateCount();
  }
  function updateCount() {
    if (sDebouncing) return; // debounce 窗口(含流式新行的增量计数):维持 —,到期后由 rebuild 显示新结果
    sInput.classList.toggle('err', !!serrMsg);
    if (!sInput.value) { sCount.textContent = '—'; sCount.className = ''; sCount.title = ''; }
    else if (serrMsg) { sCount.textContent = '无效正则'; sCount.className = 'err'; sCount.title = 'RegExp: ' + serrMsg; }
    else if (!matches.length) { sCount.textContent = '0/0'; sCount.className = 'err'; sCount.title = ''; }
    else {
      sCount.textContent = (sCur + 1) + '/' + (capped ? '2000+' : matches.length);
      sCount.className = ''; sCount.title = '';
    }
    const dis = !!serrMsg || !sInput.value || !matches.length; // 错误态/空 query/无命中 → 导航禁用
    sPrev.disabled = sNext.disabled = dis;
  }
  // 导航滚动:当前命中行已整个可见则不动,否则垂直居中;此滚动视为手动滚动
  function revealHit(el) {
    const lr = log.getBoundingClientRect(), er = el.getBoundingClientRect();
    if (er.top >= lr.top && er.bottom <= lr.bottom) return;
    log.scrollTop += (er.top + er.bottom) / 2 - (lr.top + lr.bottom) / 2;
  }
  function setCurrent(i) {
    const prev = sCur >= 0 && sCur < matches.length ? matches[sCur] : null;
    sCur = i;
    const cur = matches[i];
    if (prev && prev.rec !== cur.rec) renderRow(prev.rec);
    renderRow(cur.rec);
    revealHit(cur.rec.el);
    updateCount();
  }
  function navigate(dir) {
    if (!sre || serrMsg || !sInput.value || !matches.length) return;
    let i = sCur + dir;
    if (i >= matches.length) i = 0;             // 循环环绕
    if (i < 0) i = matches.length - 1;
    setCurrent(i);
  }
  function resetRows() { // 行 DOM 清空(清空/快照重建):高亮回 IDLE,query 与开关保留
    log.innerHTML = '';
    matches = []; sCur = -1; capped = false; sPending.clear();
    updateCount();
  }
  // 行被 DOM 裁剪(>500,与服务端 MAX_LOG_ROWS 对齐):摘除其命中(命中序与行序一致 → 被裁行命中构成前缀);
  // 当前命中所在行被裁 → 推进至下一存活命中(不滚动)
  function onRowsRemoved(els) {
    if (!matches.length) return;
    const set = new Set(els);
    let cut = 0;
    while (cut < matches.length && set.has(matches[cut].rec.el)) cut++;
    if (!cut) return;
    matches.splice(0, cut);
    if (sCur >= 0) {
      sCur = sCur < cut ? (matches.length ? 0 : -1) : sCur - cut;
      if (sCur >= 0) renderRow(matches[sCur].rec);
    }
    updateCount();
  }
  function clearHighlights() {
    matches = []; sCur = -1; capped = false;
    for (const el of [...log.children]) {
      const rec = el.__rec;
      if (rec && rec.ranges.length) { rec.ranges = []; queueRender(rec); }
    }
  }
  function openSearch() {
    const first = !searchOpen;
    searchOpen = true;
    sBar.hidden = false;
    const sel = selectionInLog();               // 仅当 #log 内有文本选区才带入
    if (sel) {
      sInput.value = sel.split('\n')[0].slice(0, 200);
      rebuild();                                // 带入选区即重算
    } else if (first) {
      rebuild();                                // 首次打开:全量重算,不滚动不定位
    }
    sInput.focus();
    sInput.select();                            // query 全选,键入即覆盖
  }
  function closeSearch() {
    if (!searchOpen) return;
    searchOpen = false;
    sBar.hidden = true;
    clearHighlights();                            // 清全部高亮;query 与开关保留内存
  }
  function refreshToggles() {
    sCaseBtn.classList.toggle('on', scase);
    sRegexBtn.classList.toggle('on', sregex);
    sCaseBtn.setAttribute('aria-pressed', String(scase));
    sRegexBtn.setAttribute('aria-pressed', String(sregex));
  }
  sCaseBtn.onclick = () => {                      // 开关切换:立即重算
    scase = !scase; lsSet('search-case', scase ? '1' : '0');
    refreshToggles(); if (searchOpen) rebuild();
  };
  sRegexBtn.onclick = () => {
    sregex = !sregex;
    refreshToggles(); if (searchOpen) rebuild();
    // 正则开关同步服务端偏好(重启后面板初值恢复)
    postSettings({ searchRegex: sregex }).catch(() => {});
  };
  sInput.addEventListener('input', () => {        // 输入即搜:150ms debounce 全量重算
    clearTimeout(sDebT);
    // 输入即清计数位为 —(含 err 类):debounce 窗口内不显示上一轮陈旧计数
    sDebouncing = true;
    sCount.textContent = '—';
    sCount.className = '';
    sCount.title = '';
    sDebT = setTimeout(() => { sDebouncing = false; if (searchOpen) rebuild(); }, 150);
  });
  sInput.addEventListener('keydown', e => {
    if (e.key === 'Enter') { e.preventDefault(); navigate(e.shiftKey ? -1 : 1); }
  });
  sNext.addEventListener('click', () => { navigate(1); sInput.focus(); });
  sPrev.addEventListener('click', () => { navigate(-1); sInput.focus(); });
  sClose.addEventListener('click', closeSearch);
  // document 级接管(capture,先于页内 handler):Ctrl+F 打开、F3/Shift+F3 导航、Esc 关闭
  document.addEventListener('keydown', e => {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && (e.key === 'f' || e.key === 'F')) {
      e.preventDefault();
      openSearch();
    } else if (e.key === 'F3') {
      if (searchOpen) { e.preventDefault(); navigate(e.shiftKey ? -1 : 1); }
    } else if (e.key === 'Escape' && searchOpen) {
      e.preventDefault();
      closeSearch();
    }
  }, true);
  refreshToggles();

  return {
    isOpen: () => searchOpen,
    onRowsRemoved,
    renderRow,
    resetRows,
    scanRec,
    selectionInLog,
    updateCount,
    setRegex(value) {
      sregex = value;
      refreshToggles();
    },
  };
}
