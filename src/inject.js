/* pi-history: injected at serve time. Every feature is isolated in try/catch; if the export's structure
   or data shape is not what we expect, nothing is changed. */
(function () {
  'use strict';
  function safe(fn) { try { fn(); } catch (e) { try { console.warn('[pi-history]', e); } catch (_) {} } }

  var data = null;
  safe(function () {
    var el = document.getElementById('session-data');
    if (!el) return;
    var bin = atob(el.textContent.trim());
    var bytes = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    var d = JSON.parse(new TextDecoder('utf-8').decode(bytes));
    if (d && Array.isArray(d.entries)) data = d;
  });

  function fmtTok(n) {
    n = n || 0;
    if (n < 1000) return String(n);
    if (n < 1e6) return (n / 1000).toFixed(n < 1e4 ? 1 : 0).replace(/\.0$/, '') + 'k';
    return (n / 1e6).toFixed(1).replace(/\.0$/, '') + 'M';
  }
  function costOf(u) {
    var c = u && u.cost;
    if (!c) return null;
    if (typeof c.total === 'number') return c.total;
    return (c.input || 0) + (c.output || 0) + (c.cacheRead || 0) + (c.cacheWrite || 0);
  }
  function fmtCost(v) { return '$' + (v < 0.01 ? v.toFixed(4) : v.toFixed(v < 10 ? 3 : 2)); }
  function fmtDur(ms) {
    if (!(ms > 0)) return '';
    var m = Math.round(ms / 60000);
    if (m < 1) return Math.max(1, Math.round(ms / 1000)) + '秒';
    if (m < 60) return m + '分';
    var h = Math.floor(m / 60);
    if (h < 24) return h + '時間' + (m % 60 ? (m % 60) + '分' : '');
    return Math.floor(h / 24) + '日' + (h % 24) + '時間';
  }
  var ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
  function esc(s) { return String(s).replace(/[&<>"']/g, function (c) { return ESC[c]; }); }
  function pad(n) { return n < 10 ? '0' + n : '' + n; }
  function fmtDate(t) {
    var d = new Date(t);
    if (isNaN(d)) return '';
    return d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()) + ' ' + pad(d.getHours()) + ':' + pad(d.getMinutes());
  }

  // ---- sticky summary bar ----
  safe(function () {
    var content = document.getElementById('content');
    if (!content || document.getElementById('pi-bar')) return;
    var bar = document.createElement('div');
    bar.id = 'pi-bar';
    var html = '<a href="/" title="セッション一覧へ">← 一覧に戻る</a>';
    safe(function () {
      if (!data) return;
      var tok = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, cost = 0, hasCost = false;
      var users = 0, asst = 0, tools = 0, models = {}, first = null, last = null;
      data.entries.forEach(function (e) {
        if (e.timestamp) {
          var t = Date.parse(e.timestamp);
          if (!isNaN(t)) { if (first === null || t < first) first = t; if (last === null || t > last) last = t; }
        }
        if (e.type !== 'message' || !e.message) return;
        var m = e.message;
        if (m.role === 'user') users++;
        if (m.role === 'assistant') {
          asst++;
          if (m.model) models[m.model] = 1;
          if (Array.isArray(m.content)) m.content.forEach(function (b) { if (b && b.type === 'toolCall') tools++; });
          var u = m.usage;
          if (u) {
            tok.input += u.input || 0; tok.output += u.output || 0; tok.cacheRead += u.cacheRead || 0; tok.cacheWrite += u.cacheWrite || 0;
            var c = costOf(u);
            if (c !== null) { cost += c; hasCost = true; }
          }
        }
      });
      var h = data.header || {};
      if (h.cwd) {
        var base = String(h.cwd).replace(/[\\/]+$/, '').split(/[\\/]/).pop();
        html += '<span class="pi-item" title="' + esc(h.cwd) + '">📁<b>' + esc(base || h.cwd) + '</b></span>';
      }
      var start = first !== null ? first : Date.parse(h.timestamp);
      if (!isNaN(start)) {
        var span = last !== null ? fmtDur(last - start) : '';
        html += '<span class="pi-item" title="開始〜最終更新">期間<b>' + esc(fmtDate(start)) +
          (last !== null && last - start >= 60000 ? ' 〜 ' + esc(fmtDate(last)) : '') + (span ? '（' + span + '）' : '') + '</b></span>';
      }
      html += '<span class="pi-item" title="user / assistant / tool calls">メッセージ<b>' + users + ' / ' + asst + ' / ' + tools + '</b></span>';
      if (hasCost) html += '<span class="pi-item">コスト<b>' + fmtCost(cost) + '</b></span>';
      html += '<span class="pi-item" title="input / output / cacheRead / cacheWrite">トークン<b>↑' + fmtTok(tok.input) + ' ↓' + fmtTok(tok.output) +
        ' R' + fmtTok(tok.cacheRead) + ' W' + fmtTok(tok.cacheWrite) + '</b></span>';
      var ms = Object.keys(models);
      if (ms.length) html += '<span class="pi-item" title="' + esc(ms.join(', ')) + '">モデル<b>' + esc(ms[0]) + (ms.length > 1 ? ' +' + (ms.length - 1) : '') + '</b></span>';
    });
    html += '<span class="pi-grow"></span>' +
      '<button type="button" data-pi="thinking" title="thinking の全展開/全折りたたみ (T)">thinking</button>' +
      '<button type="button" data-pi="tools" title="ツール出力の全展開/全折りたたみ (O)">tools</button>';
    bar.innerHTML = html;
    content.insertBefore(bar, content.firstChild);

    var map = { thinking: 'toggle-thinking', tools: 'toggle-tools' };
    function real(b) { return document.querySelector('[data-action="' + map[b.getAttribute('data-pi')] + '"]'); }
    function sync() {
      bar.querySelectorAll('button[data-pi]').forEach(function (b) {
        var r = real(b);
        if (r) b.setAttribute('aria-pressed', r.getAttribute('aria-pressed') || 'false');
        else b.style.display = 'none';
      });
    }
    bar.addEventListener('click', function (ev) {
      var b = ev.target.closest && ev.target.closest('button[data-pi]');
      if (!b) return;
      var r = real(b);
      if (r) r.click();
      sync();
    });
    document.addEventListener('keydown', function () { setTimeout(sync, 0); });
    document.addEventListener('click', function () { setTimeout(sync, 0); });
    // thinking is expanded by default in the export; start collapsed (the export keeps this state across re-renders)
    var th = document.querySelector('[data-action="toggle-thinking"]');
    if (th && th.getAttribute('aria-pressed') === 'true') th.click();
    sync();
  });

  // ---- click a thinking block to open/close just that block ----
  safe(function () {
    var msgs = document.getElementById('messages');
    if (!msgs) return;
    msgs.addEventListener('click', function (ev) {
      var tb = ev.target.closest && ev.target.closest('.thinking-block');
      if (!tb) return;
      var sel = window.getSelection && window.getSelection();
      if (sel && String(sel).length) return;
      tb.classList.toggle('pi-open');
    });
  });

  // ---- per-assistant-message usage badge (re-applied when the export re-renders #messages) ----
  safe(function () {
    var msgs = document.getElementById('messages');
    if (!msgs || !data) return;
    var usage = {};
    data.entries.forEach(function (e) {
      if (e.type === 'message' && e.message && e.message.role === 'assistant' && e.message.usage) usage['entry-' + e.id] = e.message;
    });
    function decorate() {
      safe(function () {
        msgs.querySelectorAll('.assistant-message[id]').forEach(function (el) {
          if (el.querySelector('.pi-usage')) return;
          var m = usage[el.id];
          if (!m) return;
          var u = m.usage, c = costOf(u);
          var b = document.createElement('span');
          b.className = 'pi-usage';
          b.title = 'input ' + (u.input || 0) + ' / output ' + (u.output || 0) + ' / cacheRead ' + (u.cacheRead || 0) +
            ' / cacheWrite ' + (u.cacheWrite || 0) + (m.model ? ' · ' + m.model : '');
          b.innerHTML = (c !== null ? '<b>' + fmtCost(c) + '</b> · ' : '') + '↑' + fmtTok(u.input) + ' ↓' + fmtTok(u.output) + ' R' + fmtTok(u.cacheRead);
          var ts = el.querySelector(':scope > .message-timestamp');
          if (ts) ts.appendChild(b);
          else {
            var d = document.createElement('div');
            d.className = 'message-timestamp';
            d.appendChild(b);
            el.insertBefore(d, el.firstChild);
          }
        });
      });
    }
    decorate();
    new MutationObserver(decorate).observe(msgs, { childList: true });
  });
})();
