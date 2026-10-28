export const fmtBytes = n => n < 1024 ? n + ' B' : (n / 1024).toFixed(1) + ' KB';

export function fmtRate(n) {
  n = Math.max(0, n || 0);
  if (n < 1024) return Math.round(n) + ' B/s';
  if (n < 1048576) return (n / 1024).toFixed(1) + ' KB/s';
  return (n / 1048576).toFixed(1) + ' MB/s';
}

export function fmtUptime(s) {
  s = Math.max(0, Math.floor(s || 0));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), ss = s % 60;
  return h ? h + 'h' + m + 'm' : m ? m + 'm' + ss + 's' : ss + 's';
}

function fmtTimestamp(ts) {
  const d = new Date(ts);
  const p = (n, w = 2) => String(n).padStart(w, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}

function appendSegment(el, run, from, to, hit, current) {
  const node = document.createElement(hit ? 'mark' : 'span');
  node.textContent = run.text.slice(from, to);
  if (run.fg) node.style.color = run.fg;
  if (hit) node.className = current ? 's-hit cur' : 's-hit';
  el.appendChild(node);
}

function timestampLength(el) {
  const first = el.firstElementChild;
  return first?.classList.contains('ts') ? first.textContent.length : 0;
}

function captureSelection(el) {
  const selection = window.getSelection();
  if (!selection || !selection.rangeCount) return null;
  const anchorInside = el.contains(selection.anchorNode);
  const focusInside = el.contains(selection.focusNode);
  if (!anchorInside && !focusInside) return null;
  const prefix = timestampLength(el);
  const offset = (node, nodeOffset) => {
    const range = document.createRange();
    range.selectNodeContents(el);
    range.setEnd(node, nodeOffset);
    return Math.max(0, range.toString().length - prefix);
  };
  return {
    selection,
    anchorNode: selection.anchorNode,
    anchorOffset: anchorInside ? offset(selection.anchorNode, selection.anchorOffset) : selection.anchorOffset,
    focusNode: selection.focusNode,
    focusOffset: focusInside ? offset(selection.focusNode, selection.focusOffset) : selection.focusOffset,
    anchorInside,
    focusInside,
  };
}

function textPoint(el, offset) {
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
  let node = walker.nextNode();
  while (node) {
    if (offset <= node.textContent.length) return [node, offset];
    offset -= node.textContent.length;
    node = walker.nextNode();
  }
  return [el, el.childNodes.length];
}

function restoreSelection(el, saved) {
  if (!saved) return;
  const prefix = timestampLength(el);
  const anchor = saved.anchorInside
    ? textPoint(el, saved.anchorOffset + prefix)
    : [saved.anchorNode, saved.anchorOffset];
  const focus = saved.focusInside
    ? textPoint(el, saved.focusOffset + prefix)
    : [saved.focusNode, saved.focusOffset];
  saved.selection.setBaseAndExtent(...anchor, ...focus);
}

// ANSI runs、时间戳和搜索高亮共用的唯一行渲染入口。
export function renderLogRow(rec, { currentRange = null, timestamps = false } = {}) {
  const el = rec.el;
  const selection = captureSelection(el);
  el.textContent = '';
  if (timestamps && rec.ts != null) {
    const prefix = document.createElement('span');
    prefix.className = 'ts';
    prefix.textContent = '[' + fmtTimestamp(rec.ts) + '] ';
    el.appendChild(prefix);
  }
  let offset = 0;
  for (const run of rec.runs) {
    const start = offset, end = offset + run.text.length;
    let position = start;
    for (const range of rec.ranges) {
      if (range.e <= start || range.s >= end) continue;
      const a = Math.max(range.s, start), b = Math.min(range.e, end);
      if (a > position) appendSegment(el, run, position - start, a - start, false, false);
      appendSegment(el, run, a - start, b - start, true, range === currentRange);
      position = b;
    }
    if (position < end) appendSegment(el, run, position - start, end - start, false, false);
    offset = end;
  }
  rec.renderedRef = rec.ranges;
  restoreSelection(el, selection);
}
